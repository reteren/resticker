//! Панель менеджера групп окон (запрос пользователя 2026-08-25: «кнопка в
//! режиме редактирования, по которой вылезет менеджер окон» — с прямым
//! указанием на панель «Presets» как на образец).
//!
//! Показывает список групп, а по клику по строке — её состав: какие окна
//! внутри. Оттуда же группа удаляется и переименовывается.
//!
//! Родственник [`crate::preset_picker`] намеренно: та же геометрия, тот же
//! способ отдавать нажатия наружу, тот же приём с черновиком имени. Разница
//! одна — строка группы РАСКРЫВАЕТСЯ, а не применяется: применить группу
//! можно хоткеем `Ctrl+Shift+<номер>`, а вот заглянуть внутрь больше негде.
//!
//! Строитель чистый: на вход срез групп и что раскрыто, на выход [`Panel`].
//! Состояние живёт в координаторе.
//!
//! Материал — Dark Liquid Glass (`docs/DESIGN_LIQUID_GLASS.md`): корпус —
//! плита стекла (§4), строки списка — карточки, раскрытая строка — выбранная
//! (залита светом, §8.8), интерактив ведёт `Button`, который сам ужимается и
//! светит гало (§5). Старого языка оформления (объёмные рамки VGUI,
//! светло-серая палитра настроек) здесь нет — он удалён из проекта
//! целиком (§7).

use rst_core::model::WindowGroup;
use rst_render::{
    Box2D, Button, ButtonContent, LINE_HEIGHT, Panel, Primitive, Widget, WidgetId, glass,
    glass_card, glass_on, text_size, theme,
};

use crate::window_picker::truncate_to_width;

/// Идентификатор панели. Диапазон 900+: тулбар 0-8, панель у курсора 100+,
/// панель выбора окон 200+, пресеты 400+, лента окон 600+, лента раскладок
/// 800+. Цифра монитора занимает 902, поэтому начинаем с 910.
pub const PANEL_ID: WidgetId = 910;
/// Строка группы: `ROW_BASE + индекс` в срезе `groups`. Клик раскрывает или
/// сворачивает состав.
pub const ROW_BASE: WidgetId = 911;
/// Кнопка удаления группы: `DELETE_BASE + индекс`.
pub const DELETE_BASE: WidgetId = 940;
// Поля имени и кнопки переименования здесь НЕТ намеренно: группы только
// нумеруются (решение пользователя 2026-08-26 — «нахрена мне вообще функция
// названий групп»). Номер — это цифра хоткея `Ctrl+Shift+N`, им группу и
// зовут; произвольное имя добавляло бы второй способ называть то же самое.
/// Кнопка «закрыть панель».
pub const BTN_CLOSE: WidgetId = 972;
/// Кнопка «собрать новую группу» — открывает меню редактирования групп.
pub const BTN_NEW: WidgetId = 976;
/// Кнопка «править состав раскрытой группы» — открывает то же меню, но с
/// уже отмеченными окнами этой группы.
pub const BTN_EDIT: WidgetId = 977;
/// Заголовок панели.
const ID_TITLE: WidgetId = 973;
/// Подпись пустого списка.
const ID_EMPTY: WidgetId = 974;
/// Разделитель между списком и действиями.
const ID_DIVIDER: WidgetId = 975;
/// Первая строка состава раскрытой группы.
const MEMBER_BASE: WidgetId = 980;
/// Флаг id карточки строки: не пересекается с id кнопки строки
/// (`ROW_BASE + i`), который декодирует координатор (тот же приём, что
/// `LABEL_FLAG` в `window_pick_list`).
const CARD_FLAG: WidgetId = 0x8000_0000;

