//! Лента карточек открытых окон — панель набора группы (T8, меню
//! редактирования групп, `groups.rs`).
//!
//! Внизу экрана горизонтальная лента карточек — как переключатель Alt+Tab.
//! Пользователь кликами отмечает несколько окон, чтобы собрать из них группу.
//! Отмеченная карточка загорается стеклом ControlOn и получает цифру слота
//! (с единицы): порядок набора решает, какое окно станет главным в раскладке
//! ([`crate::groups::GroupEditor::slot_of`]), и пользователь обязан этот
//! порядок видеть.
//!
//! # Кто что готовит
//!
//! Модуль — чистый строитель панели, как `preset_picker.rs`: на вход — срез
//! готовых описаний карточек ([`StripCard`]: заголовок, иконка, снимок окна
//! в RGBA, номер слота) и прямоугольник экрана в DIP, на выход — [`Panel`]
//! и сведения для декодирования кликов ([`StripPanel`]). Никакого Win32 и
//! никакого состояния: снимки снимает и кэширует вызывающий код
//! (`rst_win32::thumb_cache`), отметки хранит [`crate::groups::GroupEditor`],
//! здесь только раскладка и отрисовка. Состояние «активна ли кнопка
//! подтверждения» тоже выводится из входа: считается число карточек со
//! слотом — ровно то, что значит `GroupEditor::can_confirm`.
//!
//! # Прокрутка, а не ужатие
//!
//! Карточек может быть больше, чем влезает, — открытых окон на десктопе
//! больше десятка почти всегда. Выбрана прокрутка, а не ужатие: снимок
//! окна обязан оставаться узнаваемым, а пропорции — неискажёнными, ужатая
//! до трети карточка неотличима от соседней. Это же поведение уже есть у
//! списков окон (`window_pick_list`, `window_picker`): полоса прокрутки +
//! колесо мыши, привычные мышцы. Карточки за пределами ленты не строятся
//! вовсе — молча «за экраном» ничего не рисуется, а явная полоса
//! прокрутки ([`HScrollBar`]) намекает, что лента длиннее.
//!
//! # Ключи текстур снимков приходят от вызывающего
//!
//! `Primitive::Rgba` требует стабильный `key` для кэша текстур, и первый
//! аплоад по ключу побеждает (`UiTextureCache::rgba_icons`). Для иконок это
//! удобно (окна одного процесса делят одну текстуру), но для СНИМКОВ
//! содержимого ключ обязан различать поколения захвата: иначе повторный
//! захват того же окна не обновит превью — лента показывала бы первый
//! кадр, пока не сменится разрешение. Поэтому [`StripImage::key`] задаёт
//! вызывающий код: это он знает, когда снят новый снимок. Строитель лишь
//! переносит ключ в примитив.
//!
//! # Кнопка подтверждения
//!
//! Галочка внизу справа активна только когда отмечено не меньше
//! [`MIN_GROUP_MEMBERS`] окон: группа из одного окна — это просто окно
//! (тот же критерий, что у [`crate::groups::GroupEditor::can_confirm`]).
//! У `Button` в rst-render нет disabled-состояния, поэтому кнопка — свой
//! виджет [`ConfirmButton`] с семантикой `Checkbox::set_disabled`:
//! неактивная не отвечает на хит-тест и рисуется полупрозрачной (0.45),
//! клик опрашивается как у кнопки — `take_click`. Материал — первичное
//! стекло (§2.2 `CTRL_BG_PRIMARY`). Галочка нарисована двумя
//! повёрнутыми прямоугольниками: в наборе [`Icon`] галочки нет, а глиф «✓»
//! в шрифте не гарантирован.
//!
//! # Кнопка списка групп
//!
//! Левый угол ленты — [`BTN_MANAGER`], та же иконка `Icon::Groups`, что у
//! кнопки «Группы окон» в панели у курсора. Она не отмечает окна и ничего не
//! подтверждает: координатор поднимает по ней уже существующий флаг
//! открытия менеджера, и панель со списком групп встаёт ПОВЕРХ ленты, не
//! разрушая набор (запрос пользователя 2026-08-27). Кнопка стоит вне
//! прокручиваемой области, как и галочка: обе обязаны оставаться на месте,
//! сколько бы карточек ни листали.
//!
//! # Модуль пока не подключён к координатору
//!
//! Лента полностью готова и покрыта тестами, но её вызов из
//! `overlay_manager.rs` — отдельная задача (координатор ведёт этот файл).
//! До подключения бинарь не ссылается на модуль, и rustc честно сообщает,
//! что он мёртвый — `allow(dead_code)` снимает этот шум. При подключении
//! ленты этот атрибут нужно убрать.

#![allow(dead_code)]

use rst_core::model::MIN_GROUP_MEMBERS;
use rst_core::ui_motion::{Phase, stagger_delay_ms};
use rst_render::{
    Box2D, Button, ButtonContent, Icon, LINE_HEIGHT, Panel, PointerEvent, Primitive, Widget,
    WidgetId, box_contains, glass_card, glass_control, glass_on, glass_sunken, text_size, theme,
};

use crate::window_picker::truncate_to_width;

/// Идентификатор панели ленты. Диапазон 600+: тулбар 0-8, панель у курсора
/// 100+, панель выбора окон 200+, пресеты 400+, список закрепления 500+.
pub const PANEL_ID: WidgetId = 600;
/// Первая карточка ленты; `CARD_BASE + индекс` в срезе `cards` — тот же
/// индекс, по которому вызывающий код декодирует клик обратно в
/// `StripCard::hwnd` (тот же контракт, что `window_pick_list::ROW_BASE`).
pub const CARD_BASE: WidgetId = 601;
/// Кнопка подтверждения набора (галочка, правая часть ленты).
pub const BTN_CONFIRM: WidgetId = 650;
/// Кнопка «Список групп» — открывает менеджер групп поверх меню набора
/// (запрос пользователя 2026-08-27). Левый угол ленты, зеркально галочке
/// подтверждения в правом: два края ленты — два выхода из неё, «посмотреть,
/// что уже собрано» и «закончить».
pub const BTN_MANAGER: WidgetId = 652;
/// Полоса горизонтальной прокрутки ленты (рисуется только при переполнении).
const SCROLLBAR_ID: WidgetId = 651;
/// Флаг для id содержимого карточки — не декодируется вызывающим кодом (тот
/// же приём, что `window_pick_list::LABEL_FLAG`: интерактив карточки —
/// `Button` под тем же индексом, содержимое поверх — неинтерактивный виджет).
const LABEL_FLAG: WidgetId = 0x8000_0000;

