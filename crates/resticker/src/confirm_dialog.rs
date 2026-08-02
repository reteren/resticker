//! Модал подтверждения удаления (SPEC.md, раздел 2: «Вы точно хотите удалить
//! этот стикер?»; ROADMAP.md M2 «Диалог удаления с „Больше не спрашивать“»).
//!
//! Чистая вёрстка immediate-mode виджетов rst-render: координатор собирает
//! диалог по количеству удаляемых стикеров ([`build`]), рисует его через
//! `Panel::draw`, а нажатия опрашивает через `Panel::widget_mut::<Button>` +
//! [`Button::take_click`]. Backend подтверждения — `rst_core::ops`
//! (`should_confirm_delete` / `suppress_delete_confirmation`); этот модуль его
//! не трогает.
//!
//! Замечание о шрифте: встроенный битовый шрифт rst-render (text.rs) пока
//! содержит только цифры и символы — буквы рисуются контурным квадратом.
//! Подписи здесь — осмысленные строки-данные; их глифы добавит отдельная
//! задача шрифта.

use rst_render::{Box2D, Button, ButtonContent, Panel, WidgetId, text_size};

/// Идентификатор самой панели-модала.
pub const ID_DIALOG: WidgetId = 100;
/// Идентификатор кнопки-сообщения («Удалить N стикеров?»).
pub const ID_MESSAGE: WidgetId = 101;
/// Идентификатор тумблера «Больше не спрашивать».
pub const ID_DONT_ASK: WidgetId = 102;
/// Идентификатор кнопки «Отмена».
pub const ID_CANCEL: WidgetId = 103;
/// Идентификатор кнопки «Удалить».
pub const ID_DELETE: WidgetId = 104;

/// Ширина модала, DIP.
pub const DIALOG_W: f64 = 320.0;
/// Высота модала, DIP.
pub const DIALOG_H: f64 = 120.0;
/// Внутренний отступ содержимого от краёв модала, DIP.
pub const PAD: f64 = 16.0;
/// Зазор между кнопками в нижнем ряду, DIP.
pub const GAP: f64 = 8.0;
/// Вертикальный отступ между рядами содержимого, DIP.
pub const ROW_GAP: f64 = 12.0;
/// Высота кнопок с подписью, DIP.
pub const BUTTON_H: f64 = 24.0;
/// Горизонтальный отступ подписи внутри кнопки, DIP.
pub const BUTTON_PAD_X: f64 = 8.0;

/// Подпись тумблера «Больше не спрашивать» (галочка-квадрат — контурный
/// глиф текущего шрифта; состояние «проверено» ведёт координатор).
pub const DONT_ASK_LABEL: &str = "☐ Don't ask again";
/// Подпись кнопки удаления.
pub const DELETE_LABEL: &str = "Delete";
/// Подпись кнопки отмены.
pub const CANCEL_LABEL: &str = "Cancel";

/// Текст сообщения для `count` удаляемых стикеров.
pub fn message_for(count: u32) -> String {
    if count == 1 {
        "Delete 1 sticker?".to_string()
    } else {
        format!("Delete {count} stickers?")
    }
}

/// Собрать модал подтверждения удаления `count` стикеров, центрированный
/// в точке `center` (DIP, ADR-010). Вёрстка сверху вниз: сообщение по центру,
/// тумблер «не спрашивать» у левого края, ряд кнопок справа снизу
/// (`Delete` — крайняя справа, SPEC.md: «[Да] [Отмена]»).
pub fn build(count: u32, center: (f64, f64)) -> Panel {
    let (cx, cy) = center;
    let left = cx - DIALOG_W / 2.0;
    let right = cx + DIALOG_W / 2.0;
    let top = cy - DIALOG_H / 2.0;
    let (_, msg_h) = text_size(&message_for(count));

    let mut panel = Panel::new(
        ID_DIALOG,
        Box2D {
            cx,
            cy,
            w: DIALOG_W,
            h: DIALOG_H,
            rotation: 0.0,
        },
    );

    // Сообщение — по центру модала. Текстового виджета в rst-render пока нет,
    // поэтому сообщение — Label-кнопка; её клики игнорируются координатором.
    panel.add_widget(labeled_button(
        ID_MESSAGE,
        cx,
        top + PAD + msg_h / 2.0,
        message_for(count),
    ));

    // «Больше не спрашивать» — тумблер слева, под сообщением.
    let (toggle_w, _) = text_size(DONT_ASK_LABEL);
    panel.add_widget(labeled_button(
        ID_DONT_ASK,
        left + PAD + toggle_w / 2.0,
        top + PAD + msg_h + ROW_GAP + BUTTON_H / 2.0,
        DONT_ASK_LABEL.to_string(),
    ));

    // Ряд кнопок — справа, снизу; Delete — крайняя справа.
    let (delete_w, _) = text_size(DELETE_LABEL);
    let (cancel_w, _) = text_size(CANCEL_LABEL);
    let buttons_y = top + PAD + msg_h + ROW_GAP + BUTTON_H + ROW_GAP + BUTTON_H / 2.0;
    let delete_cx = right - PAD - (delete_w + 2.0 * BUTTON_PAD_X) / 2.0;
    let cancel_cx = delete_cx - GAP - (cancel_w + 2.0 * BUTTON_PAD_X) / 2.0;

    panel.add_widget(labeled_button(
        ID_CANCEL,
        cancel_cx,
        buttons_y,
        CANCEL_LABEL.to_string(),
    ));
    panel.add_widget(labeled_button(
        ID_DELETE,
        delete_cx,
        buttons_y,
        DELETE_LABEL.to_string(),
    ));

    panel
}

