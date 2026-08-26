//! Группы окон: состояние набора и переключения (запрос пользователя
//! 2026-08-25).
//!
//! Модель самой группы живёт в [`rst_core::model::WindowGroup`] и попадает в
//! config.json; здесь — рантайм вокруг неё: какая группа сейчас открыта, идёт
//! ли набор новой и какие окна в него уже отмечены.
//!
//! Почему набор — отдельная машина состояний, а не пара полей в
//! координаторе. Набор группы это диалог: открыть, отметить несколько окон в
//! ленте, выбрать раскладку, подтвердить или передумать. У него есть
//! промежуточное состояние, которое нельзя потерять между кадрами, и правила
//! перехода, которые хочется проверять тестами без единого живого окна на
//! экране. В координаторе, где всё вперемешку с окклюзией и снимками окон,
//! такие правила не проверить.
//!
//! Win32 отсюда не вызывается: на вход приходят снимки окон
//! (`rst_win32::window_enum::WindowInfo`), на выход — готовая
//! [`WindowGroup`] и списки того, что надо подвинуть. Двигает окна
//! координатор.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use rst_core::group_layout;
use rst_core::group_match::{self, LiveWindow, MemberKey};
use rst_core::group_visibility::GroupVisibilityState;
use rst_core::model::{
    GroupMember, GroupPlace, MAX_GROUP_MEMBERS, MIN_GROUP_MEMBERS, MonitorId, Rect, WindowGroup,
};
use rst_win32::window_enum::WindowInfo;
use uuid::Uuid;

/// Что именно правит открытое меню редактирования групп.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorTarget {
    /// Собирается новая группа; номер ей выдадут при подтверждении.
    New,
    /// Правится существующая: подтверждение заменит её состав и раскладку,
    /// а номер, имя и идентификатор останутся прежними.
    Existing(Uuid),
}

/// Открытое меню редактирования групп.
#[derive(Debug, Clone)]
pub struct GroupEditor {
    /// Монитор, на котором меню показано. Тот, где был курсор в момент
    /// нажатия хоткея: раскладка тайлинга считается по одному экрану, и
    /// выбрать его надо в момент открытия, а не в момент подтверждения —
    /// иначе курсор успеет уехать и группа ляжет не туда.
    pub monitor: MonitorId,
    /// Отмеченные окна в порядке набора. Порядок — это номера слотов
    /// раскладки: первое отмеченное попадёт в слот 1.
    picked: Vec<usize>,
    /// Выбранная раскладка — индекс в `group_layout::presets_for(n)`.
    /// `None` — раскладка не выбрана, окна останутся там, где стоят.
    ///
    /// Сбрасывается при изменении состава: число окон поменялось, значит
    /// поменялся и набор раскладок, и старый индекс указывал бы в другую
    /// таблицу.
    preset: Option<usize>,
    /// Зазор этой группы, проценты. Независим от зазора снап-зон в трее —
    /// см. [`WindowGroup::gap_pct`].
    pub gap_pct: u8,
    /// Ручная расстановка: `слот (с нуля) → hwnd`. Заполняется, когда
    /// пользователь перетаскивает окна из ленты прямо в слоты раскладки.
    ///
    /// Отдельно от `picked`, а не вместо него: перетащить можно часть окон,
    /// а остальные обязаны разложиться по порядку набора. Слот, которого
    /// здесь нет, достаётся следующему неразложенному окну.
    manual: HashMap<usize, usize>,
    target: EditorTarget,
}

/// Рантайм-состояние групп.
#[derive(Debug, Default)]
pub struct GroupsState {
    /// Группа, открытая последней через `Ctrl+Shift+<номер>`, вместе с
    /// найденными для неё окнами (см. [`OpenGroup`]).
    ///
    /// Нужна для двух вещей: `Ctrl+Alt+Shift+G` удаляет именно её (решение
    /// пользователя), и в неё же пишутся новые места окон, пока она открыта.
    active: Option<OpenGroup>,
    editor: Option<GroupEditor>,
    /// Видимость каждой группы, по её идентификатору.
    ///
    /// Хранится ПО ГРУППАМ, а не одним полем на открытую: пользователь
    /// переключается между группами хоткеями `Ctrl+Shift+<номер>`, и если бы
    /// состояние было общим, показ второй группы объявлял бы первую
    /// спрятанной, хотя её окна остались на экране. Следующее нажатие по
    /// первой тогда прятало бы уже спрятанное — переключатель врал бы через
    /// раз.
    ///
    /// Записи для группы, которую ни разу не открывали, нет: она спрятана
    /// по умолчанию ([`GroupVisibilityState::default`]).
    visibility: HashMap<Uuid, GroupVisibilityState>,
    /// Когда группу показали в последний раз.
    ///
    /// Нужно ради короткой паузы, в которую смена переднего плана НЕ прячет
    /// группу. Показ группы — это залп из нескольких чужих окон: каждое
    /// разворачивается, поднимается и на мгновение может перехватить фокус,
    /// и пока залп не улёгся, «кто сейчас впереди» — вопрос без честного
    /// ответа. Без паузы группа успевала спрятаться через полсекунды после
    /// того, как её показали (замер на живом приложении 2026-08-26).
    shown_at: Option<Instant>,
}

impl GroupEditor {
    /// Отмеченные окна в порядке набора.
    pub fn picked(&self) -> &[usize] {
        &self.picked
    }