/// Отступ ленты от краёв экрана, DIP. В §3 токена нет — внешний отступ
/// панели от края рабочей области, локальное решение.
const SCREEN_MARGIN: f64 = 20.0;
/// Внутренний отступ ленты, DIP — §3 PAD_PANEL.
const STRIP_PAD: f64 = theme::PAD_PANEL;
/// Ширина карточки, DIP. В §3 токена нет — размер снимка окна, локальное
/// решение (скругление карточки при этом — §3 RADIUS_CARD).
pub const CARD_W: f64 = 132.0;
/// Высота карточки, DIP. В §3 токена нет — см. [`CARD_W`].
pub const CARD_H: f64 = 96.0;
/// Зазор между карточками, DIP — §3 GAP_ROW.
const CARD_GAP: f64 = theme::GAP_ROW;
/// Внутренний отступ карточки, DIP — §3 PAD_CTRL_X.
const CARD_PAD: f64 = theme::PAD_CTRL_X;
/// Высота полосы снимка карточки (над подписью), DIP: карточка минус два
/// отступа минус полоса подписи (`LINE_HEIGHT` + отступ под ней).
const THUMB_H: f64 = CARD_H - 2.0 * CARD_PAD - LINE_HEIGHT - CARD_PAD;
/// Сторона бейджа номера слота, DIP. В §3 токена нет — скругление бейджа
/// при этом §3 RADIUS_TIGHT (мелкий бейдж).
const BADGE_SIZE: f64 = 18.0;
/// Отступ бейджа от угла карточки, DIP. В §3 токена нет.
const BADGE_GAP: f64 = 4.0;
/// Толщина штриха галочки, DIP. В §3 токена нет — штрих глифа, иконки не
/// переделываются (§8 спецификации).
const CHECK_THICKNESS: f64 = 2.0;
/// Сторона квадратной кнопки подтверждения — та же, что у кнопок тулбара
/// (визуально лента и тулбар — родные панели режима редактирования).
const CONFIRM_SIZE: f64 = theme::BUTTON_SIZE;

/// Готовый RGBA-растр карточки (иконка окна или снимок его содержимого).
///
/// `key` — стабильный идентификатор для кэша текстур вызывающего слоя
/// (`Primitive::Rgba.key`), см. докмодуль: его задаёт тот, кто готовит
/// данные, потому что только он знает, когда снимок стал новым.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StripImage {
    pub key: u64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Описание одной карточки ленты. Всё готовит вызывающий код: заголовок и
/// иконку — из `WindowInfo`, снимок — из `thumb_cache`, слот — из
/// `GroupEditor::slot_of` (в порядке набора, с единицы).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StripCard {
    pub hwnd: usize,
    pub title: String,
    pub icon: Option<StripImage>,
    pub thumb: Option<StripImage>,
    pub slot: Option<usize>,
}

/// Результат [`build`]: панель + сведения для скролла и декодирования
/// кликов (тот же контракт, что `window_pick_list::PickListPanel`).
pub struct StripPanel {
    pub panel: Panel,
    /// Всего карточек в срезе (не зависит от скролла).
    pub total_cards: usize,
    /// Сколько карточек влезает в ленту (полоса прокрутки появляется, когда
    /// `total_cards > visible_cards`).
    pub visible_cards: usize,
}

/// Высота ленты, DIP — панель не растёт от числа карточек, лишние
/// прокручиваются.
pub fn strip_height() -> f64 {
    2.0 * STRIP_PAD + CARD_H
}

