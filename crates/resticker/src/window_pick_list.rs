//! Список открытых окон для закрепления как стикер (M6, SPEC.md §5.1;
//! кнопка `cursor_panel::BTN_ADD_WINDOW`).
//!
//! Плоский кликабельный список — тот же паттерн, что `preset_picker.rs`
//! (один клик по строке применяет действие и закрывает панель), а НЕ
//! панель чекбоксов, как у `window_picker.rs` (та выбирает ПРАВИЛА
//! видимости для уже существующего стикера — другая задача, тот модуль
//! здесь не переиспользуется).
//!
//! Заменяет прежний режим «наведение курсора на реальное окно на экране +
//! клик» (`picking_window`/`pick_hover`/`WindowHighlight` в
//! overlay_manager.rs, до 2026-08-10): пользователь не видел, какое именно
//! окно попадёт под курсор в момент клика, и воспринимал результат как
//! случайный («выбирается какое-то текущее окно само»). Явный список с
//! названиями снимает эту неопределённость — обработка клика по строке
//! (`handle_window_pick_list_up`) живёт в overlay_manager.rs, тем же
//! местом, что у `handle_preset_picker_up`.

use rst_render::{Box2D, Button, ButtonContent, Panel, WidgetId, theme};
use rst_win32::window_enum::WindowInfo;

use crate::window_picker::truncate_to_width;

/// Идентификатор панели. Диапазон 500+: тулбар 0-8, панель у курсора 100+,
/// панель выбора окон (M4) 200+, панель пресетов (M7) 400+.
pub const PANEL_ID: WidgetId = 500;
/// Первая строка списка; `ROW_BASE + индекс` в отсортированном по
/// z-order снимке (`sorted_snapshot`) — тот же индекс, что видит вызывающий
/// код при декодировании клика обратно в `WindowInfo`.
pub const ROW_BASE: WidgetId = 501;

/// Ширина панели, DIP.
pub const WIDTH: f64 = 320.0;
/// Внутренний отступ, DIP.
const PAD: f64 = 6.0;
/// Зазор между строками, DIP.
const ROW_GAP: f64 = 4.0;
/// Сколько строк списка влезает в панель без скролла (виртуализация, тот
/// же приём, что `window_picker::PICKER_VISIBLE_ROWS`).
pub const VISIBLE_ROWS: usize = 10;

/// Высота панели под `visible_count` видимых строк (не общее число окон —
/// список выше `VISIBLE_ROWS` строк не растягивает панель, скроллится).
pub fn height(visible_count: usize) -> f64 {
    let rows = visible_count.clamp(1, VISIBLE_ROWS) as f64;
    2.0 * PAD + rows * theme::BUTTON_SIZE + (rows - 1.0) * ROW_GAP
}

/// Снимок, отсортированный по z-order (как в Alt+Tab, верхнее окно первым)
/// — общий порядок для билдера и для декодирования клика вызывающим кодом
/// (`ROW_BASE + i` → `sorted[i]`), вызывается один раз на оба использования.
pub fn sorted_snapshot(snapshot: &[WindowInfo]) -> Vec<WindowInfo> {
    let mut out: Vec<WindowInfo> = snapshot.to_vec();
    out.sort_by_key(|w| w.z_order);
    out
}

/// Подпись строки: заголовок окна, иначе (пустой заголовок) — имя exe,
/// иначе — заглушка. Окно из кэша трекера всегда должно быть чем-то
/// подписано, иначе строку нечем отличить от соседней.
fn row_label(window: &WindowInfo, max_w: f64) -> String {
    let text = if !window.title.is_empty() {
        window.title.clone()
    } else if let Some(name) = window.exe_path.file_name() {
        name.to_string_lossy().into_owned()
    } else {
        "Без имени".to_string()
    };
    truncate_to_width(&text, max_w)
}

/// Результат [`build`]: панель + общее число строк (для клампа скролла
/// вызывающим кодом, тот же контракт, что `window_picker::PickerPanel`).
pub struct PickListPanel {
    pub panel: Panel,
    pub total_rows: usize,
}