    /// Номер слота (с единицы) для окна, или `None`, если оно не отмечено.
    ///
    /// Именно это число рисуется на карточке окна в ленте: пользователь
    /// должен видеть порядок, потому что порядок и решает, какое окно
    /// станет главным в раскладке.
    pub fn slot_of(&self, hwnd: usize) -> Option<usize> {
        self.picked.iter().position(|h| *h == hwnd).map(|i| i + 1)
    }

    /// Выбранная раскладка.
    pub fn preset(&self) -> Option<usize> {
        self.preset
    }

    /// Отметить окно или снять отметку. Возвращает `true`, если состав
    /// изменился.
    ///
    /// Отказ при переполнении молчаливый (`false`): в группе не может быть
    /// больше [`MAX_GROUP_MEMBERS`] окон, потому что на большее число нет
    /// раскладок. Ругаться на девятый клик нечем — карточка просто не
    /// отметится.
    pub fn toggle_pick(&mut self, hwnd: usize) -> bool {
        if let Some(i) = self.picked.iter().position(|h| *h == hwnd) {
            self.picked.remove(i);
            self.forget_layout_choice(hwnd);
            return true;
        }
        if self.picked.len() >= MAX_GROUP_MEMBERS {
            return false;
        }
        self.picked.push(hwnd);
        // Состав изменился — число окон другое, а с ним и таблица раскладок.
        self.preset = None;
        true
    }

    /// Выбрать раскладку по индексу в таблице для текущего числа окон.
    ///
    /// Индекс не проверяется на попадание в таблицу: таблица живёт в
    /// `rst_core::group_layout`, а редактор не должен знать её размер, чтобы
    /// не разъехаться с ней при первой же правке. Проверяет тот, кто
    /// применяет.
    pub fn set_preset(&mut self, index: usize) {
        self.preset = Some(index);
    }

    /// Положить окно в конкретный слот раскладки (перетаскивание мышью).
    ///
    /// Окно, ещё не отмеченное в ленте, отмечается заодно: перетащить его в
    /// слот — это и есть «беру его в группу», требовать дополнительный клик
    /// значило бы наказывать за более точное действие.
    ///
    /// Возвращает `false`, если окно взять уже нельзя (группа полна).
    pub fn assign_slot(&mut self, slot: usize, hwnd: usize) -> bool {
        if !self.picked.contains(&hwnd) {
            let preset = self.preset;
            if !self.toggle_pick(hwnd) {
                return false;
            }
            // `toggle_pick` сбрасывает раскладку при смене состава, но здесь
            // пользователь тащит окно ИМЕННО В ЭТУ раскладку — забыть её
            // означало бы отменить его же действие.
            self.preset = preset;
        }
        // Слот занят другим окном — прежний жилец возвращается в общую
        // очередь, а не исчезает: он всё ещё в группе.
        self.manual.retain(|_, h| *h != hwnd);
        self.manual.insert(slot, hwnd);
        true
    }

    /// Убрать окно из ручной расстановки и, если оно было отмечено, из
    /// состава.
    fn forget_layout_choice(&mut self, hwnd: usize) {
        self.manual.retain(|_, h| *h != hwnd);
        self.preset = None;
    }

    /// Окна по слотам раскладки: `результат[i]` — окно слота `i + 1`.
    ///
    /// Сначала расставляются те, кого перетащили руками, затем оставшиеся
    /// окна в порядке набора занимают свободные слоты сверху вниз. Это и
    /// есть обещанное пользователю правило «порядок набора решает, кто
    /// станет главным», с ручной расстановкой как уточнением.
    pub fn slot_assignment(&self) -> Vec<usize> {
        let n = self.picked.len();
        let mut out: Vec<Option<usize>> = vec![None; n];
        for (slot, hwnd) in &self.manual {
            if *slot < n && self.picked.contains(hwnd) {
                out[*slot] = Some(*hwnd);
            }
        }
        // Оставшиеся считаем ДО заполнения дыр: ленивый итератор смотрел бы
        // в `out`, который тут же и меняется, и первое же вписанное окно
        // выпало бы из своей же очереди.
        let rest: Vec<usize> = self
            .picked
            .iter()
            .filter(|h| !out.iter().any(|s| s.as_ref() == Some(*h)))
            .copied()
            .collect();
        let mut rest = rest.into_iter();
        for slot in out.iter_mut() {
            if slot.is_none() {
                *slot = rest.next();
            }
        }
        out.into_iter().flatten().collect()
    }

    /// Набрано достаточно окон, чтобы группа имела смысл.
    pub fn can_confirm(&self) -> bool {
        self.picked.len() >= MIN_GROUP_MEMBERS
    }
}