/// Ширина панели, DIP. Шире, чем у пресетов: строка несёт номер, имя и
/// счётчик окон, а строки состава — заголовки чужих окон, которые обрезать
/// хочется как можно позже. Токена в §3 нет — панель остаётся своей ширины.
pub const WIDTH: f64 = 380.0;
/// Высота строки состава, DIP. Токена в §3 нет: это подпись, а не кнопка,
/// и дышит она плотнее кнопочной строки.
const MEMBER_H: f64 = 20.0;
/// Отступ строк состава от левого края, DIP. Токена в §3 нет; вложенность
/// должна читаться глазом.
const MEMBER_INDENT: f64 = 18.0;

/// Групп не больше девяти (по числу цифровых хоткеев), поэтому прокрутка
/// списку не нужна — в отличие от пресетов, которых бывают десятки.
pub const MAX_ROWS: usize = 9;
/// Окон в группе не больше восьми ([`rst_core::model::MAX_GROUP_MEMBERS`]).
const MAX_MEMBERS: usize = 8;

const TITLE_LABEL: &str = "Window groups";
/// Подпись пустого списка, когда хоткей меню набора не назначен вовсе:
/// советовать нечего, кроме кнопки, которая стоит тут же.
const EMPTY_LABEL_NO_HOTKEY: &str = "No groups yet — use \"New group\" below.";
const CLOSE_LABEL: &str = "Close";
/// Слово «группа» в подписи строки — подпись должна читаться, а не считываться.
const GROUP_WORD: &str = "Group";
const WINDOW_WORD: &str = "window";
const WINDOWS_WORD: &str = "windows";
const NEW_LABEL: &str = "New group";
const EDIT_LABEL: &str = "Edit windows";
const DELETE_MARK: &str = "x";

/// Сколько строк занимает список: пустой список занимает одну строку под
/// подпись.
fn visible_rows(count: usize) -> usize {
    count.clamp(1, MAX_ROWS)
}

/// Статичная надпись с явной непрозрачностью (§2.3): белый `theme::TEXT`,
/// второстепенность задаётся токеном альфы, а не отдельным серым цветом.
/// Своя, а не `rst_render::Label`: у того `set_dim` зашит на 0.5, токенов
/// §2.3 там нет — а у этой панели заголовок и подписи должны жить на
/// `TEXT_OPACITY`/`TEXT_DIM_OPACITY`.
struct StaticText {
    id: WidgetId,
    rect: Box2D,
    text: String,
    /// Непрозрачность текста — токен §2.3.
    opacity: f64,
}

impl StaticText {
    /// Надпись с левым краем `left` и центром строки `cy` (DIP) на
    /// непрозрачности `opacity`.
    fn new(id: WidgetId, left: f64, cy: f64, text: &str, opacity: f64) -> Self {
        let (tw, th) = text_size(text);
        Self {
            id,
            rect: Box2D {
                cx: left + tw / 2.0,
                cy,
                w: tw,
                h: th,
                rotation: 0.0,
            },
            text: text.to_string(),
            opacity,
        }
    }
}

impl Widget for StaticText {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn bounds(&self) -> Box2D {
        self.rect
    }

    fn set_bounds(&mut self, bounds: Box2D) {
        self.rect = bounds;
    }

    /// Не интерактивна — клики/hover сквозь неё.
    fn hit_test(&self, _pos: (f64, f64)) -> bool {
        false
    }