/// Кнопка с текстовой подписью: ширина — по подписи, высота — `BUTTON_H`.
fn labeled_button(id: WidgetId, cx: f64, cy: f64, label: String) -> Button {
    let (text_w, _) = text_size(&label);
    Button::new(
        id,
        Box2D {
            cx,
            cy,
            w: text_w + 2.0 * BUTTON_PAD_X,
            h: BUTTON_H,
            rotation: 0.0,
        },
        ButtonContent::Label(label),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rst_render::{PointerEvent, Primitive, Widget};

    fn center_of(panel: &Panel, id: WidgetId) -> (f64, f64) {
        let b = panel
            .widget::<Button>(id)
            .expect("виджет с данным id существует")
            .bounds();
        (b.cx, b.cy)
    }

    /// Подписи кнопок в порядке отрисовки (поля content у Button нет —
    /// читаем примитивы, которые панель отдаёт к отрисовке).
    fn labels_in_draw_order(panel: &Panel) -> Vec<String> {
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
    fn frame_is_centered_and_sized() {
        let panel = build(3, (400.0, 300.0));
        assert!(panel.hit_test((400.0, 300.0)), "центр модала");
        assert!(
            panel.hit_test((400.0 - DIALOG_W / 2.0, 300.0 - DIALOG_H / 2.0)),
            "верхний левый угол (граница включительна)"
        );
        assert!(
            panel.hit_test((400.0 + DIALOG_W / 2.0, 300.0 + DIALOG_H / 2.0)),
            "нижний правый угол"
        );
        assert!(
            !panel.hit_test((400.0 - DIALOG_W / 2.0 - 1.0, 300.0)),
            "слева от модала"
        );
        assert!(
            !panel.hit_test((400.0, 300.0 + DIALOG_H / 2.0 + 1.0)),
            "ниже модала"
        );
    }

    #[test]
    fn message_centered_toggle_left_aligned() {
        let panel = build(3, (400.0, 300.0));
        let top = 300.0 - DIALOG_H / 2.0;
        let msg_h = rst_render::LINE_HEIGHT;

        let (msg_cx, msg_cy) = center_of(&panel, ID_MESSAGE);
        assert_eq!(msg_cx, 400.0, "сообщение по центру");
        assert_eq!(msg_cy, top + PAD + msg_h / 2.0);

        let (toggle_cx, toggle_cy) = center_of(&panel, ID_DONT_ASK);
        let (toggle_w, _) = text_size(DONT_ASK_LABEL);
        assert_eq!(
            toggle_cx,
            400.0 - DIALOG_W / 2.0 + PAD + toggle_w / 2.0,
            "тумблер у левого края модала"
        );
        assert_eq!(toggle_cy, top + PAD + msg_h + ROW_GAP + BUTTON_H / 2.0);
    }

    #[test]
    fn buttons_bottom_row_delete_rightmost() {
        let panel = build(3, (400.0, 300.0));
        let top = 300.0 - DIALOG_H / 2.0;
        let (_, msg_h) = text_size(&message_for(3));
        let buttons_y = top + PAD + msg_h + ROW_GAP + BUTTON_H + ROW_GAP + BUTTON_H / 2.0;

        let (_, cancel_cy) = center_of(&panel, ID_CANCEL);
        let (_, delete_cy) = center_of(&panel, ID_DELETE);
        assert_eq!(cancel_cy, buttons_y, "Cancel в нижнем ряду");
        assert_eq!(delete_cy, buttons_y, "Delete в нижнем ряду");

        let (cancel_cx, _) = center_of(&panel, ID_CANCEL);
        let (delete_cx, _) = center_of(&panel, ID_DELETE);
        assert!(delete_cx > cancel_cx, "Delete правее Cancel");

        let (delete_w, _) = text_size(DELETE_LABEL);
        let (cancel_w, _) = text_size(CANCEL_LABEL);
        assert_eq!(
            delete_cx,
            400.0 + DIALOG_W / 2.0 - PAD - (delete_w + 2.0 * BUTTON_PAD_X) / 2.0,
            "Delete прижат к правому краю"
        );
        assert_eq!(
            cancel_cx,
            delete_cx - GAP - (cancel_w + 2.0 * BUTTON_PAD_X) / 2.0,
            "Cancel слева от Delete с зазором"
        );
    }

    #[test]
    fn vertical_order_message_toggle_buttons() {
        let panel = build(3, (400.0, 300.0));
        let msg_y = center_of(&panel, ID_MESSAGE).1;
        let toggle_y = center_of(&panel, ID_DONT_ASK).1;
        let buttons_y = center_of(&panel, ID_DELETE).1;
        assert!(msg_y < toggle_y, "сообщение выше тумблера");
        assert!(toggle_y < buttons_y, "тумблер выше кнопок");
    }

    #[test]
    fn labels_in_add_order_with_count_in_message() {
        let panel = build(3, (400.0, 300.0));
        assert_eq!(
            labels_in_draw_order(&panel),
            vec![
                "Delete 3 stickers?".to_string(),
                DONT_ASK_LABEL.to_string(),
                CANCEL_LABEL.to_string(),
                DELETE_LABEL.to_string(),
            ]
        );
    }

    #[test]
    fn draw_emits_frame_then_widget_primitives() {
        let panel = build(3, (400.0, 300.0));
        let mut out = Vec::new();
        panel.draw(&mut out);
        // Рамка (рамка + фон) + 4 кнопки по 2 примитива (фон + подпись).
        assert_eq!(out.len(), 2 + 4 * 2);
    }

    #[test]
    fn click_delete_fires_only_delete() {
        let mut panel = build(3, (400.0, 300.0));
        let (x, y) = center_of(&panel, ID_DELETE);
        assert!(
            panel
                .pointer_event(PointerEvent::Down { pos: (x, y) })
                .consumed
        );
        assert!(
            panel
                .pointer_event(PointerEvent::Up { pos: (x, y) })
                .consumed
        );
        assert!(panel.widget_mut::<Button>(ID_DELETE).unwrap().take_click());
        assert!(!panel.widget_mut::<Button>(ID_CANCEL).unwrap().take_click());
        assert!(
            !panel
                .widget_mut::<Button>(ID_DONT_ASK)
                .unwrap()
                .take_click()
        );
    }

    #[test]
    fn click_cancel_fires_only_cancel() {
        let mut panel = build(3, (400.0, 300.0));
        let (x, y) = center_of(&panel, ID_CANCEL);
        panel.pointer_event(PointerEvent::Down { pos: (x, y) });
        panel.pointer_event(PointerEvent::Up { pos: (x, y) });
        assert!(panel.widget_mut::<Button>(ID_CANCEL).unwrap().take_click());
        assert!(!panel.widget_mut::<Button>(ID_DELETE).unwrap().take_click());
        assert!(
            !panel
                .widget_mut::<Button>(ID_DONT_ASK)
                .unwrap()
                .take_click()
        );
    }

    #[test]
    fn click_dont_ask_fires_toggle() {
        // Тумблер — обычная кнопка; состояние «проверено/нет» ведёт координатор
        // по `take_click`.
        let mut panel = build(3, (400.0, 300.0));
        let (x, y) = center_of(&panel, ID_DONT_ASK);
        panel.pointer_event(PointerEvent::Down { pos: (x, y) });
        panel.pointer_event(PointerEvent::Up { pos: (x, y) });
        assert!(
            panel
                .widget_mut::<Button>(ID_DONT_ASK)
                .unwrap()
                .take_click()
        );
        assert!(!panel.widget_mut::<Button>(ID_DELETE).unwrap().take_click());
    }

    #[test]
    fn click_message_consumed_but_no_action() {
        let mut panel = build(3, (400.0, 300.0));
        let (x, y) = center_of(&panel, ID_MESSAGE);
        assert!(
            panel
                .pointer_event(PointerEvent::Down { pos: (x, y) })
                .consumed,
            "клик по модалу не уходит в сцену"
        );
        panel.pointer_event(PointerEvent::Up { pos: (x, y) });
        assert!(!panel.widget_mut::<Button>(ID_DELETE).unwrap().take_click());
        assert!(!panel.widget_mut::<Button>(ID_CANCEL).unwrap().take_click());
        assert!(
            !panel
                .widget_mut::<Button>(ID_DONT_ASK)
                .unwrap()
                .take_click()
        );
    }

    #[test]
    fn click_outside_modal_not_consumed() {
        let mut panel = build(3, (400.0, 300.0));
        assert!(
            !panel
                .pointer_event(PointerEvent::Down {
                    pos: (1000.0, 900.0)
                })
                .consumed
        );
    }

    #[test]
    fn translate_moves_modal_keeping_layout() {
        let mut panel = build(3, (400.0, 300.0));
        let before_msg = center_of(&panel, ID_MESSAGE);
        let before_delete = center_of(&panel, ID_DELETE);
        panel.translate(50.0, 30.0);
        let after_msg = center_of(&panel, ID_MESSAGE);
        let after_delete = center_of(&panel, ID_DELETE);
        assert_eq!(after_msg.0 - before_msg.0, 50.0);
        assert_eq!(after_msg.1 - before_msg.1, 30.0);
        assert_eq!(after_delete.0 - before_delete.0, 50.0);
        assert_eq!(after_delete.1 - before_delete.1, 30.0);
    }

    #[test]
    fn message_for_singular_and_plural() {
        assert_eq!(message_for(1), "Delete 1 sticker?");
        assert_eq!(message_for(2), "Delete 2 stickers?");
        assert_eq!(message_for(3), "Delete 3 stickers?");
        assert_eq!(message_for(0), "Delete 0 stickers?");
    }
}