impl GroupsState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Открытое меню редактирования, если оно открыто.
    pub fn editor(&self) -> Option<&GroupEditor> {
        self.editor.as_ref()
    }

    pub fn editor_mut(&mut self) -> Option<&mut GroupEditor> {
        self.editor.as_mut()
    }

    /// Идентификатор открытой группы.
    pub fn active(&self) -> Option<Uuid> {
        self.active.as_ref().map(|g| g.id)
    }

    /// Нажали хоткей меню редактирования групп.
    ///
    /// Тот же хоткей и открывает меню, и подтверждает набор (требование
    /// пользователя: «нажимаешь галочку или Alt+Shift+G»). Поэтому здесь
    /// только открытие; подтверждение — [`GroupsState::confirm`], и решает,
    /// что именно вызвать, координатор по наличию открытого меню.
    pub fn open_editor(&mut self, monitor: MonitorId, gap_pct: u8) {
        self.editor = Some(GroupEditor {
            monitor,
            picked: Vec::new(),
            preset: None,
            gap_pct,
            manual: HashMap::new(),
            target: EditorTarget::New,
        });
    }

    /// Открыть меню на существующей группе — правка состава из менеджера.
    pub fn open_editor_for(&mut self, group: &WindowGroup, monitor: MonitorId, picked: Vec<usize>) {
        self.editor = Some(GroupEditor {
            monitor,
            picked,
            preset: None,
            gap_pct: group.gap_pct,
            manual: HashMap::new(),
            target: EditorTarget::Existing(group.id),
        });
    }

    /// Закрыть меню, ничего не создавая.
    pub fn close_editor(&mut self) {
        self.editor = None;
    }

    /// Подтвердить набор: собрать группу из отмеченных окон.
    ///
    /// `windows` — текущий снимок окон, откуда берутся приметы (exe,
    /// заголовок, класс) для опознания окна после перезапуска.
    /// `existing` — уже существующие группы: нужны, чтобы выдать новой
    /// свободный номер.
    ///
    /// `None` — подтверждать нечего: окон меньше двух, или все отмеченные
    /// окна успели закрыться, пока меню было открыто. Меню при этом НЕ
    /// закрывается: пользователь остаётся там же и видит, что произошло.
    ///
    /// Места (`GroupMember::place`) здесь не заполняются: где окна встанут,
    /// решает раскладка, а применяет её координатор — он же и запишет
    /// фактическую геометрию, когда окна на неё встанут. Записывать сюда
    /// желаемое значило бы сохранить место, которого окно может и не занять
    /// (чужие права, собственный минимальный размер).
    pub fn confirm(
        &mut self,
        windows: &[WindowInfo],
        existing: &[WindowGroup],
    ) -> Option<WindowGroup> {
        let editor = self.editor.as_ref()?;
        if !editor.can_confirm() {
            return None;
        }
        let by_hwnd: HashMap<usize, &WindowInfo> = windows.iter().map(|w| (w.hwnd, w)).collect();
        let members: Vec<GroupMember> = editor
            .slot_assignment()
            .into_iter()
            .filter_map(|hwnd| by_hwnd.get(&hwnd).copied())
            .map(|w| GroupMember {
                exe_path: w.exe_path.clone(),
                title: w.title.clone(),
                class: w.class.clone(),
                place: None,
            })
            .collect();
        if members.len() < MIN_GROUP_MEMBERS {
            return None;
        }

        let group = match editor.target {
            EditorTarget::Existing(id) => {
                let old = existing.iter().find(|g| g.id == id)?;
                WindowGroup {
                    id,
                    number: old.number,
                    name: old.name.clone(),
                    members,
                    gap_pct: editor.gap_pct,
                }
            }
            EditorTarget::New => {
                let number = WindowGroup::next_number(existing)?;
                WindowGroup {
                    id: Uuid::new_v4(),
                    number,
                    name: number.to_string(),
                    members,
                    gap_pct: editor.gap_pct,
                }
            }
        };
        self.editor = None;
        Some(group)
    }
}

impl GroupEditor {
    /// Куда встанут окна, если применить выбранную раскладку.
    ///
    /// `work_area` — рабочая область монитора в физических пикселях. На
    /// выходе пары «окно и его прямоугольник», в порядке слотов: слот 1
    /// первым. `None` — раскладка не выбрана, и двигать окна не за чем;
    /// группа тогда сохранит их там, где они стоят.
    ///
    /// Зазор задан в процентах, а [`group_layout::apply`] принимает пиксели,
    /// и перевод между ними — решение, которое стоит объяснить. Проценты
    /// считаются от МЕНЬШЕЙ СТОРОНЫ САМОГО МАЛЕНЬКОГО слота этой раскладки.
    /// Не от экрана: 5% высоты FullHD — это 54 пикселя, и на раскладке из
    /// восьми окон такой зазор съел бы сами окна. Не от каждого слота
    /// по-своему: у соседей общая граница, и два разных зазора по её
    /// сторонам разъехались бы. Привязка к самому маленькому слоту даёт
    /// один зазор на всю раскладку и гарантирует, что он останется малой
    /// долей даже самого тесного окна.
    pub fn layout_targets(&self, work_area: Rect) -> Option<Vec<(usize, Rect)>> {
        let windows = self.slot_assignment();
        let preset = group_layout::presets_for(windows.len()).get(self.preset?)?;
        // Первый проход без зазора — только чтобы узнать размер самого
        // тесного слота. Считать его по долям вручную значило бы повторить
        // здесь округление из `apply` и рано или поздно с ним разойтись.
        let bare = group_layout::apply(preset, work_area, 0);
        let smallest = bare.iter().map(|r| r.w.min(r.h)).min().unwrap_or(0);
        let gap = (u32::from(self.gap_pct) * smallest / 100) as i32;
        let rects = group_layout::apply(preset, work_area, gap);
        Some(windows.into_iter().zip(rects).collect())
    }
}

/// Сколько показ группы считается «ещё не улёгшимся».
///
/// Замер 2026-08-26: после показа группы из четырёх окон передний план
/// возвращался к постороннему окну через 400–900 мс. Порог взят с запасом
/// над верхней границей замера. Больше секунды брать нельзя: столько уже
/// хватает, чтобы человек осознанно переключился на другое окно, а его выбор
/// оспаривать не нужно.
const GROUP_SHOW_SETTLE: Duration = Duration::from_millis(1200);

/// Насколько окно должно разойтись с запомненным местом, чтобы это считалось
/// перемещением, DIP.
///
/// Не ноль: окно возвращает свои границы с округлением до физических
/// пикселей, и на дробном масштабе (125%, 150%) обратный перевод в DIP
/// почти никогда не даёт исходное число в точности. Порог ниже единицы
/// заставлял бы переписывать config.json на каждом снимке окон при полностью
/// неподвижном экране.
const PLACE_EPS_DIP: f64 = 1.5;