    fn draw(&self, out: &mut Vec<Primitive>) {
        out.push(Primitive::Text {
            rect: self.rect,
            text: self.text.clone(),
            color: theme::TEXT,
            opacity: self.opacity,
        });
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// Разделитель-волосинка между списком и действиями: белый свет силой
/// внешней обводки (§2.1 `STROKE`) толщиной `HAIRLINE`. Свой, а не
/// `rst_render::Divider`: тот всё ещё рисует тёмный `PANEL_BORDER`, а на
/// чёрном стекле тёмная линия читалась бы грязью.
struct Hairline {
    id: WidgetId,
    rect: Box2D,
}

impl Hairline {
    /// Горизонтальная линия шириной `w` с центром в `(cx, cy)`.
    fn new(id: WidgetId, cx: f64, cy: f64, w: f64) -> Self {
        Self {
            id,
            rect: Box2D {
                cx,
                cy,
                w,
                h: theme::HAIRLINE,
                rotation: 0.0,
            },
        }
    }
}

impl Widget for Hairline {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn bounds(&self) -> Box2D {
        self.rect
    }

    fn set_bounds(&mut self, bounds: Box2D) {
        self.rect = bounds;
    }

    fn hit_test(&self, _pos: (f64, f64)) -> bool {
        false
    }

    fn draw(&self, out: &mut Vec<Primitive>) {
        out.push(Primitive::Fill {
            rect: self.rect,
            color: theme::TEXT,
            opacity: glass::STROKE_ALPHA,
        });
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// Карточка строки группы (§8.8): строка списка — карточка стекла,
/// раскрытая (выбранная) — залита светом `glass_on`. Лежит ПОД кнопкой
/// строки: кнопка даёт хит-тест и фазы продавливания (§5), карточка —
/// «тело» строки, видимое между кнопками и по краям.
struct GroupRowCard {
    id: WidgetId,
    bounds: Box2D,
    /// Раскрыта ли группа этой строки — выбранная строка заливается светом.
    selected: bool,
}

impl GroupRowCard {
    fn new(id: WidgetId, bounds: Box2D, selected: bool) -> Self {
        Self {
            id,
            bounds,
            selected,
        }
    }
}

impl Widget for GroupRowCard {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn bounds(&self) -> Box2D {
        self.bounds
    }

    fn set_bounds(&mut self, bounds: Box2D) {
        self.bounds = bounds;
    }

    /// Декорация: события строки принимает кнопка над ней.
    fn hit_test(&self, _pos: (f64, f64)) -> bool {
        false
    }

    fn draw(&self, out: &mut Vec<Primitive>) {
        // Радиус совпадает с авто-радиусом кнопки строки (RADIUS_TIGHT для
        // высоты ≤ BUTTON_SIZE): иначе из-под кнопки выглядывали бы углы
        // карточки с другим скруглением.
        if self.selected {
            glass_on(out, self.bounds, theme::RADIUS_TIGHT, 1.0);
        } else {
            glass_card(out, self.bounds, theme::RADIUS_TIGHT, 1.0);
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// Высота панели под `count` групп, из которых одна раскрыта на
/// `expanded_members` окон.
///
/// Считается формулой из тех же отступов, что и раскладка: иначе правка
/// любого зазора разъезжается с рамкой, и заметить это можно только глазами.
pub fn height(count: usize, expanded_members: usize) -> f64 {
    let rows = visible_rows(count) as f64;
    let members = expanded_members.min(MAX_MEMBERS) as f64;
    2.0 * theme::PAD_PANEL
        + LINE_HEIGHT
        + theme::GAP_ROW
        + rows * theme::BUTTON_SIZE
        + (rows - 1.0) * theme::GAP_ROW
        + members * (MEMBER_H + theme::GAP_ROW)
        + theme::GAP_ROW
        + theme::HAIRLINE
        + theme::GAP_ROW
        + theme::BUTTON_SIZE
}

/// Подпись пустого списка: комбинацию берёт вызывающий из ЖИВОГО конфига.
///
/// Раньше здесь стоял литерал «press Alt+Shift+G» — он пережил две смены
/// дефолта (`Ctrl+Alt+G`, затем `Ctrl+Shift+G`) и любую перенастройку
/// пользователем, то есть панель советовала нажать то, что ничего не делает
/// (и ровно ту пару `Alt+Shift`, которую Windows отдаёт переключателю
/// раскладки). Строка
/// собирается здесь, а не у вызывающего, чтобы формулировка обоих случаев
/// (хоткей есть / хоткея нет) лежала рядом с остальными подписями панели.
fn empty_label(open_hotkey: Option<&str>) -> String {
    match open_hotkey {
        Some(combo) => format!("No groups yet — press {combo} to build one."),
        None => EMPTY_LABEL_NO_HOTKEY.to_string(),
    }
}

/// Собрать панель.
///
/// `expanded` — индекс раскрытой группы в срезе `groups` (`None` — все
/// свёрнуты). Раскрыта не больше одной: две раскрытые группы по восемь окон
/// уже не помещаются на экран, а выбирать, какую обрезать, — решение, которое
/// пользователю объяснить нечем.
///
/// `open_hotkey` — комбинация, открывающая меню набора окон, в том виде, в
/// каком она РЕАЛЬНО зарегистрирована (`None` — не назначена или не
/// разбирается). Показывается только в подписи пустого списка.
pub fn build(
    groups: &[WindowGroup],
    expanded: Option<usize>,
    frame: Box2D,
    open_hotkey: Option<&str>,
) -> Panel {
    let mut panel = Panel::new(PANEL_ID, frame)
        .with_corner_radius(theme::RADIUS_WINDOW)
        .with_surface(glass::Surface::Modal);
    let left = frame.cx - frame.w / 2.0 + theme::PAD_PANEL;
    let right = frame.cx + frame.w / 2.0 - theme::PAD_PANEL;
    let top = frame.cy - frame.h / 2.0 + theme::PAD_PANEL;
    let content_w = right - left;

    let title_cy = top + LINE_HEIGHT / 2.0;
    panel.add_widget(StaticText::new(
        ID_TITLE,
        left,
        title_cy,
        TITLE_LABEL,
        theme::TEXT_OPACITY,
    ));

    let list_top = title_cy + LINE_HEIGHT / 2.0 + theme::GAP_ROW;
    // Список групп. Строки состава вставляются СРАЗУ под своей группой,
    // поэтому вертикальная позиция считается накопительно, а не по индексу:
    // раскрытая группа сдвигает всё, что ниже неё.
    let mut cy = list_top + theme::BUTTON_SIZE / 2.0;
    if groups.is_empty() {
        panel.add_widget(StaticText::new(
            ID_EMPTY,
            left,
            cy,
            &truncate_to_width(&empty_label(open_hotkey), content_w),
            theme::TEXT_DIM_OPACITY,
        ));
        // Подпись занимает строку списка ровно как настоящая группа —
        // [`height`] и считает её за строку (`visible_rows(0) == 1`).
        // Накопитель обязан сдвинуться на ту же строку, что и тело цикла
        // ниже: без этого разделитель и нижний ряд кнопок вставали на
        // место, где строки нет, и наезжали прямо на подпись (живой
        // репорт пользователя со скриншотом, 2026-08-31: текст «No groups
        // yet…» лежал поверх кнопок «New group» и «Close»).
        cy += theme::BUTTON_SIZE + theme::GAP_ROW;
    }
    for (i, group) in groups.iter().take(MAX_ROWS).enumerate() {
        let row_w = content_w - theme::BUTTON_SIZE - theme::GAP_ROW;
        // Подпись читается словами, а не набором чисел: «Group 3 — 4 windows».
        // Раньше здесь было «3 3 (4)» — номер, имя (совпадавшее с номером) и
        // счётчик в скобках, и понять это было нельзя (репорт 2026-08-26).
        let label = format!(
            "{} {} — {} {}",
            GROUP_WORD,
            group.number,
            group.members.len(),
            if group.members.len() == 1 {
                WINDOW_WORD
            } else {
                WINDOWS_WORD
            }
        );
        let row_bounds = Box2D {
            cx: left + row_w / 2.0,
            cy,
            w: row_w,
            h: theme::BUTTON_SIZE,
            rotation: 0.0,
        };
        panel.add_widget(GroupRowCard::new(
            ROW_BASE + i as WidgetId + CARD_FLAG,
            row_bounds,
            expanded == Some(i),
        ));
        panel.add_widget(Button::new(
            ROW_BASE + i as WidgetId,
            row_bounds,
            ButtonContent::Label(truncate_to_width(&label, row_w - 2.0 * theme::PAD_CTRL_X)),
        ));
        panel.add_widget(
            Button::new(
                DELETE_BASE + i as WidgetId,
                Box2D {
                    cx: right - theme::BUTTON_SIZE / 2.0,
                    cy,
                    w: theme::BUTTON_SIZE,
                    h: theme::BUTTON_SIZE,
                    rotation: 0.0,
                },
                ButtonContent::Label(DELETE_MARK.to_string()),
            )
            .with_label_color(theme::DANGER),
        );
        cy += theme::BUTTON_SIZE / 2.0;

        if expanded == Some(i) {
            for (m, member) in group.members.iter().take(MAX_MEMBERS).enumerate() {
                cy += theme::GAP_ROW + MEMBER_H / 2.0;
                // Слот виден числом: он объясняет, почему окна встают именно
                // так, — первое в списке попадает в главный слот раскладки.
                let text = format!("{}. {}", m + 1, member.title);
                panel.add_widget(StaticText::new(
                    MEMBER_BASE + m as WidgetId,
                    left + MEMBER_INDENT,
                    cy,
                    &truncate_to_width(&text, content_w - MEMBER_INDENT),
                    theme::TEXT_DIM_OPACITY,
                ));
                cy += MEMBER_H / 2.0;
            }
        }
        cy += theme::GAP_ROW + theme::BUTTON_SIZE / 2.0;
    }

    // Разделитель ставится под фактическим низом списка, а не по формуле:
    // раскрытая группа сдвигает его вниз, и второй счёт разъехался бы с
    // первым.
    let list_bottom = cy - theme::BUTTON_SIZE / 2.0 - theme::GAP_ROW;
    let divider_cy = list_bottom + theme::GAP_ROW;
    panel.add_widget(Hairline::new(ID_DIVIDER, frame.cx, divider_cy, content_w));

    // Нижний ряд действий — сразу под разделителем: строки переименования
    // между ними больше нет.
    let actions_cy = divider_cy + theme::GAP_ROW + theme::BUTTON_SIZE / 2.0;
    let new_w = text_size(NEW_LABEL).0 + 2.0 * theme::PAD_CTRL_X;
    panel.add_widget(Button::new(
        BTN_NEW,
        Box2D {
            cx: left + new_w / 2.0,
            cy: actions_cy,
            w: new_w,
            h: theme::BUTTON_SIZE,
            rotation: 0.0,
        },
        ButtonContent::Label(NEW_LABEL.to_string()),
    ));
    // «Править состав» есть только при раскрытой группе: без неё непонятно,
    // чей состав правим, а неактивная кнопка в этой панели ничем не
    // отличается от активной — стиль такого состояния не поддерживает.
    if expanded.is_some() {
        let edit_w = text_size(EDIT_LABEL).0 + 2.0 * theme::PAD_CTRL_X;
        panel.add_widget(Button::new(
            BTN_EDIT,
            Box2D {
                cx: left + new_w + theme::GAP_ROW + edit_w / 2.0,
                cy: actions_cy,
                w: edit_w,
                h: theme::BUTTON_SIZE,
                rotation: 0.0,
            },
            ButtonContent::Label(EDIT_LABEL.to_string()),
        ));
    }
    let close_w = text_size(CLOSE_LABEL).0 + 2.0 * theme::PAD_CTRL_X;
    panel.add_widget(Button::new(
        BTN_CLOSE,
        Box2D {
            cx: right - close_w / 2.0,
            cy: actions_cy,
            w: close_w,
            h: theme::BUTTON_SIZE,
            rotation: 0.0,
        },
        ButtonContent::Label(CLOSE_LABEL.to_string()),
    ));

    panel
}

#[cfg(test)]
mod tests {
    use super::*;
    use rst_core::model::{GroupMember, MAX_GROUP_MEMBERS};
    use rst_render::glass::Surface;
    use std::path::PathBuf;
    use uuid::Uuid;

    /// Комбинация меню набора в том виде, в каком её отдаёт координатор.
    const HOTKEY: Option<&str> = Some("Ctrl+Shift+G");

    fn member(title: &str) -> GroupMember {
        GroupMember {
            exe_path: PathBuf::from("app.exe"),
            title: title.to_string(),
            class: "Class".to_string(),
            place: None,
        }
    }

    fn group(number: u8, members: usize) -> WindowGroup {
        WindowGroup {
            id: Uuid::new_v4(),
            number,
            name: format!("группа {number}"),
            members: (0..members).map(|i| member(&format!("окно {i}"))).collect(),
            gap_pct: 5,
        }
    }

    fn frame(count: usize, expanded: usize) -> Box2D {
        Box2D {
            cx: 1280.0,
            cy: 720.0,
            w: WIDTH,
            h: height(count, expanded),
            rotation: 0.0,
        }
    }

    /// Все виджеты панели должны лежать внутри её рамки.
    fn assert_inside(panel: &Panel, f: Box2D, ids: &[WidgetId]) {
        let top = f.cy - f.h / 2.0;
        let bottom = f.cy + f.h / 2.0;
        for id in ids {
            let Some(b) = panel.widget::<Button>(*id).map(Widget::bounds) else {
                continue;
            };
            assert!(
                b.cy - b.h / 2.0 >= top - 0.5 && b.cy + b.h / 2.0 <= bottom + 0.5,
                "виджет {id} вылез за панель: {b:?} при рамке {top}..{bottom}"
            );
        }
    }

    /// Подсказка пустого списка обязана называть ЖИВУЮ комбинацию: до этого
    /// здесь стоял литерал `Alt+Shift+G`, переживший смену дефолта, — панель
    /// советовала нажать то, что не назначено (репорт 2026-09-16).
    #[test]
    fn empty_label_names_the_configured_hotkey() {
        assert!(empty_label(Some("Ctrl+Shift+G")).contains("Ctrl+Shift+G"));
        assert!(empty_label(Some("Ctrl+Shift+F9")).contains("Ctrl+Shift+F9"));
    }

    /// Хоткей снят в настройках — советовать нечего, кроме кнопки рядом:
    /// назвать несуществующую комбинацию было бы тем же обманом.
    #[test]
    fn empty_label_without_hotkey_points_at_the_button() {
        let text = empty_label(None);
        assert!(
            !text.contains('+'),
            "комбинации в тексте быть не должно: {text}"
        );
        assert!(text.contains(NEW_LABEL), "должна называть кнопку: {text}");
    }

    #[test]
    fn empty_list_still_builds_a_usable_panel() {
        // Первый запуск: групп нет. Панель обязана открыться и объяснить,
        // что делать, а не показать пустоту.
        let f = frame(0, 0);
        let panel = build(&[], None, f, HOTKEY);
        assert!(panel.widget::<StaticText>(ID_EMPTY).is_some());
        assert!(panel.widget::<Button>(BTN_CLOSE).is_some());
    }

    /// Подпись «групп ещё нет» не имеет права наезжать на нижний ряд
    /// кнопок (живой репорт со скриншотом, 2026-08-31): накопитель `cy` не
    /// сдвигался на пустую строку, и разделитель с кнопками вставали выше,
    /// чем сама подпись.
    #[test]
    fn empty_list_label_does_not_overlap_the_action_row() {
        let f = frame(0, 0);
        let panel = build(&[], None, f, HOTKEY);
        let label = panel
            .widget::<StaticText>(ID_EMPTY)
            .map(Widget::bounds)
            .expect("подпись пустого списка");
        let label_bottom = label.cy + label.h / 2.0;
        for id in [BTN_NEW, BTN_CLOSE] {
            let b = panel
                .widget::<Button>(id)
                .map(Widget::bounds)
                .expect("кнопка нижнего ряда");
            assert!(
                b.cy - b.h / 2.0 >= label_bottom,
                "кнопка {id} налезает на подпись: верх {} < низа подписи {label_bottom}",
                b.cy - b.h / 2.0
            );
        }
        // И весь нижний ряд обязан остаться внутри рамки — сдвиг не должен
        // выдавить его наружу.
        assert_inside(&panel, f, &[BTN_NEW, BTN_CLOSE]);
    }

    #[test]
    fn every_group_gets_its_own_row_and_delete_button() {
        let groups: Vec<WindowGroup> = (1..=3).map(|n| group(n, 2)).collect();
        let f = frame(groups.len(), 0);
        let panel = build(&groups, None, f, HOTKEY);
        for i in 0..groups.len() {
            assert!(
                panel.widget::<Button>(ROW_BASE + i as WidgetId).is_some(),
                "нет строки группы {i}"
            );
            assert!(
                panel
                    .widget::<Button>(DELETE_BASE + i as WidgetId)
                    .is_some(),
                "нет кнопки удаления группы {i}"
            );
        }
    }

    #[test]
    fn a_collapsed_group_does_not_show_its_windows() {
        let groups = vec![group(1, 3)];
        let panel = build(&groups, None, frame(1, 0), HOTKEY);
        assert!(
            panel.widget::<StaticText>(MEMBER_BASE).is_none(),
            "свёрнутая группа не должна показывать состав"
        );
    }

    #[test]
    fn an_expanded_group_lists_every_window_inside_it() {
        let groups = vec![group(1, 4)];
        let panel = build(&groups, Some(0), frame(1, 4), HOTKEY);
        for m in 0..4 {
            assert!(
                panel
                    .widget::<StaticText>(MEMBER_BASE + m as WidgetId)
                    .is_some(),
                "нет строки окна {m}"
            );
        }
    }

    #[test]
    fn nine_groups_with_a_full_one_expanded_still_fit_the_panel() {
        // Предельный случай: девять групп (по числу хоткеев) и раскрытая
        // группа из восьми окон. Формула высоты обязана его учитывать,
        // иначе кнопки уезжают за нижний край.
        let groups: Vec<WindowGroup> = (1..=MAX_ROWS as u8)
            .map(|n| group(n, MAX_GROUP_MEMBERS))
            .collect();
        let f = frame(groups.len(), MAX_GROUP_MEMBERS);
        let panel = build(&groups, Some(0), f, HOTKEY);
        let mut ids: Vec<WidgetId> = vec![BTN_NEW, BTN_CLOSE];
        for i in 0..groups.len() {
            ids.push(ROW_BASE + i as WidgetId);
            ids.push(DELETE_BASE + i as WidgetId);
        }
        assert_inside(&panel, f, &ids);
    }

    #[test]
    fn expanding_a_group_pushes_the_rows_below_it_down() {
        // Строки состава вставляются под своей группой, а не поверх — иначе
        // раскрытая группа накрыла бы соседнюю.
        let groups: Vec<WindowGroup> = (1..=3).map(|n| group(n, 3)).collect();
        let collapsed = build(&groups, None, frame(3, 0), HOTKEY);
        let expanded = build(&groups, Some(0), frame(3, 3), HOTKEY);
        let row1_collapsed = collapsed
            .widget::<Button>(ROW_BASE + 1)
            .map(Widget::bounds)
            .expect("строка группы 2");
        let row1_expanded = expanded
            .widget::<Button>(ROW_BASE + 1)
            .map(Widget::bounds)
            .expect("строка группы 2");
        assert!(
            row1_expanded.cy > row1_collapsed.cy,
            "раскрытие первой группы обязано сдвинуть вторую вниз"
        );
    }

    #[test]
    fn the_row_label_reads_as_words_not_as_a_row_of_numbers() {
        // Раньше подпись была «3 3 (4)» — номер, имя (совпадавшее с номером)
        // и счётчик в скобках, и понять её было нельзя (репорт 2026-08-26).
        // Теперь она читается словами и несёт номер — цифру хоткея.
        let groups = vec![group(7, 2)];
        let panel = build(&groups, None, frame(1, 0), HOTKEY);
        let mut prims = Vec::new();
        panel.draw(&mut prims);
        let readable = prims.iter().any(|p| match p {
            rst_render::Primitive::Text { text, .. } => {
                let t = text.trim_start();
                t.starts_with(GROUP_WORD) && t.contains('7')
            }
            _ => false,
        });
        assert!(
            readable,
            "подпись строки обязана читаться словами и нести номер группы"
        );
    }

    #[test]
    fn a_degenerate_frame_does_not_panic() {
        // Гонка переподключения монитора: экран может прийти вырожденным.
        let f = Box2D {
            cx: 0.0,
            cy: 0.0,
            w: 1.0,
            h: 1.0,
            rotation: 0.0,
        };
        let groups = vec![group(1, 2)];
        let _ = build(&groups, Some(0), f, HOTKEY);
    }

    #[test]
    fn a_group_with_more_windows_than_the_table_allows_is_clamped() {
        // Конфиг правят руками: девять окон в группе — не повод рисовать
        // строку за краем панели.
        let mut g = group(1, MAX_GROUP_MEMBERS);
        g.members.push(member("лишнее окно"));
        let f = frame(1, MAX_GROUP_MEMBERS);
        let panel = build(&[g], Some(0), f, HOTKEY);
        assert!(
            panel
                .widget::<StaticText>(MEMBER_BASE + MAX_MEMBERS as WidgetId)
                .is_none(),
            "девятое окно рисовать некуда"
        );
        assert_inside(&panel, f, &[BTN_NEW, BTN_CLOSE]);
    }

    #[test]
    fn panel_body_is_a_glass_panel() {
        // §4: корпус — одна плита стекла. Раньше фон был «рамка + заливка»
        // (два Fill), теперь — одна Primitive::Glass с Surface::Modal.
        let f = frame(0, 0);
        let panel = build(&[], None, f, HOTKEY);
        let mut prims = Vec::new();
        panel.draw(&mut prims);
        assert!(
            prims.iter().any(|p| matches!(
                p,
                Primitive::Glass {
                    surface: Surface::Modal,
                    ..
                }
            )),
            "корпус панели обязан быть плитой стекла"
        );
    }

    #[test]
    fn expanded_row_is_drawn_selected_and_collapsed_rows_are_cards() {
        // §8.8: строка списка — карточка стекла, выбранная (раскрытая)
        // строка залита светом. Раньше фон строки был плоской заливкой.
        let groups: Vec<WindowGroup> = (1..=2).map(|n| group(n, 2)).collect();
        let collapsed = build(&groups, None, frame(2, 0), HOTKEY);
        let mut prims = Vec::new();
        collapsed.draw(&mut prims);
        assert!(
            prims.iter().any(|p| matches!(
                p,
                Primitive::Glass {
                    surface: Surface::Card,
                    ..
                }
            )),
            "свёрнутые строки обязаны быть карточками стекла"
        );
        let expanded = build(&groups, Some(0), frame(2, 2), HOTKEY);
        let mut prims = Vec::new();
        expanded.draw(&mut prims);
        assert!(
            prims.iter().any(|p| matches!(
                p,
                Primitive::Glass {
                    surface: Surface::ControlOn,
                    ..
                }
            )),
            "раскрытая строка обязана читаться выбранной (glass_on)"
        );
    }
}
