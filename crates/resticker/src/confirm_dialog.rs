//! Модал подтверждения удаления (SPEC.md, раздел 2: «Вы точно хотите удалить
//! этот стикер?»; ROADMAP.md M2 «Диалог удаления с „Больше не спрашивать“»).
//!
//! Чистая вёрстка immediate-mode виджетов rst-render: координатор собирает
//! диалог по количеству удаляемых стикеров ([`build`]), рисует его через
//! `Panel::draw`, а нажатия опрашивает через `Panel::widget_mut` +
//! `Button::take_click` / `Checkbox::take_changed`. Backend подтверждения —
//! `rst_core::ops` (`should_confirm_delete` / `suppress_delete_confirmation`);
//! этот модуль его не трогает.
//!
//! Оформление — Dark Liquid Glass (docs/DESIGN_LIQUID_GLASS.md): корпус —
//! плита чёрного стекла с радиусом окна (§3 `RADIUS_WINDOW`), кнопки —
//! стекло с фазами наведения/нажатия (кнопки rst-render анимируют сами).
//! Подпись опасного действия — единственный цвет интерфейса [`DANGER`]
//! (§2.4), фон при этом обычный: красная кнопка целиком кричала бы на весь
//! экран.
//!
//! Ширина модала считается по содержимому, а не задана константой: подписи
//! кнопок раньше не помещались (репорт пользователя 2026-08-23 — «Canc»
//! вместо «Cancel»), потому что ряд кнопок верстался с ошибкой в
//! арифметике, а размеры были подобраны под старый растровый шрифт.

use rst_render::{
    Box2D, Button, ButtonContent, Checkbox, LINE_HEIGHT, Label, Panel, Primitive, Widget, WidgetId,
    glass, text_size, theme,
};

/// Идентификатор самой панели-модала.
pub const ID_DIALOG: WidgetId = 100;
/// Идентификатор надписи-сообщения («Удалить N стикеров?»).
pub const ID_MESSAGE: WidgetId = 101;
/// Идентификатор тумблера «Больше не спрашивать».
pub const ID_DONT_ASK: WidgetId = 102;
/// Идентификатор кнопки «Отмена».
pub const ID_CANCEL: WidgetId = 103;
/// Идентификатор кнопки «Удалить».
pub const ID_DELETE: WidgetId = 104;
/// Идентификатор подписи рядом с тумблером.
const ID_DONT_ASK_LABEL: WidgetId = 105;

/// Минимальная ширина модала, DIP (шире — если не влезает содержимое).
pub const DIALOG_MIN_W: f64 = 320.0;
/// Зазор между чекбоксом и его подписью, DIP. В §3 токена для зазора
/// «контрол — подпись» нет, оставлена именованной константой модуля.
const CHECK_GAP: f64 = 8.0;
/// Минимальная ширина кнопки, DIP (`min-width: 70px` у `.button` старых
/// настроек). В §3 токена нет, оставлена именованной константой модуля.
pub const BUTTON_MIN_W: f64 = 76.0;

/// Подпись тумблера «Больше не спрашивать».
pub const DONT_ASK_LABEL: &str = "Don't ask again";
/// Подпись кнопки удаления.
pub const DELETE_LABEL: &str = "Delete";
/// Подпись кнопки отмены.
pub const CANCEL_LABEL: &str = "Cancel";

/// §2.4 `DANGER` — единственный цвет во всём интерфейсе (#D0463C), только
/// тонкие предупреждения. Псевдоним общего токена: константа заведена в
/// `rst_render::theme`, дублировать её по модулям нельзя (§9.1).
use rst_render::theme::DANGER;

/// Текст сообщения для `count` удаляемых стикеров.
pub fn message_for(count: u32) -> String {
    if count == 1 {
        "Delete 1 sticker?".to_string()
    } else {
        format!("Delete {count} stickers?")
    }
}

/// Ширина кнопки под подпись `label`: подпись плюс горизонтальный отступ
/// подписи в кнопке (§3 `PAD_CTRL_X`) с каждой стороны, но не уже
/// [`BUTTON_MIN_W`].
fn button_width(label: &str) -> f64 {
    (text_size(label).0 + 2.0 * theme::PAD_CTRL_X).max(BUTTON_MIN_W)
}