/// Открытая группа: соответствие членов живым окнам, установленное В МОМЕНТ
/// ОТКРЫТИЯ и дальше не пересчитываемое.
///
/// Почему не искать окна заново на каждом снимке. Опознание
/// ([`rst_core::group_match`]) стоит недёшево и, главное, неустойчиво:
/// заголовок окна меняется на лету, и повторное сопоставление могло бы
/// внезапно решить, что место члена 1 теперь занимает окно члена 3, — и
/// записать в группу чужую геометрию. Один раз найдено — дальше держимся
/// за `HWND`, а он живёт ровно столько, сколько живёт окно.
#[derive(Debug, Clone)]
pub struct OpenGroup {
    pub id: Uuid,
    /// `HWND` для каждого члена по его индексу. `None` — окно не нашлось
    /// (приложение не запущено); место такого члена остаётся нетронутым и
    /// дождётся, когда окно вернётся.
    windows: Vec<Option<usize>>,
}

impl OpenGroup {
    /// Все найденные окна группы в порядке членов.
    ///
    /// Только для тестов: в рабочем коде окна берутся по индексу члена
    /// ([`Self::window_of`]), потому что решения машины видимости и
    /// сохранённые места привязаны именно к номеру члена, а не к позиции в
    /// списке найденных — иначе одно закрытое приложение сдвигало бы всё
    /// остальное на слот вперёд.
    #[cfg(test)]
    pub fn windows(&self) -> Vec<usize> {
        self.windows.iter().flatten().copied().collect()
    }

    /// Окно члена группы по его индексу.
    pub fn window_of(&self, member: usize) -> Option<usize> {
        self.windows.get(member).copied().flatten()
    }
}

/// Куда поставить одно окно при открытии группы.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupPlacement {
    pub hwnd: usize,
    pub place: GroupPlace,
}

impl GroupsState {
    /// Открыть группу: опознать её окна среди живых и сказать, кого куда
    /// поставить.
    ///
    /// Возвращает места только для тех членов, чьё окно нашлось И чьё место
    /// вообще запоминалось. Член без сохранённого места (группу собрали, но
    /// раскладку не применяли) не двигается: у нас нет мнения о том, где ему
    /// стоять, а двигать окно в произвольную точку хуже, чем не трогать.
    ///
    /// Окна, которых не нашлось, молча пропускаются — приложение просто не
    /// запущено. Это штатная ситуация, а не ошибка: пользователь выбрал
    /// «ждать окно и ловить по приложению и заголовку».
    pub fn open_group(&mut self, group: &WindowGroup, live: &[LiveWindow]) -> Vec<GroupPlacement> {
        let keys: Vec<MemberKey> = group
            .members
            .iter()
            .map(|m| MemberKey {
                exe_path: m.exe_path.clone(),
                title: m.title.clone(),
                class: m.class.clone(),
            })
            .collect();
        let matched = group_match::match_members(&keys, live);
        let windows: Vec<Option<usize>> = matched
            .iter()
            .map(|idx| idx.map(|i| live[i].hwnd))
            .collect();

        let places = group
            .members
            .iter()
            .zip(&windows)
            .filter_map(|(member, hwnd)| {
                Some(GroupPlacement {
                    hwnd: (*hwnd)?,
                    place: member.place.clone()?,
                })
            })
            .collect();

        self.active = Some(OpenGroup {
            id: group.id,
            windows,
        });
        places
    }

    /// Группа больше не открыта (её удалили или закрыли последнее окно).
    pub fn close_group(&mut self) {
        self.active = None;
    }

    /// Видимость группы: показана ли она и закреплена ли поверх всех.
    ///
    /// Группа, которую ещё не открывали, спрятана — так и должно быть после
    /// перезапуска программы: притворяться показанной, не подняв ни одного
    /// окна, значит соврать первому же нажатию хоткея.
    pub fn visibility(&self, group: Uuid) -> GroupVisibilityState {
        self.visibility.get(&group).copied().unwrap_or_default()
    }

    /// Запомнить новую видимость группы после решения машины.
    pub fn set_visibility(&mut self, group: Uuid, state: GroupVisibilityState) {
        let was_shown = self.visibility(group).shown;
        if state.shown && !was_shown {
            self.shown_at = Some(Instant::now());
        }
        self.visibility.insert(group, state);
    }

    /// Группу показали только что, и залп её окон ещё не улёгся.
    ///
    /// Пока это так, смена переднего плана не считается уходом пользователя
    /// на постороннее окно: передний план в эти доли секунды перебрасывают
    /// сами показываемые окна.
    pub fn just_shown(&self) -> bool {
        self.shown_at
            .is_some_and(|at| at.elapsed() < GROUP_SHOW_SETTLE)
    }

    /// Забыть видимость удалённой группы, чтобы новая группа с тем же
    /// идентификатором (после импорта конфига) не унаследовала чужое
    /// состояние.
    pub fn forget_visibility(&mut self, group: Uuid) {
        self.visibility.remove(&group);
    }

    /// Есть ли сейчас группа, ПОКАЗАННАЯ на экране.
    ///
    /// Отдельно от [`Self::open`]: открытая, но спрятанная группа остаётся
    /// «последней открытой» (её удаляет `Ctrl+Alt+Shift+G` и закрепляет
    /// `Ctrl+Alt+Shift+T`), но обслуживать на каждом снимке её не нужно —
    /// прятать уже спрятанное не от чего. По этому признаку решается, держать
    /// ли разбуженным трекер окон.
    pub fn shown_group(&self) -> Option<Uuid> {
        let id = self.active()?;
        self.visibility(id).shown.then_some(id)
    }