/// Собрать ленту карточек внизу экрана `screen` (DIP).
///
/// `scroll` — сколько карточек пропущено слева (виртуализация: строятся
/// только видимые, как у `window_pick_list`). Строитель сам скролл не
/// клампит — вызывающий код правит его после `build` (та же схема, что у
/// `rebuild_window_pick_list`).
pub fn build(cards: &[StripCard], scroll: usize, screen: Box2D) -> StripPanel {
    // Лента прижата к нижнему краю экрана с отступом SCREEN_MARGIN. Если
    // экран ниже самой ленты, верхний край клампится к верху экрана с тем
    // же отступом: вырожденный экран не должен утащить ленту за нижний край
    // (переполнение вверх — единственный вариант, и он не паникует).
    // Ширина — по фактическому числу карточек, но не шире экрана: лента из
    // трёх окон во весь экран выглядит сломанной, а из тридцати обязана
    // прокручиваться, а не вылезать за края (живой репорт 2026-08-25).
    let available = (screen.w - 2.0 * SCREEN_MARGIN).max(0.0);
    let n = cards.len().max(1) as f64;
    let needed = 2.0 * STRIP_PAD
        + n * CARD_W
        + (n - 1.0) * CARD_GAP
        // Две квадратные кнопки по краям: список групп слева, галочка справа.
        + 2.0 * (CARD_GAP + CONFIRM_SIZE);
    let strip_w = needed.min(available);
    let strip_h = strip_height();
    let screen_top = screen.cy - screen.h / 2.0;
    let screen_bottom = screen.cy + screen.h / 2.0;
    let mut strip_top = screen_bottom - SCREEN_MARGIN - strip_h;
    if strip_top < screen_top + SCREEN_MARGIN {
        strip_top = screen_top + SCREEN_MARGIN;
    }
    let frame = Box2D {
        cx: screen.cx,
        cy: strip_top + strip_h / 2.0,
        w: strip_w,
        h: strip_h,
        rotation: 0.0,
    };
    let mut panel = Panel::new(PANEL_ID, frame).with_corner_radius(theme::RADIUS_WINDOW);

    let left = frame.cx - frame.w / 2.0 + STRIP_PAD;
    let right = frame.cx + frame.w / 2.0 - STRIP_PAD;
    // Галочка — крайний правый элемент ленты, карточки занимают остаток:
    // подтверждение всегда на виду, даже когда лента длинная и прокручена.
    let confirm_cx = right - CONFIRM_SIZE / 2.0;
    let cards_right = confirm_cx - CONFIRM_SIZE / 2.0 - CARD_GAP;
    // Кнопка списка групп — крайняя левая, тем же поводом: она не должна
    // уезжать вместе с прокруткой карточек.
    let manager_cx = left + CONFIRM_SIZE / 2.0;
    let cards_left = manager_cx + CONFIRM_SIZE / 2.0 + CARD_GAP;
    let visible_w = cards_right - cards_left;
    // Сколько карточек влезает: первый центр — в `left + CARD_W/2`, дальше
    // шаг `CARD_W + CARD_GAP`, последняя карточка обязана закончиться не
    // правее `cards_right`. Неполная карточка у правого края не строится —
    // за краем ленты ничего не рисуется молча.
    let visible_cards = if visible_w <= 0.0 {
        0
    } else {
        ((visible_w + CARD_GAP) / (CARD_W + CARD_GAP)).floor() as usize
    };

    let picked = cards.iter().filter(|c| c.slot.is_some()).count();
    for (i, card) in cards.iter().enumerate().skip(scroll).take(visible_cards) {
        let x = cards_left + CARD_W / 2.0 + (i - scroll) as f64 * (CARD_W + CARD_GAP);
        let rect = Box2D {
            cx: x,
            cy: frame.cy,
            w: CARD_W,
            h: CARD_H,
            rotation: 0.0,
        };
        // Фон, hover/armed, хит-тест карточки и гало наведения — кнопка без
        // своей подписи (пустая подпись не эмитит текст, тот же приём, что
        // строки `window_pick_list`); материал карточки — стекло Card с
        // каскадом появления, его рисует `CardContent` поверх.
        panel.add_widget(Button::new(
            CARD_BASE + i as WidgetId,
            rect,
            ButtonContent::Label(String::new()),
        ));
        let title = truncate_to_width(&card.title, CARD_W - 2.0 * CARD_PAD);
        let mut appear = Phase::new(theme::CARD_IN_MS);
        appear.set_target(true);
        panel.add_widget(CardContent {
            id: CARD_BASE + i as WidgetId + LABEL_FLAG,
            card: rect,
            thumb: card.thumb.clone(),
            icon: card.icon.clone(),
            title,
            slot: card.slot,
            appear,
            // Каскад от первой карточки ленты (§5): каждая следующая ждёт
            // свой шаг, и ряд «выплывает», а не появляется разом.
            delay_ms: stagger_delay_ms(i),
        });
    }

    // Иконка та же, что у кнопки «Группы окон» в панели у курсора: одна и та
    // же панель, вызванная из двух мест, обязана выглядеть одинаково.
    // Мелкая иконка-кнопка — §3 RADIUS_TIGHT (высота кнопки не больше
    // BUTTON_SIZE + 2, радиус выбирает сам `Button`).
    panel.add_widget(Button::new(
        BTN_MANAGER,
        Box2D {
            cx: manager_cx,
            cy: frame.cy,
            w: CONFIRM_SIZE,
            h: CONFIRM_SIZE,
            rotation: 0.0,
        },
        ButtonContent::Icon(Icon::Groups),
    ));

    panel.add_widget(ConfirmButton::new(
        BTN_CONFIRM,
        Box2D {
            cx: confirm_cx,
            cy: frame.cy,
            w: CONFIRM_SIZE,
            h: CONFIRM_SIZE,
            rotation: 0.0,
        },
        picked < MIN_GROUP_MEMBERS,
    ));

    // Полоса прокрутки — только когда есть что листать; при нуле видимых
    // карточек (вырожденный экран) листать бесполезно — лента пуста.
    if cards.len() > visible_cards && visible_cards > 0 {
        panel.add_widget(HScrollBar::new(
            SCROLLBAR_ID,
            Box2D {
                cx: (cards_left + cards_right) / 2.0,
                cy: frame.cy + frame.h / 2.0 - STRIP_PAD / 2.0,
                w: visible_w,
                h: theme::SCROLLBAR_WIDTH,
                rotation: 0.0,
            },
            visible_cards,
            cards.len(),
            scroll,
        ));
    }

    StripPanel {
        panel,
        total_cards: cards.len(),
        visible_cards,
    }
}

/// Содержимое карточки: стеклянное тело (Card, у отмеченной — ControlOn),
/// снимок окна (или иконка как fallback, или утопленный плейсхолдер),
/// подпись и бейдж слота. Не интерактивно — клики и hover строки
/// обрабатывает `Button` под ним (тот же приём разделения «фон/интерактив»
/// и «контент», что `window_pick_list::RowContent`).
struct CardContent {
    id: WidgetId,
    card: Box2D,
    thumb: Option<StripImage>,
    icon: Option<StripImage>,
    title: String,
    slot: Option<usize>,
    /// Фаза появления карточки (§5, CARD_IN_MS): тело и тексты гаснут
    /// вместе с ней, иначе содержимое выскочило бы раньше стекла.
    appear: Phase,
    /// Остаток задержки каскада до начала появления, мс.
    delay_ms: f64,
}

impl Widget for CardContent {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn bounds(&self) -> Box2D {
        self.card
    }

    fn set_bounds(&mut self, bounds: Box2D) {
        self.card = bounds;
    }

    fn hit_test(&self, _pos: (f64, f64)) -> bool {
        false
    }

    fn animate(&mut self, dt_ms: f64) -> bool {
        if self.delay_ms > 0.0 {
            self.delay_ms = (self.delay_ms - dt_ms).max(0.0);
            return true;
        }
        self.appear.advance(dt_ms)
    }