/// Размер модала (ширина, высота) в DIP для `count` удаляемых стикеров:
/// ширина — по самому широкому ряду, высота — по трём рядам с отступами.
pub fn dialog_size(count: u32) -> (f64, f64) {
    let message_w = text_size(&message_for(count)).0;
    let check_row_w = theme::CHECKBOX_SIZE + CHECK_GAP + text_size(DONT_ASK_LABEL).0;
    let buttons_w = button_width(CANCEL_LABEL) + theme::GAP_ROW + button_width(DELETE_LABEL);
    let content_w = message_w.max(check_row_w).max(buttons_w);
    let w = (content_w + 2.0 * theme::PAD_PANEL).max(DIALOG_MIN_W);
    let check_row_h = theme::CHECKBOX_SIZE.max(LINE_HEIGHT);
    let h = 2.0 * theme::PAD_PANEL
        + LINE_HEIGHT
        + theme::GAP_ROW
        + check_row_h
        + theme::GAP_ROW
        + theme::BUTTON_SIZE;
    (w, h)
}

/// Собрать модал подтверждения удаления `count` стикеров, центрированный
/// в точке `center` (DIP, ADR-010). Вёрстка сверху вниз: сообщение по центру,
/// тумблер «не спрашивать» у левого края, ряд кнопок справа снизу
/// (`Delete` — крайняя справа, SPEC.md: «[Да] [Отмена]»).
pub fn build(count: u32, center: (f64, f64)) -> Panel {
    let (cx, cy) = center;
    let (dialog_w, dialog_h) = dialog_size(count);
    let left = cx - dialog_w / 2.0;
    let right = cx + dialog_w / 2.0;
    let top = cy - dialog_h / 2.0;

    let mut panel = Panel::new(
        ID_DIALOG,
        Box2D {
            cx,
            cy,
            w: dialog_w,
            h: dialog_h,
            rotation: 0.0,
        },
    )
    // Радиус окна (§3): модал — большая панель, а не мелкий контрол.
    .with_corner_radius(theme::RADIUS_WINDOW)
    .with_surface(glass::Surface::Modal);

    // Сообщение — по центру модала, обычной надписью: это текст, а не
    // кнопка (раньше оно было Label-кнопкой и рисовалось с фоном кнопки,
    // из-за чего выглядело нажимаемым).
    let message = message_for(count);
    let message_w = text_size(&message).0;
    let message_cy = top + theme::PAD_PANEL + LINE_HEIGHT / 2.0;
    panel.add_widget(Label::new(
        ID_MESSAGE,
        cx - message_w / 2.0,
        message_cy,
        &message,
    ));

    // «Больше не спрашивать» — настоящий чекбокс с подписью справа. Подпись
    // второстепенная: белый свет на `TEXT_DIM_OPACITY` (§2.3) — отдельных
    // серых цветов больше нет.
    let check_row_h = theme::CHECKBOX_SIZE.max(LINE_HEIGHT);
    let check_cy = message_cy + LINE_HEIGHT / 2.0 + theme::GAP_ROW + check_row_h / 2.0;
    panel.add_widget(Checkbox::standard(
        ID_DONT_ASK,
        left + theme::PAD_PANEL + theme::CHECKBOX_SIZE / 2.0,
        check_cy,
        false,
    ));
    panel.add_widget(DimLabel::new(
        ID_DONT_ASK_LABEL,
        left + theme::PAD_PANEL + theme::CHECKBOX_SIZE + CHECK_GAP,
        check_cy,
        DONT_ASK_LABEL,
    ));

    // Ряд кнопок — справа, снизу; Delete крайняя справа.
    let buttons_cy = check_cy + check_row_h / 2.0 + theme::GAP_ROW + theme::BUTTON_SIZE / 2.0;
    let delete_w = button_width(DELETE_LABEL);
    let cancel_w = button_width(CANCEL_LABEL);
    let delete_cx = right - theme::PAD_PANEL - delete_w / 2.0;
    // Полная ширина соседней кнопки, а не половина: старая формула вычитала
    // только половину ширины «Cancel» и клала кнопки друг на друга — «Delete»
    // рисовался поверх и срезал подпись до «Canc».
    let cancel_cx = delete_cx - delete_w / 2.0 - theme::GAP_ROW - cancel_w / 2.0;

    panel.add_widget(dialog_button(
        ID_CANCEL,
        cancel_cx,
        buttons_cy,
        cancel_w,
        CANCEL_LABEL,
    ));
    // Delete — подтверждающая кнопка модала: она ведёт диалог, поэтому
    // первичная (§2.2 `CTRL_BG_PRIMARY`, ярче «Отмены»). Опасность действия
    // — цветом подписи, а не фоном.
    panel.add_widget(
        dialog_button(ID_DELETE, delete_cx, buttons_cy, delete_w, DELETE_LABEL)
            .primary()
            .with_label_color(DANGER),
    );

    panel
}