    /// Открытая группа.
    pub fn open(&self) -> Option<&OpenGroup> {
        self.active.as_ref()
    }

    /// Запомнить новые места окон открытой группы.
    ///
    /// Пользователь выбрал «запоминать постоянно, пока группа открыта»:
    /// подвинул окно — через мгновение это уже записано, ничего нажимать не
    /// надо. Отсюда и порог [`PLACE_EPS_DIP`]: без него дрожание округления
    /// заставляло бы переписывать config.json по нескольку раз в секунду на
    /// полностью неподвижном экране.
    ///
    /// `live_places` — где окна находятся сейчас, в DIP своего монитора
    /// (перевод из физических пикселей — забота координатора, здесь Win32
    /// нет). Окно, которого в этой карте нет, считается исчезнувшим с
    /// экрана: его место НЕ трогаем, иначе свёрнутое окно записало бы в
    /// группу свою мусорную геометрию.
    ///
    /// Возвращает `true`, если хоть одно место изменилось, — это сигнал
    /// координатору сохранить конфиг.
    pub fn note_places(
        &self,
        group: &mut WindowGroup,
        live_places: &HashMap<usize, GroupPlace>,
    ) -> bool {
        let Some(open) = &self.active else {
            return false;
        };
        if open.id != group.id {
            return false;
        }
        let mut changed = false;
        for (i, member) in group.members.iter_mut().enumerate() {
            let Some(hwnd) = open.window_of(i) else {
                continue;
            };
            let Some(now) = live_places.get(&hwnd) else {
                continue;
            };
            if member.place.as_ref().is_some_and(|p| same_place(p, now)) {
                continue;
            }
            member.place = Some(now.clone());
            changed = true;
        }
        changed
    }
}

