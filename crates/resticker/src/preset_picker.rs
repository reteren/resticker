//! Панель быстрого переключения пресетов у курсора (M7, SPEC.md §3.8
//! «Загрузить пресет»; ROADMAP.md — «быстрое переключение… из панели
//! редактирования»).
//!
//! Модальный (для мыши и клавиатуры) попап-список — тем же паттерном, что
//! `confirm_dialog.rs`: открывается кнопкой `cursor_panel::BTN_PRESETS`,
//! закрывается кликом по пресету (применяет его), кликом мимо или `Esc`
//! (без изменений). В отличие от `window_picker.rs` — плоский список без
//! чекбоксов/групп/скролла: пресетов обычно немного, а панель — не
//! редактор одного стикера, а просто список действий.

use rst_core::model::Preset;
use rst_render::{Box2D, Button, ButtonContent, Panel, WidgetId, theme};

use crate::window_picker::truncate_to_width;

/// Идентификатор панели. Диапазон 400+: тулбар 0-8, панель у курсора 100+,
/// панель выбора окон 200+.
pub const PANEL_ID: WidgetId = 400;
/// Первая строка списка пресетов; `ROW_BASE + индекс` в `Config.presets`.
pub const ROW_BASE: WidgetId = 401;

/// Ширина панели, DIP.
pub const WIDTH: f64 = 220.0;
/// Внутренний отступ, DIP.
const PAD: f64 = 6.0;
/// Зазор между строками, DIP.
const ROW_GAP: f64 = 4.0;

/// Высота панели под `count` пресетов. Вызывающий код (`open_preset_picker`
/// в overlay_manager.rs) не открывает панель при пустом списке (кнопка шлёт
/// уведомление вместо этого) — `count == 0` здесь не встречается на
/// практике, но не паникует (даёт высоту одной пустой строки).
pub fn height(count: usize) -> f64 {
    let rows = count.max(1) as f64;
    2.0 * PAD + rows * theme::BUTTON_SIZE + (rows - 1.0) * ROW_GAP
}

/// Собрать панель: один кликабельный ряд на пресет, сверху вниз в порядке
/// `presets` (тот же порядок, что `Config.presets` — вызывающий код
/// декодирует клик по `id - ROW_BASE` обратно в индекс этого среза).
pub fn build(presets: &[Preset], frame: Box2D) -> Panel {
    let mut panel = Panel::new(PANEL_ID, frame);
    let top = frame.cy - frame.h / 2.0 + PAD;
    let row_w = frame.w - 2.0 * PAD;
    for (i, preset) in presets.iter().enumerate() {
        let cy = top + i as f64 * (theme::BUTTON_SIZE + ROW_GAP) + theme::BUTTON_SIZE / 2.0;
        let label = truncate_to_width(&preset.name, row_w - 2.0 * theme::BUTTON_PAD);
        panel.add_widget(Button::new(
            ROW_BASE + i as WidgetId,
            Box2D {
                cx: frame.cx,
                cy,
                w: row_w,
                h: theme::BUTTON_SIZE,
                rotation: 0.0,
            },
            ButtonContent::Label(label),
        ));
    }
    panel
}

#[cfg(test)]
mod tests {
    use super::*;
    use rst_render::{Primitive, Widget};
    use uuid::Uuid;

    fn preset(name: &str) -> Preset {
        Preset {
            id: Uuid::new_v4(),
            name: name.to_string(),
            stickers: Vec::new(),
        }
    }

    fn frame(h: f64) -> Box2D {
        Box2D {
            cx: 200.0,
            cy: 300.0,
            w: WIDTH,
            h,
            rotation: 0.0,
        }
    }

    fn labels(panel: &Panel) -> Vec<String> {
        let mut out = Vec::new();
        panel.draw(&mut out);
        out.into_iter()
            .filter_map(|p| match p {
                Primitive::Text { text, .. } => Some(text),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn empty_list_builds_no_rows() {
        let panel = build(&[], frame(height(0)));
        assert!(labels(&panel).is_empty());
        assert!(panel.widget::<Button>(ROW_BASE).is_none());
    }

    #[test]
    fn one_row_per_preset_in_order() {
        let presets = vec![preset("Работа"), preset("Стрим"), preset("Ночь")];
        let panel = build(&presets, frame(height(presets.len())));
        assert_eq!(labels(&panel), vec!["Работа", "Стрим", "Ночь"]);
        for i in 0..3 {
            assert!(
                panel.widget::<Button>(ROW_BASE + i as WidgetId).is_some(),
                "строка {i} должна существовать"
            );
        }
        assert!(panel.widget::<Button>(ROW_BASE + 3).is_none());
    }

    #[test]
    fn rows_stack_top_to_bottom_inside_frame() {
        let presets = vec![preset("A"), preset("B")];
        let f = frame(height(presets.len()));
        let panel = build(&presets, f);
        let b0 = panel.widget::<Button>(ROW_BASE).unwrap().bounds();
        let b1 = panel.widget::<Button>(ROW_BASE + 1).unwrap().bounds();
        assert!(b1.cy > b0.cy, "вторая строка ниже первой");
        assert!(
            b0.cy - b0.h / 2.0 >= f.cy - f.h / 2.0,
            "первая строка в рамке"
        );
        assert!(
            b1.cy + b1.h / 2.0 <= f.cy + f.h / 2.0,
            "последняя строка в рамке"
        );
    }

    #[test]
    fn long_name_is_truncated_to_row_width() {
        let presets = vec![preset(&"очень-длинное-имя-пресета-".repeat(10))];
        let panel = build(&presets, frame(height(presets.len())));
        let text = &labels(&panel)[0];
        assert!(text.ends_with("..."), "длинное имя усечено многоточием");
        assert!(text.len() < presets[0].name.len());
    }

    #[test]
    fn height_grows_with_preset_count() {
        assert!(height(3) > height(1));
        assert_eq!(
            height(0),
            height(1),
            "нулевой список не даёт нулевую/отрицательную высоту"
        );
    }
}