/// Собрать видимый срез списка. `sorted` — [`sorted_snapshot`] (уже
/// отсортированный, чтобы не сортировать на каждый вызов при скролле),
/// `scroll` — сколько строк пропущено сверху, `frame` — рамка панели
/// (типовой размер — [`WIDTH`]×[`height`]`(sorted.len())`, вызывающий код
/// сам решает).
pub fn build(sorted: &[WindowInfo], scroll: usize, frame: Box2D) -> PickListPanel {
    let mut panel = Panel::new(PANEL_ID, frame);
    let top = frame.cy - frame.h / 2.0 + PAD;
    let row_w = frame.w - 2.0 * PAD;
    let max_text_w = row_w - 2.0 * theme::BUTTON_PAD;

    for (i, window) in sorted.iter().enumerate().skip(scroll).take(VISIBLE_ROWS) {
        let visible_index = i - scroll;
        let cy = top + visible_index as f64 * (theme::BUTTON_SIZE + ROW_GAP) + theme::BUTTON_SIZE / 2.0;
        let label = row_label(window, max_text_w);
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

    PickListPanel {
        panel,
        total_rows: sorted.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rst_render::{Primitive, Widget};
    use std::path::PathBuf;

    fn window(exe: &str, title: &str, z: u32) -> WindowInfo {
        WindowInfo {
            hwnd: z as usize + 1,
            rect: Default::default(),
            pid: 1,
            exe_path: if exe.is_empty() {
                PathBuf::new()
            } else {
                PathBuf::from(exe)
            },
            title: title.to_string(),
            class: String::new(),
            z_order: z,
            iconic: false,
            icon: None,
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
    fn empty_snapshot_builds_no_rows() {
        let sorted = sorted_snapshot(&[]);
        let p = build(&sorted, 0, frame(height(0)));
        assert_eq!(p.total_rows, 0);
        assert!(labels(&p.panel).is_empty());
        assert!(p.panel.widget::<Button>(ROW_BASE).is_none());
    }

    #[test]
    fn rows_sorted_by_z_order_top_first() {
        let snapshot = [
            window(r"C:\Apps\back.exe", "Back", 5),
            window(r"C:\Apps\front.exe", "Front", 1),
            window(r"C:\Apps\mid.exe", "Mid", 3),
        ];
        let sorted = sorted_snapshot(&snapshot);
        let p = build(&sorted, 0, frame(height(sorted.len())));
        assert_eq!(labels(&p.panel), vec!["Front", "Mid", "Back"]);
        for i in 0..3 {
            assert!(p.panel.widget::<Button>(ROW_BASE + i as WidgetId).is_some());
        }
    }

    #[test]
    fn empty_title_falls_back_to_exe_file_name() {
        let snapshot = [window(r"C:\Apps\notepad.exe", "", 0)];
        let sorted = sorted_snapshot(&snapshot);
        let p = build(&sorted, 0, frame(height(1)));
        assert_eq!(labels(&p.panel), vec!["notepad.exe"]);
    }

    #[test]
    fn empty_title_and_path_falls_back_to_placeholder() {
        let snapshot = [window("", "", 0)];
        let sorted = sorted_snapshot(&snapshot);
        let p = build(&sorted, 0, frame(height(1)));
        assert_eq!(labels(&p.panel), vec!["Без имени"]);
    }

    #[test]
    fn scroll_skips_rows_from_top_and_keeps_original_ids() {
        let snapshot: Vec<WindowInfo> = (0..15)
            .map(|i| window(&format!(r"C:\Apps\app{i}.exe"), &format!("Win {i}"), i))
            .collect();
        let sorted = sorted_snapshot(&snapshot);
        let p = build(&sorted, 12, frame(height(VISIBLE_ROWS)));
        assert_eq!(p.total_rows, 15);
        // Скролл 12 из 15 строк — видно только 3 последних (12, 13, 14),
        // остальные виджеты за окном скролла не строятся.
        assert_eq!(labels(&p.panel), vec!["Win 12", "Win 13", "Win 14"]);
        assert!(p.panel.widget::<Button>(ROW_BASE + 12).is_some());
        assert!(p.panel.widget::<Button>(ROW_BASE + 11).is_none());
    }

    #[test]
    fn visible_rows_capped_even_with_more_windows() {
        let snapshot: Vec<WindowInfo> = (0..20)
            .map(|i| window(&format!(r"C:\Apps\app{i}.exe"), &format!("Win {i}"), i))
            .collect();
        let sorted = sorted_snapshot(&snapshot);
        let p = build(&sorted, 0, frame(height(VISIBLE_ROWS)));
        assert_eq!(p.total_rows, 20);
        assert_eq!(labels(&p.panel).len(), VISIBLE_ROWS);
    }

    #[test]
    fn long_title_is_truncated_to_row_width() {
        let snapshot = [window(r"C:\Apps\app.exe", &"очень-длинный-заголовок-".repeat(10), 0)];
        let sorted = sorted_snapshot(&snapshot);
        let p = build(&sorted, 0, frame(height(1)));
        let text = &labels(&p.panel)[0];
        assert!(text.ends_with("..."), "длинный заголовок усечён многоточием");
        assert!(text.len() < snapshot[0].title.len());
    }

    #[test]
    fn height_grows_with_visible_count_but_caps_at_visible_rows() {
        assert!(height(3) > height(1));
        assert_eq!(height(0), height(1), "нулевой список не даёт нулевую высоту");
        assert_eq!(
            height(VISIBLE_ROWS),
            height(VISIBLE_ROWS + 50),
            "высота не растёт за пределы видимых строк — список скроллится"
        );
    }

    #[test]
    fn rows_stack_top_to_bottom_inside_frame() {
        let snapshot = [window(r"C:\Apps\a.exe", "A", 0), window(r"C:\Apps\b.exe", "B", 1)];
        let sorted = sorted_snapshot(&snapshot);
        let f = frame(height(sorted.len()));
        let p = build(&sorted, 0, f);
        let b0 = p.panel.widget::<Button>(ROW_BASE).unwrap().bounds();
        let b1 = p.panel.widget::<Button>(ROW_BASE + 1).unwrap().bounds();
        assert!(b1.cy > b0.cy, "вторая строка ниже первой");
        assert!(b0.cy - b0.h / 2.0 >= f.cy - f.h / 2.0, "первая строка в рамке");
        assert!(b1.cy + b1.h / 2.0 <= f.cy + f.h / 2.0, "последняя строка в рамке");
    }
}