    fn draw(&self, out: &mut Vec<Primitive>) {
        let (left, top, bottom) = (
            self.card.cx - self.card.w / 2.0,
            self.card.cy - self.card.h / 2.0,
            self.card.cy + self.card.h / 2.0,
        );
        let appear = self.appear.eased();
        // Отмеченная карточка — стекло ControlOn (§2.2 «включённый
        // переключатель»): выбор читается светом и усиленной обводкой, а не
        // рамкой-состоянием VGUI.
        if self.slot.is_some() {
            glass_on(out, self.card, theme::RADIUS_CARD, appear);
        } else {
            glass_card(out, self.card, theme::RADIUS_CARD, appear);
        }
        let thumb_area = Box2D {
            cx: self.card.cx,
            cy: top + CARD_PAD + THUMB_H / 2.0,
            w: self.card.w - 2.0 * CARD_PAD,
            h: THUMB_H,
            rotation: 0.0,
        };
        // Снимок — если есть, иначе иконка в натуральном размере, иначе
        // утопленный жёлоб-плейсхолдер: слот снимка никогда не пустует
        // молча (тот же приём, что плейсхолдер иконки в `window_pick_list`).
        match self.thumb.as_ref().or(self.icon.as_ref()) {
            Some(img) => {
                let rect = if self.thumb.is_some() {
                    fit_rect(thumb_area, img.width, img.height)
                } else {
                    // Иконка маленькая и рисованная под натуральный размер —
                    // растягивать её на весь слот значило бы размыть пиксель-арт.
                    Box2D {
                        cx: thumb_area.cx,
                        cy: thumb_area.cy,
                        w: f64::from(img.width),
                        h: f64::from(img.height),
                        rotation: 0.0,
                    }
                };
                out.push(Primitive::Rgba {
                    rect,
                    key: img.key,
                    width: img.width,
                    height: img.height,
                    rgba: img.rgba.clone(),
                    opacity: appear,
                });
            }
            None => glass_sunken(out, thumb_area, theme::RADIUS_CTRL, appear),
        }
        if let Some(slot) = self.slot {
            // Бейдж с цифрой — тот же свет, что и у тела отмеченной
            // карточки: «это окно уже в группе, и оно n-е» (§2.2 ControlOn,
            // скругление мелкого бейджа — §3 RADIUS_TIGHT).
            let badge = Box2D {
                cx: left + BADGE_GAP + BADGE_SIZE / 2.0,
                cy: top + BADGE_GAP + BADGE_SIZE / 2.0,
                w: BADGE_SIZE,
                h: BADGE_SIZE,
                rotation: 0.0,
            };
            glass_on(out, badge, theme::RADIUS_TIGHT, appear);
            let digit = slot.to_string();
            let (dw, dh) = text_size(&digit);
            out.push(Primitive::Text {
                rect: Box2D {
                    cx: badge.cx,
                    cy: badge.cy,
                    w: dw,
                    h: dh,
                    rotation: 0.0,
                },
                text: digit,
                color: theme::TEXT,
                opacity: theme::TEXT_OPACITY * appear,
            });
        }
        if !self.title.is_empty() {
            let (tw, th) = text_size(&self.title);
            out.push(Primitive::Text {
                rect: Box2D {
                    cx: self.card.cx,
                    cy: bottom - CARD_PAD - LINE_HEIGHT / 2.0,
                    w: tw,
                    h: th,
                    rotation: 0.0,
                },
                text: self.title.clone(),
                color: theme::TEXT,
                opacity: theme::TEXT_OPACITY * appear,
            });
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// Вписать кадр `w×h` в `area`, сохраняя пропорции (буквбоксинг): снимок
/// окна рисуется с натуральным соотношением сторон, иначе окно выглядело бы
/// растянутым.
fn fit_rect(area: Box2D, w: u32, h: u32) -> Box2D {
    let scale = (area.w / f64::from(w)).min(area.h / f64::from(h)).max(0.0);
    Box2D {
        cx: area.cx,
        cy: area.cy,
        w: f64::from(w) * scale,
        h: f64::from(h) * scale,
        rotation: 0.0,
    }
}

/// Два штриха галочки в квадрате содержимого кнопки: из верхнего левого угла
/// вниз к вершине и из вершины вверх вправо (классическая «птичка», ось y
/// вниз). Чистая функция — тестируется без панели.
fn checkmark_strokes(content: Box2D) -> [Box2D; 2] {
    let (w, h) = (content.w, content.h);
    let (x0, y0) = (content.cx - w / 2.0, content.cy - h / 2.0);
    let a = (x0 + 0.26 * w, y0 + 0.42 * h);
    let v = (x0 + 0.46 * w, y0 + 0.62 * h);
    let c = (x0 + 0.76 * w, y0 + 0.30 * h);
    [stroke(a, v), stroke(v, c)]
}

/// Штрих-прямоугольник между точками `a` и `b` толщиной
/// [`CHECK_THICKNESS`]: центр — середина отрезка, ширина — длина, поворот —
/// направление (та же геометрия, что `SelectionBox::outline_rects`).
fn stroke(a: (f64, f64), b: (f64, f64)) -> Box2D {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    Box2D {
        cx: (a.0 + b.0) / 2.0,
        cy: (a.1 + b.1) / 2.0,
        w: dx.hypot(dy),
        h: CHECK_THICKNESS,
        rotation: dy.atan2(dx),
    }
}

/// Кнопка подтверждения набора (галочка).
///
/// Своя, потому что у `rst_render::Button` нет disabled-состояния, а
/// требование «неактивна, пока отмечено меньше двух окон» — визуальное и
/// интерактивное сразу (см. докмодуль). Поведение — как у кнопки, состояние
/// — как у чекбокса: неактивная не ловит хит-тест и рисуется с
/// непрозрачностью 0.45. Материал — первичное стекло (§2.2 CTRL_BG_PRIMARY):
/// подтверждение набора — главное действие ленты и обязано быть ярче
/// остальных контролов; фазы наведения/нажатия — §5.
pub struct ConfirmButton {
    id: WidgetId,
    bounds: Box2D,
    disabled: bool,
    hovered: bool,
    armed: bool,
    clicked: bool,
    /// Фаза наведения 0..1 (§5): кнопка продавливается плавно, а не ступенькой.
    hover_phase: Phase,
    /// Фаза нажатия 0..1 (§5).
    press_phase: Phase,
}

impl ConfirmButton {
    fn new(id: WidgetId, bounds: Box2D, disabled: bool) -> Self {
        Self {
            id,
            bounds,
            disabled,
            hovered: false,
            armed: false,
            clicked: false,
            hover_phase: Phase::new(theme::HOVER_MS),
            press_phase: Phase::new(theme::PRESS_MS),
        }
    }

    /// Был ли клик с прошлого опроса (флаг сбрасывается) — тот же контракт,
    /// что `Button::take_click`.
    pub fn take_click(&mut self) -> bool {
        std::mem::take(&mut self.clicked)
    }
}

impl Widget for ConfirmButton {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn bounds(&self) -> Box2D {
        self.bounds
    }

    fn set_bounds(&mut self, bounds: Box2D) {
        self.bounds = bounds;
    }

    fn hit_test(&self, pos: (f64, f64)) -> bool {
        !self.disabled && box_contains(&self.bounds, pos)
    }

    fn set_hovered(&mut self, hovered: bool) -> bool {
        let changed = std::mem::replace(&mut self.hovered, hovered) != hovered;
        if changed {
            self.hover_phase.set_target(hovered);
        }
        changed
    }

    fn pointer_event(&mut self, ev: PointerEvent) -> bool {
        match ev {
            PointerEvent::Down { pos } => {
                if self.disabled {
                    return false;
                }
                if self.hit_test(pos) {
                    self.armed = true;
                    self.press_phase.set_target(true);
                    return true;
                }
                false
            }
            PointerEvent::Up { pos } => {
                if !self.armed {
                    return false;
                }
                self.armed = false;
                self.press_phase.set_target(false);
                if !self.disabled && self.hit_test(pos) {
                    self.clicked = true;
                }
                true
            }
            PointerEvent::Move { .. } | PointerEvent::Wheel { .. } => false,
        }
    }

    fn animate(&mut self, dt_ms: f64) -> bool {
        let hover = self.hover_phase.advance(dt_ms);
        let press = self.press_phase.advance(dt_ms);
        hover || press
    }

    fn draw(&self, out: &mut Vec<Primitive>) {
        // Первичное стекло (§2.2 CTRL_BG_PRIMARY): подтверждение — главное
        // действие ленты. Приглушение неактивной — та же непрозрачность,
        // что у disabled чекбокса: кнопка видна, но явно «не для нажатия».
        let opacity = if self.disabled { 0.45 } else { 1.0 };
        glass_control(
            out,
            self.bounds,
            theme::RADIUS_TIGHT,
            self.hover_phase.eased(),
            self.press_phase.eased(),
            true,
            opacity,
        );
        let pad = theme::BUTTON_PAD;
        let content = Box2D {
            w: (self.bounds.w - 2.0 * pad).max(0.0),
            h: (self.bounds.h - 2.0 * pad).max(0.0),
            ..self.bounds
        };
        for s in checkmark_strokes(content) {
            out.push(Primitive::Fill {
                rect: s,
                color: theme::TEXT,
                opacity,
            });
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// Горизонтальная полоса прокрутки ленты: только отрисовка, неинтерактивная
/// (колесо мыши двигает `scroll` на вызывающем слое — тот же контракт, что у
/// `rst_render::ScrollBar`). Вертикальный `ScrollBar` rst-render не годится:
/// он рисует ручку вдоль СВОЕЙ длинной стороны, то есть всегда вертикально.
struct HScrollBar {
    id: WidgetId,
    bounds: Box2D,
    thumb_fraction: f64,
    thumb_offset: f64,
}

impl HScrollBar {
    /// `visible`/`total` — то же, что `StripPanel::visible_cards`/
    /// `total_cards`; `scroll` — текущая позиция в карточках. `total <=
    /// visible` даёт ручку во всю дорожку (не паникует — вызывающий слой
    /// обычно вообще не добавляет виджет в этом случае).
    fn new(id: WidgetId, bounds: Box2D, visible: usize, total: usize, scroll: usize) -> Self {
        let total = total.max(1);
        let thumb_fraction = (visible as f64 / total as f64).min(1.0);
        let max_scroll = total.saturating_sub(1).max(1);
        let thumb_offset = if total <= visible {
            0.0
        } else {
            (scroll.min(max_scroll) as f64 / max_scroll as f64) * (1.0 - thumb_fraction)
        };
        Self {
            id,
            bounds,
            thumb_fraction,
            thumb_offset,
        }
    }
}

impl Widget for HScrollBar {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn bounds(&self) -> Box2D {
        self.bounds
    }

    fn set_bounds(&mut self, bounds: Box2D) {
        self.bounds = bounds;
    }

    fn hit_test(&self, _pos: (f64, f64)) -> bool {
        false
    }

    fn draw(&self, out: &mut Vec<Primitive>) {
        let (w, h) = (self.bounds.w.max(0.0), self.bounds.h.max(0.0));
        // Дорожка — утопленный жёлоб (§2.2 SUNKEN_BG, §4 «утопленная
        // поверхность переворачивает свет»), ручка — белый свет (§2.4):
        // у скроллбара нет своего токена в §3, но материал тот же.
        glass_sunken(
            out,
            Box2D {
                w,
                h,
                ..self.bounds
            },
            theme::RADIUS_TIGHT,
            1.0,
        );
        let left = self.bounds.cx - self.bounds.w / 2.0;
        let thumb_w = (self.bounds.w * self.thumb_fraction).max(theme::SCROLLBAR_MIN_THUMB_H);
        let thumb_left = left + (self.bounds.w - thumb_w) * self.thumb_offset;
        out.push(Primitive::Fill {
            rect: Box2D {
                cx: thumb_left + thumb_w / 2.0,
                cy: self.bounds.cy,
                w: thumb_w.max(0.0),
                h,
                rotation: 0.0,
            },
            color: theme::SLIDER_FILL,
            opacity: 0.9,
        });
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rst_render::glass::Surface;

    fn screen(w: f64, h: f64) -> Box2D {
        Box2D {
            cx: w / 2.0,
            cy: h / 2.0,
            w,
            h,
            rotation: 0.0,
        }
    }

    fn card(hwnd: usize, title: &str, slot: Option<usize>) -> StripCard {
        StripCard {
            hwnd,
            title: title.to_string(),
            icon: None,
            thumb: None,
            slot,
        }
    }

    fn cards(count: usize) -> Vec<StripCard> {
        (0..count)
            .map(|i| card(i + 1, &format!("окно {i}"), None))
            .collect()
    }

    // ==== Ленты по размеру содержимого (живой репорт 2026-08-25) ====

    #[test]
    fn the_window_strip_is_no_wider_than_its_cards_need() {
        // Живой репорт: лента из нескольких окон растягивалась во весь экран
        // и выглядела пустой полосой. Ширина обязана считаться по карточкам.
        let scr = screen(1920.0, 1080.0);
        let few = build(&cards(3), 0, scr);
        let many = build(&cards(12), 0, scr);
        assert!(
            few.panel.frame().w < many.panel.frame().w,
            "лента из трёх карточек обязана быть уже ленты из двенадцати"
        );
        assert!(
            few.panel.frame().w < scr.w,
            "лента из трёх карточек не должна занимать весь экран"
        );
    }

    #[test]
    fn a_long_window_list_stops_at_the_screen_edge() {
        // Тридцать окон обязаны прокручиваться, а не вылезать за края.
        let scr = screen(1920.0, 1080.0);
        let built = build(&cards(30), 0, scr);
        let f = built.panel.frame();
        assert!(
            f.cx - f.w / 2.0 >= scr.cx - scr.w / 2.0 - 0.5
                && f.cx + f.w / 2.0 <= scr.cx + scr.w / 2.0 + 0.5,
            "лента вылезла за экран: {f:?}"
        );
        assert!(
            built.visible_cards < built.total_cards,
            "лишние карточки обязаны уйти в прокрутку"
        );
    }

    #[test]
    fn an_empty_window_list_still_builds_a_strip_with_the_confirm_button() {
        // Вырожденный случай: окон нет вовсе. Панель обязана собраться и не
        // паниковать.
        let built = build(&[], 0, screen(1920.0, 1080.0));
        assert_eq!(built.total_cards, 0);
        assert!(built.panel.frame().w > 0.0);
    }

    #[test]
    fn every_card_stays_inside_the_strip() {
        // Ширина считается формулой; ошибка в ней выпустила бы последнюю
        // карточку за край панели, и заметить это можно было бы только
        // глазами.
        let scr = screen(1920.0, 1080.0);
        for n in [1usize, 3, 7, 12] {
            let built = build(&cards(n), 0, scr);
            let f = built.panel.frame();
            for i in 0..built.visible_cards {
                let b = built
                    .panel
                    .widget::<Button>(CARD_BASE + i as WidgetId)
                    .map(rst_render::Widget::bounds)
                    .unwrap_or_else(|| panic!("нет карточки {i} при {n} окнах"));
                assert!(
                    b.cx - b.w / 2.0 >= f.cx - f.w / 2.0 - 0.5
                        && b.cx + b.w / 2.0 <= f.cx + f.w / 2.0 + 0.5,
                    "при {n} окнах карточка {i} вылезла за ленту: {b:?}"
                );
            }
        }
    }

    fn texts(panel: &Panel) -> Vec<String> {
        prims(panel)
            .into_iter()
            .filter_map(|p| match p {
                Primitive::Text { text, .. } => Some(text),
                _ => None,
            })
            .collect()
    }

    fn prims(panel: &Panel) -> Vec<Primitive> {
        let mut out = Vec::new();
        panel.draw(&mut out);
        out
    }

    /// Карточки не выходят за экран ни при каком числе карточек и ни при
    /// каком положении скролла: виртуализация строит только видимые, а
    /// остальные уходят в прокрутку, а не за край.
    #[test]
    fn cards_never_leave_the_screen_at_any_count_and_scroll() {
        let scr = screen(1600.0, 900.0);
        let (l, r) = (scr.cx - scr.w / 2.0, scr.cx + scr.w / 2.0);
        let (t, b) = (scr.cy - scr.h / 2.0, scr.cy + scr.h / 2.0);
        for count in [0usize, 1, 3, 10, 40] {
            for scroll in [0usize, 5, 37] {
                let result = build(&cards(count), scroll, scr);
                for prim in prims(&result.panel) {
                    let rect = match &prim {
                        Primitive::Fill { rect, .. }
                        | Primitive::Glass { rect, .. }
                        | Primitive::Icon { rect, .. }
                        | Primitive::Rgba { rect, .. }
                        | Primitive::Text { rect, .. } => *rect,
                    };
                    assert!(
                        rect.cx - rect.w / 2.0 >= l - 1e-9
                            && rect.cx + rect.w / 2.0 <= r + 1e-9
                            && rect.cy - rect.h / 2.0 >= t - 1e-9
                            && rect.cy + rect.h / 2.0 <= b + 1e-9,
                        "примитив {prim:?} вылез за экран (count={count}, scroll={scroll})"
                    );
                }
            }
        }
    }

    /// Цифра слота рисуется на своей карточке: бейдж живёт в углу карточки,
    /// а не где-то ещё в ленте.
    #[test]
    fn slot_digit_is_drawn_on_the_picked_card() {
        let cards = vec![
            card(10, "десять", Some(1)),
            card(20, "двадцать", Some(2)),
            card(30, "тридцать", None),
        ];
        let result = build(&cards, 0, screen(1600.0, 900.0));
        let labels = texts(&result.panel);
        assert!(
            labels.contains(&"1".to_string()),
            "цифра первого слота: {labels:?}"
        );
        assert!(
            labels.contains(&"2".to_string()),
            "цифра второго слота: {labels:?}"
        );
        assert!(
            !labels.contains(&"3".to_string()),
            "неотмеченное окно слота не имеет"
        );
        for (i, digit) in [(0usize, "1"), (1, "2")] {
            let card_bounds = result
                .panel
                .widget::<Button>(CARD_BASE + i as WidgetId)
                .unwrap_or_else(|| panic!("карточка {i}"))
                .bounds();
            let badge_rect = prims(&result.panel)
                .into_iter()
                .find_map(|p| match p {
                    Primitive::Text { rect, text, .. } if text == digit => Some(rect),
                    _ => None,
                })
                .expect("текст бейджа слота");
            assert!(
                badge_rect.cx - badge_rect.w / 2.0 >= card_bounds.cx - card_bounds.w / 2.0
                    && badge_rect.cx + badge_rect.w / 2.0 <= card_bounds.cx + card_bounds.w / 2.0
                    && badge_rect.cy - badge_rect.h / 2.0 >= card_bounds.cy - card_bounds.h / 2.0
                    && badge_rect.cy + badge_rect.h / 2.0 <= card_bounds.cy + card_bounds.h / 2.0,
                "бейдж {digit} внутри карточки {i}: {badge_rect:?}"
            );
        }
    }

    /// Кнопка списка групп стоит в левом углу ленты и не налезает ни на одну
    /// карточку: обе краевые кнопки живут ВНЕ прокручиваемой области, иначе
    /// они уезжали бы вместе с карточками.
    #[test]
    fn the_groups_list_button_sits_at_the_left_edge_clear_of_the_cards() {
        for count in [2usize, 6, 30] {
            let result = build(&cards(count), 0, screen(1600.0, 900.0));
            let button = result
                .panel
                .widget::<Button>(BTN_MANAGER)
                .expect("кнопка списка групп")
                .bounds();
            let confirm = result
                .panel
                .widget::<ConfirmButton>(BTN_CONFIRM)
                .expect("галочка")
                .bounds();
            assert!(
                button.cx < confirm.cx,
                "список слева, подтверждение справа (count={count})"
            );
            for i in 0..count {
                let Some(card) = result.panel.widget::<Button>(CARD_BASE + i as WidgetId) else {
                    continue; // карточка за пределами видимой части ленты
                };
                let card = card.bounds();
                assert!(
                    card.cx - card.w / 2.0 >= button.cx + button.w / 2.0,
                    "карточка {i} налезла на кнопку списка (count={count})"
                );
                assert!(
                    card.cx + card.w / 2.0 <= confirm.cx - confirm.w / 2.0,
                    "карточка {i} налезла на галочку (count={count})"
                );
            }
        }
    }

    /// Группа из одного окна — это просто окно: галочка неактивна при нуле
    /// и одной отметке и активна при двух и больше (тот же критерий, что
    /// `GroupEditor::can_confirm`).
    #[test]
    fn confirm_button_disabled_until_two_windows_are_picked() {
        for (picked, disabled) in [(0usize, true), (1, true), (2, false), (5, false)] {
            let mut cards = cards(6);
            for (i, c) in cards.iter_mut().enumerate() {
                if i < picked {
                    c.slot = Some(i + 1);
                }
            }
            let result = build(&cards, 0, screen(1600.0, 900.0));
            let btn = result
                .panel
                .widget::<ConfirmButton>(BTN_CONFIRM)
                .expect("кнопка подтверждения");
            assert_eq!(btn.disabled, disabled, "отмечено {picked}");
            let center = (btn.bounds.cx, btn.bounds.cy);
            assert_eq!(
                btn.hit_test(center),
                !disabled,
                "неактивная кнопка не отвечает на хит-тест (отмечено {picked})"
            );
        }
    }

    /// Клик по галочке регистрируется только когда она активна: disabled не
    /// армируется и не даёт `take_click`, клик мимо кнопки тоже не считается.
    #[test]
    fn confirm_click_registers_only_when_enabled() {
        let scr = screen(1600.0, 900.0);

        let mut list = cards(3);
        list[0].slot = Some(1);
        let mut result = build(&list, 0, scr);
        let center = {
            let b = result
                .panel
                .widget::<ConfirmButton>(BTN_CONFIRM)
                .unwrap()
                .bounds();
            (b.cx, b.cy)
        };
        result
            .panel
            .pointer_event(PointerEvent::Down { pos: center });
        result.panel.pointer_event(PointerEvent::Up { pos: center });
        assert!(
            !result
                .panel
                .widget_mut::<ConfirmButton>(BTN_CONFIRM)
                .unwrap()
                .take_click(),
            "одна отметка — клик не считается"
        );

        let mut list = cards(3);
        list[0].slot = Some(1);
        list[1].slot = Some(2);
        let mut result = build(&list, 0, scr);
        let center = {
            let b = result
                .panel
                .widget::<ConfirmButton>(BTN_CONFIRM)
                .unwrap()
                .bounds();
            (b.cx, b.cy)
        };
        // Сначала клик мимо (левый край ленты) — не считается.
        result.panel.pointer_event(PointerEvent::Down {
            pos: (10.0, center.1),
        });
        result.panel.pointer_event(PointerEvent::Up {
            pos: (10.0, center.1),
        });
        assert!(
            !result
                .panel
                .widget_mut::<ConfirmButton>(BTN_CONFIRM)
                .unwrap()
                .take_click()
        );
        // Теперь в самую кнопку — считается.
        result
            .panel
            .pointer_event(PointerEvent::Down { pos: center });
        result.panel.pointer_event(PointerEvent::Up { pos: center });
        assert!(
            result
                .panel
                .widget_mut::<ConfirmButton>(BTN_CONFIRM)
                .unwrap()
                .take_click()
        );
    }

    /// Вырожденный экран (уже ленты, нулевой) не паникует и не строит
    /// карточки, которые заведомо не влезают.
    #[test]
    fn degenerate_screen_builds_without_panicking() {
        for scr in [screen(50.0, 40.0), screen(0.0, 0.0), screen(30.0, 300.0)] {
            let mut result = build(&cards(12), 3, scr);
            assert_eq!(result.visible_cards, 0, "экран {scr:?}");
            let _ = prims(&result.panel);
            result.panel.pointer_event(PointerEvent::Down {
                pos: (scr.cx, scr.cy),
            });
            result.panel.pointer_event(PointerEvent::Up {
                pos: (scr.cx, scr.cy),
            });
        }
    }

    /// Полоса прокрутки появляется только когда карточек больше, чем влезает
    /// (тот же регрессионный сценарий, что у `window_pick_list`: длинный
    /// список без намёка на листание выглядит обрезанным).
    #[test]
    fn scrollbar_appears_only_when_cards_overflow_the_strip() {
        let scr = screen(1600.0, 900.0);
        let short = build(&cards(3), 0, scr);
        assert!(
            short.panel.widget::<HScrollBar>(SCROLLBAR_ID).is_none(),
            "3 карточки влезают — полосы быть не должно"
        );
        let long = build(&cards(20), 0, scr);
        assert!(
            long.panel.widget::<HScrollBar>(SCROLLBAR_ID).is_some(),
            "20 карточек не влезают — полоса обязана появиться"
        );
        assert!(long.visible_cards < long.total_cards);
    }

    /// Число видимых карточек соответствует геометрии: слоты укладываются
    /// шагом `CARD_W + CARD_GAP`, последняя заканчивается перед галочкой.
    #[test]
    fn visible_cards_match_the_strip_geometry() {
        let scr = screen(1600.0, 900.0);
        let result = build(&cards(40), 0, scr);
        assert_eq!(result.visible_cards, 10);
        let frame = result.panel.frame();
        let last = result
            .panel
            .widget::<Button>(CARD_BASE + result.visible_cards as WidgetId - 1)
            .unwrap()
            .bounds();
        assert!(
            last.cx + last.w / 2.0 < frame.cx + frame.w / 2.0 - STRIP_PAD,
            "последняя видимая карточка не лезет под галочку: {last:?}"
        );
    }

    /// Длинный заголовок усекается многоточием до ширины карточки — текст
    /// произвольной длины не должен рисоваться поверх соседних карточек.
    #[test]
    fn long_title_is_truncated_to_the_card_width() {
        let c = card(1, &"очень-длинный-заголовок-окна-".repeat(6), None);
        let result = build(std::slice::from_ref(&c), 0, screen(1600.0, 900.0));
        // Усечение режет длинное слово — ищем по началу, а не по «заголовок».
        let label = texts(&result.panel)
            .into_iter()
            .find(|t| t.starts_with("очень"))
            .expect("подпись карточки");
        assert!(label.ends_with("..."), "{label}");
        assert!(text_size(&label).0 <= CARD_W - 2.0 * CARD_PAD);
    }

    /// Снимок окна вписывается в слот буквбоксингом (пропорции сохраняются)
    /// и несёт ключ текстуры и размеры, заданные вызывающим кодом.
    #[test]
    fn thumbnail_is_letterboxed_into_the_card_slot() {
        let mut c = card(1, "окно", None);
        c.thumb = Some(StripImage {
            key: 77,
            width: 200,
            height: 100,
            rgba: vec![0; 200 * 100 * 4],
        });
        let result = build(std::slice::from_ref(&c), 0, screen(1600.0, 900.0));
        // Среди растров есть и скруглённые углы панели (8×8) — снимок
        // опознаётся по исходным размерам растра.
        let (rect, key, width, height) = prims(&result.panel)
            .into_iter()
            .find_map(|p| match p {
                Primitive::Rgba {
                    rect,
                    key,
                    width,
                    height,
                    ..
                } if width == 200 && height == 100 => Some((rect, key, width, height)),
                _ => None,
            })
            .expect("снимок окна на карточке");
        assert_eq!(key, 77);
        assert_eq!((width, height), (200, 100));
        // Слот 108×44, кадр 200×100: масштаб по высоте 0.44, ширина 88.
        assert!((rect.w - 88.0).abs() < 1e-9, "ширина {rect:?}");
        assert!((rect.h - 44.0).abs() < 1e-9, "высота {rect:?}");
        let card_bounds = result.panel.widget::<Button>(CARD_BASE).unwrap().bounds();
        assert_eq!(rect.cx, card_bounds.cx);
        assert!(
            (rect.cy - (card_bounds.cy - CARD_H / 2.0 + CARD_PAD + THUMB_H / 2.0)).abs() < 1e-9,
            "по центру слота снимка: {rect:?}"
        );
    }

    /// Карточка без снимка и иконки рисует утопленный жёлоб-плейсхолдер на
    /// месте снимка (тот же приём, что плейсхолдер иконки в
    /// `window_pick_list`).
    #[test]
    fn card_without_image_draws_a_placeholder_slot() {
        let result = build(&[card(1, "окно", None)], 0, screen(1600.0, 900.0));
        let placeholder = prims(&result.panel)
            .into_iter()
            .find_map(|p| match p {
                Primitive::Glass {
                    rect,
                    surface: Surface::Sunken,
                    ..
                } if rect.w == CARD_W - 2.0 * CARD_PAD && rect.h == THUMB_H => Some(rect),
                _ => None,
            })
            .expect("плейсхолдер слота снимка");
        let card_bounds = result.panel.widget::<Button>(CARD_BASE).unwrap().bounds();
        assert_eq!(placeholder.cx, card_bounds.cx);
    }

    /// Отмеченная карточка загорается стеклом ControlOn и несёт бейдж слота:
    /// тело карточки и бейдж — два стеклянных примитива ControlOn (§2.2,
    /// §7), неотмеченная остаётся стеклом Card.
    #[test]
    fn picked_card_lights_up_with_glass_and_badge() {
        let mut c = card(1, "окно", None);
        c.slot = Some(1);
        let result = build(std::slice::from_ref(&c), 0, screen(1600.0, 900.0));
        let card_bounds = result.panel.widget::<Button>(CARD_BASE).unwrap().bounds();
        let on = prims(&result.panel)
            .into_iter()
            .filter_map(|p| match p {
                Primitive::Glass {
                    rect,
                    surface: Surface::ControlOn,
                    ..
                } => Some(rect),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(on.len(), 2, "тело карточки + бейдж слота");
        assert!(
            on.iter()
                .all(|r| r.cx - r.w / 2.0 >= card_bounds.cx - card_bounds.w / 2.0
                    && r.cx + r.w / 2.0 <= card_bounds.cx + card_bounds.w / 2.0
                    && r.cy - r.h / 2.0 >= card_bounds.cy - card_bounds.h / 2.0
                    && r.cy + r.h / 2.0 <= card_bounds.cy + card_bounds.h / 2.0),
            "бейдж и тело внутри карточки"
        );
        assert!(
            texts(&result.panel).contains(&"1".to_string()),
            "цифра слота на бейдже"
        );
        let plain = build(&[card(1, "окно", None)], 0, screen(1600.0, 900.0));
        let on = prims(&plain.panel)
            .into_iter()
            .filter(|p| {
                matches!(
                    p,
                    Primitive::Glass {
                        surface: Surface::ControlOn,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(on, 0, "неотмеченная карточка — обычное стекло Card");
    }

    /// Оба штриха галочки лежат внутри квадрата содержимого кнопки и идут
    /// в разные стороны: первый — вниз-вправо, второй — вверх-вправо.
    #[test]
    fn checkmark_strokes_are_two_rotated_segments_inside_the_button() {
        let content = Box2D {
            cx: 0.0,
            cy: 0.0,
            w: 16.0,
            h: 16.0,
            rotation: 0.0,
        };
        let [first, second] = checkmark_strokes(content);
        for s in [first, second] {
            assert!(s.cx - s.w / 2.0 >= -8.0 && s.cx + s.w / 2.0 <= 8.0, "{s:?}");
            assert!(s.cy - s.h / 2.0 >= -8.0 && s.cy + s.h / 2.0 <= 8.0, "{s:?}");
        }
        assert!(
            first.rotation > 0.0,
            "левый штрих спускается вниз: {first:?}"
        );
        assert!(
            second.rotation < 0.0,
            "правый штрих поднимается вверх: {second:?}"
        );
    }

    /// Галочка стоит у правого края ленты, по вертикали по центру.
    #[test]
    fn confirm_button_sits_at_the_strip_right_edge() {
        let result = build(&cards(2), 0, screen(1600.0, 900.0));
        let btn = result
            .panel
            .widget::<ConfirmButton>(BTN_CONFIRM)
            .unwrap()
            .bounds();
        let frame = result.panel.frame();
        assert!(
            btn.cx + btn.w / 2.0 <= frame.cx + frame.w / 2.0 - 1e-9,
            "галочка в пределах ленты"
        );
        assert!(
            (btn.cx + btn.w / 2.0 - (frame.cx + frame.w / 2.0 - STRIP_PAD)).abs() < 1e-9,
            "галочка прижата к правому краю с отступом"
        );
        assert!((btn.cy - frame.cy).abs() < 1e-9, "по вертикали по центру");
    }

    /// Лента прижата к нижнему краю экрана с отступом [`SCREEN_MARGIN`].
    #[test]
    fn strip_sits_at_the_bottom_of_the_screen() {
        let scr = screen(1600.0, 900.0);
        let result = build(&cards(1), 0, scr);
        let frame = result.panel.frame();
        assert!(
            (frame.cy + frame.h / 2.0 - (scr.cy + scr.h / 2.0 - SCREEN_MARGIN)).abs() < 1e-9,
            "низ ленты на SCREEN_MARGIN выше нижнего края экрана"
        );
    }

    /// Пустая лента — не панель-призрак: галочка (неактивная) на месте,
    /// карточек нет, полосы прокрутки нет.
    #[test]
    fn empty_strip_shows_only_a_disabled_confirm_button() {
        let result = build(&[], 0, screen(1600.0, 900.0));
        assert_eq!(result.total_cards, 0);
        assert!(result.panel.widget::<Button>(CARD_BASE).is_none());
        assert!(result.panel.widget::<HScrollBar>(SCROLLBAR_ID).is_none());
        assert!(
            result
                .panel
                .widget::<ConfirmButton>(BTN_CONFIRM)
                .unwrap()
                .disabled
        );
    }
}