/// Кнопка модала: фиксированная высота — сторона квадратной кнопки тулбара
/// (§3 `BUTTON_SIZE`, общая высота всех контролов этого размера), подпись
/// по центру. Материал и фазы наведения/нажатия рисует сама кнопка.
fn dialog_button(id: WidgetId, cx: f64, cy: f64, w: f64, label: &str) -> Button {
    Button::new(
        id,
        Box2D {
            cx,
            cy,
            w,
            h: theme::BUTTON_SIZE,
            rotation: 0.0,
        },
        ButtonContent::Label(label.to_string()),
    )
}

/// Второстепенная подпись: белый текст на [`theme::TEXT_DIM_OPACITY`] вместо
/// отдельного серого цвета — «цвет» интерфейса один, свет разной силы (§2.3).
/// Своя, а не [`Label`] с `set_dim`: у `Label` приглушение зашито числом 0.5,
/// а здесь непрозрачность — из токена темы.
struct DimLabel {
    id: WidgetId,
    rect: Box2D,
    text: String,
}

impl DimLabel {
    /// Подпись с левым краем в `left` и центром по вертикали в `cy` — та же
    /// геометрия, что у [`Label`].
    fn new(id: WidgetId, left: f64, cy: f64, text: &str) -> Self {
        let (tw, _) = text_size(text);
        Self {
            id,
            rect: Box2D {
                cx: left + tw / 2.0,
                cy,
                w: tw,
                h: LINE_HEIGHT,
                rotation: 0.0,
            },
            text: text.to_string(),
        }
    }
}

impl Widget for DimLabel {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn bounds(&self) -> Box2D {
        self.rect
    }

    fn set_bounds(&mut self, bounds: Box2D) {
        self.rect = bounds;
    }

    /// Не интерактивна — клики/hover сквозь неё, как у [`Label`].
    fn hit_test(&self, _pos: (f64, f64)) -> bool {
        false
    }