/// Места совпадают с точностью до [`PLACE_EPS_DIP`] и стоят на одном мониторе.
fn same_place(a: &GroupPlace, b: &GroupPlace) -> bool {
    a.monitor_id == b.monitor_id
        && (a.x - b.x).abs() <= PLACE_EPS_DIP
        && (a.y - b.y).abs() <= PLACE_EPS_DIP
        && (a.w - b.w).abs() <= PLACE_EPS_DIP
        && (a.h - b.h).abs() <= PLACE_EPS_DIP
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn monitor() -> MonitorId {
        MonitorId("mon".to_string())
    }

    fn win(hwnd: usize, title: &str) -> WindowInfo {
        WindowInfo {
            hwnd,
            title: title.to_string(),
            class: "Class".to_string(),
            exe_path: PathBuf::from("app.exe"),
            ..Default::default()
        }
    }

    fn snapshot() -> Vec<WindowInfo> {
        (1..=9).map(|i| win(i, &format!("окно {i}"))).collect()
    }

    fn editor_with(picked: &[usize]) -> GroupsState {
        let mut st = GroupsState::new();
        st.open_editor(monitor(), 5);
        for h in picked {
            st.editor_mut().expect("меню открыто").toggle_pick(*h);
        }
        st
    }

    fn place(x: f64, y: f64) -> GroupPlace {
        GroupPlace {
            monitor_id: monitor(),
            x,
            y,
            w: 800.0,
            h: 600.0,
        }
    }

    fn member(title: &str, place: Option<GroupPlace>) -> GroupMember {
        GroupMember {
            exe_path: PathBuf::from("app.exe"),
            title: title.to_string(),
            class: "Class".to_string(),
            place,
        }
    }

    fn live(hwnd: usize, title: &str) -> LiveWindow {
        LiveWindow {
            hwnd,
            exe_path: PathBuf::from("app.exe"),
            title: title.to_string(),
            class: "Class".to_string(),
        }
    }

    fn saved_group(members: Vec<GroupMember>) -> WindowGroup {
        WindowGroup {
            id: Uuid::new_v4(),
            number: 1,
            name: "1".to_string(),
            members,
            gap_pct: 0,
        }
    }

    #[test]
    fn opening_a_group_places_every_window_that_was_found() {
        let g = saved_group(vec![
            member("редактор", Some(place(0.0, 0.0))),
            member("браузер", Some(place(900.0, 0.0))),
        ]);
        let mut st = GroupsState::new();
        let placements = st.open_group(&g, &[live(11, "браузер"), live(22, "редактор")]);
        assert_eq!(placements.len(), 2);
        // Порядок результата — порядок членов группы, а не порядок окон в
        // снимке: слоты нумеруются по членам.
        assert_eq!(placements[0].hwnd, 22);
        assert_eq!(placements[1].hwnd, 11);
        assert_eq!(st.active(), Some(g.id));
    }

    #[test]
    fn a_member_whose_app_is_not_running_is_simply_skipped() {
        // Пользователь выбрал «ждать окно»: не найденное приложение — это не
        // ошибка и не повод разваливать группу.
        let g = saved_group(vec![
            member("редактор", Some(place(0.0, 0.0))),
            member("почта", Some(place(900.0, 0.0))),
        ]);
        let mut st = GroupsState::new();
        let placements = st.open_group(&g, &[live(22, "редактор")]);
        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].hwnd, 22);
        assert_eq!(
            st.open().expect("группа открыта").window_of(1),
            None,
            "пропавший член обязан остаться без окна, а не украсть чужое"
        );
    }

    #[test]
    fn a_member_without_a_saved_place_is_not_moved() {
        // Группу собрали, но раскладку не применяли: мнения о том, где стоять
        // этому окну, у нас нет, и двигать его в произвольную точку хуже, чем
        // не трогать.
        let g = saved_group(vec![
            member("редактор", Some(place(0.0, 0.0))),
            member("браузер", None),
        ]);
        let mut st = GroupsState::new();
        let placements = st.open_group(&g, &[live(11, "браузер"), live(22, "редактор")]);
        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].hwnd, 22);
        assert_eq!(
            st.open().expect("группа открыта").windows().len(),
            2,
            "окно найдено и запомнено, даже если двигать его некуда"
        );
    }

    #[test]
    fn moving_a_window_inside_an_open_group_is_remembered() {
        let mut g = saved_group(vec![
            member("редактор", Some(place(0.0, 0.0))),
            member("браузер", Some(place(900.0, 0.0))),
        ]);
        let mut st = GroupsState::new();
        st.open_group(&g, &[live(11, "браузер"), live(22, "редактор")]);

        let mut now = HashMap::new();
        now.insert(22, place(0.0, 0.0));
        now.insert(11, place(950.0, 40.0)); // пользователь подвинул браузер
        assert!(
            st.note_places(&mut g, &now),
            "сдвиг обязан попасть в группу"
        );
        assert_eq!(g.members[1].place.as_ref().expect("место").x, 950.0);
        assert_eq!(g.members[0].place.as_ref().expect("место").x, 0.0);
    }

    #[test]
    fn rounding_jitter_does_not_rewrite_the_config() {
        // Границы окна возвращаются с округлением до физических пикселей, и
        // на дробном масштабе обратный перевод в DIP почти никогда не даёт
        // исходное число. Без порога это переписывало бы config.json на
        // каждом снимке окон при неподвижном экране.
        let mut g = saved_group(vec![
            member("редактор", Some(place(100.0, 100.0))),
            member("браузер", Some(place(900.0, 0.0))),
        ]);
        let mut st = GroupsState::new();
        st.open_group(&g, &[live(11, "браузер"), live(22, "редактор")]);

        let mut now = HashMap::new();
        now.insert(22, place(100.4, 99.6));
        now.insert(11, place(900.0, 0.0));
        assert!(
            !st.note_places(&mut g, &now),
            "дрожание округления — не движение"
        );
    }

    #[test]
    fn a_window_missing_from_the_snapshot_keeps_its_saved_place() {
        // Свёрнутое окно отдаёт мусорную геометрию. Записать её значило бы
        // при следующем открытии группы поставить окно неизвестно куда.
        let mut g = saved_group(vec![
            member("редактор", Some(place(100.0, 100.0))),
            member("браузер", Some(place(900.0, 0.0))),
        ]);
        let mut st = GroupsState::new();
        st.open_group(&g, &[live(11, "браузер"), live(22, "редактор")]);

        let now = HashMap::new(); // ни одного окна на экране
        assert!(!st.note_places(&mut g, &now));
        assert_eq!(g.members[0].place.as_ref().expect("место").x, 100.0);
    }

    #[test]
    fn places_are_not_written_into_a_group_that_is_not_the_open_one() {
        // Защита от того, чтобы геометрия одной группы уехала в другую:
        // окно может состоять сразу в нескольких группах, и у каждой своё
        // место для него.
        let mut other = saved_group(vec![
            member("редактор", Some(place(0.0, 0.0))),
            member("браузер", Some(place(900.0, 0.0))),
        ]);
        let opened = saved_group(vec![
            member("редактор", Some(place(0.0, 0.0))),
            member("браузер", Some(place(900.0, 0.0))),
        ]);
        let mut st = GroupsState::new();
        st.open_group(&opened, &[live(11, "браузер"), live(22, "редактор")]);

        let mut now = HashMap::new();
        now.insert(22, place(500.0, 500.0));
        assert!(!st.note_places(&mut other, &now));
        assert_eq!(other.members[0].place.as_ref().expect("место").x, 0.0);
    }

    #[test]
    fn note_places_does_nothing_when_no_group_is_open() {
        let mut g = saved_group(vec![
            member("редактор", Some(place(0.0, 0.0))),
            member("браузер", Some(place(900.0, 0.0))),
        ]);
        let st = GroupsState::new();
        let mut now = HashMap::new();
        now.insert(22, place(500.0, 500.0));
        assert!(!st.note_places(&mut g, &now));
    }

    #[test]
    fn a_window_that_changed_monitor_is_remembered_on_the_new_one() {
        let mut g = saved_group(vec![
            member("редактор", Some(place(0.0, 0.0))),
            member("браузер", Some(place(900.0, 0.0))),
        ]);
        let mut st = GroupsState::new();
        st.open_group(&g, &[live(11, "браузер"), live(22, "редактор")]);

        let mut moved = place(0.0, 0.0);
        moved.monitor_id = MonitorId("второй".to_string());
        let mut now = HashMap::new();
        now.insert(22, moved);
        now.insert(11, place(900.0, 0.0));
        assert!(
            st.note_places(&mut g, &now),
            "смена монитора — это перемещение"
        );
        assert_eq!(
            g.members[0].place.as_ref().expect("место").monitor_id,
            MonitorId("второй".to_string()),
            "группа может занимать оба монитора — это выбор пользователя"
        );
    }

    fn work() -> Rect {
        Rect {
            x: 0,
            y: 0,
            w: 2560,
            h: 1400,
        }
    }

    #[test]
    fn without_a_chosen_layout_windows_are_not_moved() {
        // Группу собрали, раскладку не выбрали: мнения о том, где окнам
        // стоять, у нас нет, и двигать их куда попало хуже, чем не трогать.
        let st = editor_with(&[1, 2, 3]);
        assert!(st.editor().expect("меню").layout_targets(work()).is_none());
    }

    #[test]
    fn the_first_picked_window_gets_slot_one() {
        // Обещание пользователю: порядок набора решает, кто станет главным.
        // Проверяем на раскладке, где слот 1 заведомо больше остальных.
        let mut st = editor_with(&[7, 3, 5]);
        let ed = st.editor_mut().expect("меню");
        let biggest = biggest_slot_preset(3);
        ed.set_preset(biggest);
        let targets = ed.layout_targets(work()).expect("раскладка выбрана");
        assert_eq!(targets[0].0, 7, "первое отмеченное окно идёт в слот 1");
        let area = |r: &Rect| u64::from(r.w) * u64::from(r.h);
        assert!(
            targets[1..]
                .iter()
                .all(|(_, r)| area(r) < area(&targets[0].1)),
            "в этой раскладке слот 1 обязан быть самым большим"
        );
    }

    /// Индекс раскладки для `n` окон, где слот 1 строго больше остальных.
    fn biggest_slot_preset(n: usize) -> usize {
        rst_core::group_layout::presets_for(n)
            .iter()
            .position(|p| {
                let first = p.slots[0].w * p.slots[0].h;
                p.slots[1..].iter().all(|s| s.w * s.h < first - 1e-9)
            })
            .expect("хотя бы одна раскладка вида главное плюс стопка")
    }

    #[test]
    fn a_manual_drop_overrides_the_picking_order() {
        let mut st = editor_with(&[1, 2, 3]);
        let ed = st.editor_mut().expect("меню");
        ed.set_preset(biggest_slot_preset(3));
        // Третье окно перетащили в главный слот.
        ed.assign_slot(0, 3);
        let targets = ed.layout_targets(work()).expect("раскладка выбрана");
        assert_eq!(targets[0].0, 3);
    }

    #[test]
    fn a_bigger_gap_makes_every_window_smaller() {
        let mut st = editor_with(&[1, 2]);
        let ed = st.editor_mut().expect("меню");
        ed.set_preset(0);
        ed.gap_pct = 0;
        let tight = ed.layout_targets(work()).expect("раскладка");
        ed.gap_pct = 20;
        let loose = ed.layout_targets(work()).expect("раскладка");
        for (a, b) in tight.iter().zip(&loose) {
            assert!(
                b.1.w <= a.1.w && b.1.h <= a.1.h,
                "с зазором окно обязано стать не больше: было {}x{}, стало {}x{}",
                a.1.w,
                a.1.h,
                b.1.w,
                b.1.h
            );
        }
        assert!(
            loose[0].1.w < tight[0].1.w,
            "зазор в 20% обязан быть заметен, а не съесться округлением"
        );
    }

    #[test]
    fn windows_never_leave_the_work_area() {
        // Проверяем все раскладки на всех размерах группы: слот, вылезший за
        // рабочую область, увёл бы окно под панель задач или на соседний
        // монитор.
        let w = work();
        for n in MIN_GROUP_MEMBERS..=MAX_GROUP_MEMBERS {
            let picked: Vec<usize> = (1..=n).collect();
            let mut st = editor_with(&picked);
            let count = rst_core::group_layout::presets_for(n).len();
            for i in 0..count {
                let ed = st.editor_mut().expect("меню");
                ed.set_preset(i);
                ed.gap_pct = 10;
                for (_, r) in ed.layout_targets(w).expect("раскладка") {
                    assert!(
                        r.x >= w.x
                            && r.y >= w.y
                            && r.x + r.w as i32 <= w.x + w.w as i32
                            && r.y + r.h as i32 <= w.y + w.h as i32,
                        "раскладка {i} на {n} окон вывела слот за рабочую область: {r:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_degenerate_work_area_does_not_panic() {
        // Гонка переподключения монитора: рабочая область может прийти
        // нулевой, и падать из-за этого нельзя.
        let mut st = editor_with(&[1, 2]);
        let ed = st.editor_mut().expect("меню");
        ed.set_preset(0);
        let targets = ed
            .layout_targets(Rect {
                x: 0,
                y: 0,
                w: 0,
                h: 0,
            })
            .expect("раскладка выбрана");
        assert_eq!(targets.len(), 2);
    }

    #[test]
    fn picking_order_becomes_slot_numbers() {
        // Порядок набора — это и есть обещанное пользователю правило:
        // первое отмеченное окно попадает в слот 1, то есть в главное окно
        // раскладок вида «главное плюс стопка».
        let st = editor_with(&[7, 3, 5]);
        let ed = st.editor().expect("меню открыто");
        assert_eq!(ed.slot_of(7), Some(1));
        assert_eq!(ed.slot_of(3), Some(2));
        assert_eq!(ed.slot_of(5), Some(3));
        assert_eq!(ed.slot_of(9), None, "неотмеченное окно слота не имеет");
    }

    #[test]
    fn unpicking_shifts_the_rest_up() {
        let mut st = editor_with(&[1, 2, 3]);
        let ed = st.editor_mut().expect("меню открыто");
        assert!(ed.toggle_pick(1), "повторный клик снимает отметку");
        assert_eq!(ed.slot_of(2), Some(1), "второе окно обязано стать первым");
        assert_eq!(ed.slot_of(3), Some(2));
    }

    #[test]
    fn a_group_cannot_take_more_than_eight_windows() {
        let mut st = editor_with(&[1, 2, 3, 4, 5, 6, 7, 8]);
        let ed = st.editor_mut().expect("меню открыто");
        assert!(
            !ed.toggle_pick(9),
            "девятое окно брать некуда — нет раскладки"
        );
        assert_eq!(ed.picked().len(), MAX_GROUP_MEMBERS);
    }

    #[test]
    fn changing_the_line_up_forgets_the_chosen_layout() {
        // Раскладки заведены по числу окон; взяв ещё одно окно, пользователь
        // смотрит уже в другую таблицу, и прежний индекс указывал бы в неё
        // наугад.
        let mut st = editor_with(&[1, 2]);
        let ed = st.editor_mut().expect("меню открыто");
        ed.set_preset(3);
        assert_eq!(ed.preset(), Some(3));
        ed.toggle_pick(4);
        assert_eq!(ed.preset(), None);
    }

    #[test]
    fn dragging_a_window_into_a_slot_keeps_the_chosen_layout() {
        // Перетаскивание — это уточнение ВНУТРИ выбранной раскладки. Если бы
        // оно сбрасывало выбор, пользователь отменял бы раскладку тем самым
        // действием, которым её заполняет.
        let mut st = editor_with(&[1, 2]);
        let ed = st.editor_mut().expect("меню открыто");
        ed.set_preset(2);
        assert!(ed.assign_slot(2, 5), "новое окно берётся в группу заодно");
        assert_eq!(ed.preset(), Some(2));
        assert!(ed.picked().contains(&5));
    }

    #[test]
    fn manual_slots_win_and_the_rest_fill_the_gaps_in_order() {
        let mut st = editor_with(&[1, 2, 3]);
        let ed = st.editor_mut().expect("меню открыто");
        // Третье окно перетащено в первый слот; первое и второе занимают
        // оставшиеся слоты в своём порядке.
        ed.assign_slot(0, 3);
        assert_eq!(ed.slot_assignment(), vec![3, 1, 2]);
    }

    #[test]
    fn dropping_a_window_onto_an_occupied_slot_displaces_the_previous_one() {
        let mut st = editor_with(&[1, 2]);
        let ed = st.editor_mut().expect("меню открыто");
        ed.assign_slot(0, 2);
        ed.assign_slot(0, 1);
        // Второе окно вытеснено из слота, но из ГРУППЫ не выпало — просто
        // встаёт в общую очередь.
        assert_eq!(ed.slot_assignment(), vec![1, 2]);
        assert!(ed.picked().contains(&2));
    }

    #[test]
    fn confirming_two_windows_creates_group_number_one() {
        let mut st = editor_with(&[4, 6]);
        let group = st
            .confirm(&snapshot(), &[])
            .expect("две отметки — уже группа");
        assert_eq!(group.number, 1);
        assert_eq!(group.name, "1");
        assert_eq!(group.members.len(), 2);
        assert_eq!(group.members[0].title, "окно 4", "порядок набора сохранён");
        assert!(st.editor().is_none(), "подтверждение закрывает меню");
    }

    #[test]
    fn confirming_a_single_window_is_refused_and_keeps_the_menu_open() {
        // Группа из одного окна — это просто окно. Меню при отказе обязано
        // остаться открытым, иначе пользователь теряет весь набор из-за
        // одного лишнего нажатия хоткея.
        let mut st = editor_with(&[1]);
        assert!(st.confirm(&snapshot(), &[]).is_none());
        assert!(st.editor().is_some());
    }

    #[test]
    fn windows_closed_while_the_menu_was_open_drop_out_of_the_group() {
        let mut st = editor_with(&[1, 2, 99]);
        // Окна 99 в снимке нет — оно закрылось, пока шёл набор.
        let group = st
            .confirm(&snapshot(), &[])
            .expect("двух живых окон хватает");
        assert_eq!(group.members.len(), 2);
    }

    #[test]
    fn confirming_when_every_picked_window_died_is_refused() {
        let mut st = editor_with(&[98, 99]);
        assert!(st.confirm(&snapshot(), &[]).is_none());
        assert!(
            st.editor().is_some(),
            "меню остаётся, чтобы отказ был виден"
        );
    }

    #[test]
    fn editing_an_existing_group_keeps_its_number_and_name() {
        // Правка состава не должна менять хоткей группы: пользователь
        // привык открывать её по своей цифре.
        let mut existing = WindowGroup {
            id: Uuid::new_v4(),
            number: 4,
            name: "работа".to_string(),
            members: Vec::new(),
            gap_pct: 0,
        };
        existing.gap_pct = 3;
        let mut st = GroupsState::new();
        st.open_editor_for(&existing, monitor(), vec![1, 2]);
        st.editor_mut().expect("меню").gap_pct = 12;
        let group = st
            .confirm(&snapshot(), std::slice::from_ref(&existing))
            .expect("правка существующей группы");
        assert_eq!(group.id, existing.id);
        assert_eq!(group.number, 4);
        assert_eq!(group.name, "работа");
        assert_eq!(
            group.gap_pct, 12,
            "зазор берётся из меню, а не из старой группы"
        );
    }

    #[test]
    fn tenth_group_is_refused_because_there_is_no_hotkey_for_it() {
        let existing: Vec<WindowGroup> = (1..=9)
            .map(|n| WindowGroup {
                id: Uuid::new_v4(),
                number: n,
                name: n.to_string(),
                members: Vec::new(),
                gap_pct: 0,
            })
            .collect();
        let mut st = editor_with(&[1, 2]);
        assert!(
            st.confirm(&snapshot(), &existing).is_none(),
            "цифровых хоткеев девять — десятую группу нечем открывать"
        );
    }
}