    fn draw(&self, out: &mut Vec<Primitive>) {
        out.push(Primitive::Text {
            rect: self.rect,
            text: self.text.clone(),
            color: theme::TEXT,
            opacity: theme::TEXT_DIM_OPACITY,
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
    use rst_render::{PointerEvent, Primitive, Widget};

    fn bounds_of<W: 'static + Widget>(panel: &Panel, id: WidgetId) -> Box2D {
        panel
            .widget::<W>(id)
            .expect("виджет с данным id существует")
            .bounds()
    }

    fn button_bounds(panel: &Panel, id: WidgetId) -> Box2D {
        bounds_of::<Button>(panel, id)
    }

    /// Все строки, которые панель отдаёт к отрисовке, в порядке отрисовки.
    fn texts(panel: &Panel) -> Vec<String> {
        let mut out = Vec::new();
        panel.draw(&mut out);
        out.iter()
            .filter_map(|p| match p {
                Primitive::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn message_matches_count() {
        assert_eq!(message_for(1), "Delete 1 sticker?");
        assert_eq!(message_for(7), "Delete 7 stickers?");
    }

    #[test]
    fn button_labels_fit_inside_their_buttons() {
        // Регрессия на репорт «текст не вмещается на кнопках»: подпись
        // должна помещаться в кнопку вместе с внутренними отступами
        // конвейера отрисовки (`theme::BUTTON_PAD` с каждой стороны).
        let panel = build(1, (500.0, 400.0));
        for (id, label) in [(ID_CANCEL, CANCEL_LABEL), (ID_DELETE, DELETE_LABEL)] {
            let b = button_bounds(&panel, id);
            let (tw, _) = text_size(label);
            assert!(
                tw <= b.w - 2.0 * theme::BUTTON_PAD,
                "подпись {label:?} ({tw} DIP) не влезает в кнопку шириной {}",
                b.w
            );
        }
        assert!(texts(&panel).iter().any(|t| t == CANCEL_LABEL));
        assert!(texts(&panel).iter().any(|t| t == DELETE_LABEL));
    }

    #[test]
    fn buttons_do_not_overlap_and_delete_is_rightmost() {
        // Та же регрессия с другой стороны: кнопки лежали друг на друге.
        let panel = build(1, (500.0, 400.0));
        let cancel = button_bounds(&panel, ID_CANCEL);
        let delete = button_bounds(&panel, ID_DELETE);
        assert!(
            cancel.cx + cancel.w / 2.0 <= delete.cx - delete.w / 2.0,
            "кнопки перекрываются: cancel {cancel:?}, delete {delete:?}"
        );
        assert!(
            (delete.cx - delete.w / 2.0) - (cancel.cx + cancel.w / 2.0) - theme::GAP_ROW < 1e-9,
            "между кнопками ровно GAP_ROW"
        );
        assert_eq!(cancel.cy, delete.cy, "кнопки в одном ряду");
    }

    #[test]
    fn everything_fits_inside_the_dialog() {
        for count in [1, 12, 999] {
            let panel = build(count, (500.0, 400.0));
            let f = panel.frame();
            let (l, r) = (f.cx - f.w / 2.0, f.cx + f.w / 2.0);
            let (t, b) = (f.cy - f.h / 2.0, f.cy + f.h / 2.0);
            let mut prims = Vec::new();
            panel.draw(&mut prims);
            for prim in &prims {
                let rect = match prim {
                    Primitive::Fill { rect, .. }
                    | Primitive::Glass { rect, .. }
                    | Primitive::Icon { rect, .. }
                    | Primitive::Rgba { rect, .. }
                    | Primitive::Text { rect, .. } => rect,
                };
                assert!(
                    rect.cx - rect.w / 2.0 >= l - 1e-9
                        && rect.cx + rect.w / 2.0 <= r + 1e-9
                        && rect.cy - rect.h / 2.0 >= t - 1e-9
                        && rect.cy + rect.h / 2.0 <= b + 1e-9,
                    "примитив {rect:?} вылез за модал {f:?} (count={count})"
                );
            }
        }
    }

    #[test]
    fn dialog_grows_with_a_long_message() {
        let (narrow, h1) = dialog_size(1);
        let (wide, h2) = dialog_size(999_999);
        assert!(wide >= narrow, "длинное сообщение не сужает модал");
        assert_eq!(h1, h2, "высота от числа стикеров не зависит");
        assert!(narrow >= DIALOG_MIN_W);
    }

    #[test]
    fn rows_are_stacked_top_down() {
        let panel = build(3, (500.0, 400.0));
        let message = bounds_of::<Label>(&panel, ID_MESSAGE);
        let check = bounds_of::<Checkbox>(&panel, ID_DONT_ASK);
        let delete = button_bounds(&panel, ID_DELETE);
        assert!(message.cy < check.cy, "сообщение выше тумблера");
        assert!(check.cy < delete.cy, "тумблер выше кнопок");
        let label = bounds_of::<DimLabel>(&panel, ID_DONT_ASK_LABEL);
        assert_eq!(label.cy, check.cy, "подпись на одной строке с тумблером");
        assert!(label.cx > check.cx, "подпись справа от тумблера");
    }

    #[test]
    fn checkbox_toggles_on_click() {
        let mut panel = build(1, (500.0, 400.0));
        let b = bounds_of::<Checkbox>(&panel, ID_DONT_ASK);
        panel.pointer_event(PointerEvent::Down { pos: (b.cx, b.cy) });
        panel.pointer_event(PointerEvent::Up { pos: (b.cx, b.cy) });
        let check = panel
            .widget_mut::<Checkbox>(ID_DONT_ASK)
            .expect("тумблер на месте");
        assert_eq!(check.take_changed(), Some(true), "клик включает тумблер");
        assert_eq!(check.take_changed(), None, "событие одноразовое");
    }

    #[test]
    fn click_inside_the_dialog_does_not_reach_the_scene() {
        let panel = build(1, (500.0, 400.0));
        let f = panel.frame();
        assert!(panel.hit_test((f.cx, f.cy)), "центр модала");
        assert!(
            !panel.hit_test((f.cx, f.cy - f.h / 2.0 - 10.0)),
            "выше модала — мимо"
        );
    }

    #[test]
    fn delete_label_is_red_and_cancel_is_not() {
        // Опасное действие подсвечено цветом подписи, а не фоном.
        let panel = build(1, (500.0, 400.0));
        let mut prims = Vec::new();
        panel.draw(&mut prims);
        let color_of = |label: &str| {
            prims.iter().find_map(|p| match p {
                Primitive::Text { text, color, .. } if text == label => Some(*color),
                _ => None,
            })
        };
        assert_eq!(color_of(DELETE_LABEL), Some(DANGER));
        assert_eq!(color_of(CANCEL_LABEL), Some(theme::TEXT));
    }
}
