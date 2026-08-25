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

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use rst_core::model::OverlapRule;
use rst_core::occluders::is_denylisted;
use rst_render::{
    Box2D, Button, ButtonContent, Label, Panel, Primitive, ScrollBar, Widget, WidgetId, text_size,
    theme,
};
use rst_win32::window_enum::{WindowIcon, WindowInfo};

use crate::window_picker::truncate_to_width;

/// Идентификатор панели. Диапазон 500+: тулбар 0-8, панель у курсора 100+,
/// панель выбора окон (M4) 200+, панель пресетов (M7) 400+.
pub const PANEL_ID: WidgetId = 500;
/// Первая строка списка; `ROW_BASE + индекс` в отсортированном по
/// z-order снимке (`sorted_snapshot`) — тот же индекс, что видит вызывающий
/// код при декодировании клика обратно в `WindowInfo`.
pub const ROW_BASE: WidgetId = 501;
/// Полоса скролла (тот же живой репорт пользователя, что у
/// `window_picker::PICKER_SCROLLBAR_ID`: длинный список окон не листался и
/// ничем не намекал, что это возможно).
const SCROLLBAR_ID: WidgetId = 599;
/// Плейсхолдер «нет окон» (пустой снимок): отдельный id вне диапазона строк
/// (визуальный полироль — раньше пустой список оставлял панель голым
/// прямоугольником без единой подсказки).
const EMPTY_LABEL_ID: WidgetId = 598;
/// Флаг для id надписи строки — не декодируется координатором обратно в
/// окно (тот же приём, что `window_picker::LABEL_FLAG`: строка = интерактивная
/// `Button` под тем же индексом + неинтерактивная надпись поверх неё, оба
/// виджета делят видимый прямоугольник строки).
const LABEL_FLAG: WidgetId = 0x8000_0000;

/// Ширина панели, DIP.
pub const WIDTH: f64 = 320.0;
/// Внутренний отступ, DIP.
const PAD: f64 = 6.0;
/// Зазор между строками, DIP.
const ROW_GAP: f64 = 4.0;
/// Сторона слота иконки строки, DIP — тот же размер, что
/// `window_picker::PICKER_ICON_SIZE` (визуальная параллель с «Слои
/// видимости», ближайшим аналогом этой панели в D3D11-рендере).
const ICON_SIZE: f64 = 20.0;
/// Зазор между иконкой и текстом строки, DIP — тот же, что
/// `window_picker::PICKER_GAP`.
const ICON_GAP: f64 = 8.0;
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

/// Снимок, из которого убраны окна, не годящиеся для закрепления
/// (редизайн пинов, SPEC.md «Закрепление окна»): денайлистовые
/// (`cfg.settings.denylist`, предикат [`is_denylisted`] — те же
/// процесс/заголовок-правила, что у хоткей-пина) и свёрнутые (rect от DWM
/// мусорный, пинить нечего — тот же принцип, что у `window_picker`/
/// окклюдеров). Порядок снимка сохраняется — сортировка по z-order
/// ([`sorted_snapshot`]) накладывается ПОВЕРХ этого фильтра, и индексы
/// строк должны считаться по одинаково отфильтрованному списку и в билдере,
/// и при декодировании клика (координатор, `handle_window_pick_list_up`).
///
/// Процесс для денайлиста — полный путь к exe, как его понимают
/// окклюдеры (`path_eq_ignore_case`: правило матчит и по полному пути, и по
/// одному имени файла).
pub fn eligible_snapshot(snapshot: &[WindowInfo], denylist: &[OverlapRule]) -> Vec<WindowInfo> {
    snapshot
        .iter()
        .filter(|w| {
            !w.iconic && !is_denylisted(window_exe_path(w).as_deref(), Some(&w.title), denylist)
        })
        .cloned()
        .collect()
}

/// Полный путь к exe окна для денайлиста — тот же перевод, что
/// `window_exe_path` в overlay_manager.rs (дублирован локально: тот
/// приватный, а этот модуль — самодостаточный и тестируемый без
/// координатора).
fn window_exe_path(w: &WindowInfo) -> Option<String> {
    if w.exe_path.as_os_str().is_empty() {
        None
    } else {
        w.exe_path.to_str().map(str::to_owned)
    }
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
        "Untitled".to_string()
    };
    truncate_to_width(&text, max_w)
}

/// Стабильный key иконки для кэша текстур (`Primitive::Rgba`) — тот же
/// приём, что `window_picker::icon_key`: хэш полного пути exe, окна одного
/// процесса делят одну GPU-текстуру.
fn icon_key(window: &WindowInfo) -> u64 {
    let mut hasher = DefaultHasher::new();
    window.exe_path.hash(&mut hasher);
    hasher.finish()
}

/// Результат [`build`]: панель + общее число строк (для клампа скролла
/// вызывающим кодом, тот же контракт, что `window_picker::PickerPanel`).
pub struct PickListPanel {
    pub panel: Panel,
    pub total_rows: usize,
}

/// Иконка + подпись строки (визуальный аналог `window_picker::RowLabel` —
/// не переиспользуется напрямую, тот приватен и завязан на бизнес-логику
/// панели правил соседства, здесь только отрисовка). Не интерактивна: клики
/// и hover-подсветку строки обрабатывает лежащая под ней `Button` того же
/// прямоугольника (тот же id, без [`LABEL_FLAG`]) — тот же приём разделения
/// «фон/интерактив» и «контент» строки, что в `window_picker.rs`.
struct RowContent {
    id: WidgetId,
    text_rect: Box2D,
    icon_rect: Box2D,
    text: String,
    icon: Option<(u64, WindowIcon)>,
}

impl Widget for RowContent {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn bounds(&self) -> Box2D {
        self.text_rect
    }

    fn set_bounds(&mut self, bounds: Box2D) {
        self.text_rect = bounds;
    }

    fn hit_test(&self, _pos: (f64, f64)) -> bool {
        false
    }

    fn draw(&self, out: &mut Vec<Primitive>) {
        match &self.icon {
            Some((key, icon)) => out.push(Primitive::Rgba {
                rect: self.icon_rect,
                key: *key,
                width: icon.width,
                height: icon.height,
                rgba: icon.rgba.clone(),
                opacity: 1.0,
            }),
            None => out.push(Primitive::Fill {
                rect: self.icon_rect,
                color: theme::BUTTON_BG,
                opacity: 1.0,
            }),
        }
        if !self.text.is_empty() {
            out.push(Primitive::Text {
                rect: self.text_rect,
                text: self.text.clone(),
                color: theme::TEXT,
                opacity: 1.0,
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

/// Собрать видимый срез списка. `sorted` — [`sorted_snapshot`] (уже
/// отсортированный, чтобы не сортировать на каждый вызов при скролле),
/// `scroll` — сколько строк пропущено сверху, `frame` — рамка панели
/// (типовой размер — [`WIDTH`]×[`height`]`(sorted.len())`, вызывающий код
/// сам решает).
///
/// Пустой список (`sorted` пуст) не остаётся голым прямоугольником —
/// рисуется приглушённая подсказка по центру панели (полироль: раньше
/// пустая панель ничем не отличалась от зависшей/незагруженной).
pub fn build(sorted: &[WindowInfo], scroll: usize, frame: Box2D) -> PickListPanel {
    let mut panel = Panel::new(PANEL_ID, frame);
    let top = frame.cy - frame.h / 2.0 + PAD;
    // Полоса скролла (ниже) всегда откусывает свою колонку от правого края —
    // ширина строк не скачет в зависимости от того, нужен ли сейчас скролл
    // (тот же приём, что `window_picker::build_picker_panel`).
    let row_w = frame.w - 2.0 * PAD - theme::SCROLLBAR_WIDTH - ROW_GAP;
    let row_left = frame.cx - row_w / 2.0 - theme::SCROLLBAR_WIDTH / 2.0 - ROW_GAP / 2.0;
    let icon_cx = row_left + ICON_SIZE / 2.0;
    let text_left = icon_cx + ICON_SIZE / 2.0 + ICON_GAP;
    let row_right = row_left + row_w;
    let max_text_w = (row_right - text_left).max(0.0);

    if sorted.is_empty() {
        let mut label = Label::new(EMPTY_LABEL_ID, row_left, frame.cy, "No windows available");
        label.set_dim(true);
        panel.add_widget(label);
    }

    for (i, window) in sorted.iter().enumerate().skip(scroll).take(VISIBLE_ROWS) {
        let visible_index = i - scroll;
        let cy =
            top + visible_index as f64 * (theme::BUTTON_SIZE + ROW_GAP) + theme::BUTTON_SIZE / 2.0;
        let id = ROW_BASE + i as WidgetId;
        // Фон + hover/armed-подсветка + хит-тест строки — без своей надписи
        // (иначе конвейер спрайтов растянул бы текстуру текста на всю
        // ширину строки, см. регрессионный тест в rst-render `widgets.rs`);
        // содержимое рисует `RowContent` поверх.
        panel.add_widget(Button::new(
            id,
            Box2D {
                cx: row_left + row_w / 2.0,
                cy,
                w: row_w,
                h: theme::BUTTON_SIZE,
                rotation: 0.0,
            },
            ButtonContent::Label(String::new()),
        ));
        let label = row_label(window, max_text_w);
        let (label_w, label_h) = text_size(&label);
        panel.add_widget(RowContent {
            id: id + LABEL_FLAG,
            text_rect: Box2D {
                cx: text_left + label_w / 2.0,
                cy,
                w: label_w,
                h: label_h,
                rotation: 0.0,
            },
            icon_rect: Box2D {
                cx: icon_cx,
                cy,
                w: ICON_SIZE,
                h: ICON_SIZE,
                rotation: 0.0,
            },
            text: label,
            icon: window.icon.clone().map(|icon| (icon_key(window), icon)),
        });
    }

    if sorted.len() > VISIBLE_ROWS {
        // Полная высота видимой области списка (VISIBLE_ROWS строк) —
        // независимо от того, сколько строк реально построилось на текущей
        // позиции скролла: панель не меняет размер (`height()` капается на
        // VISIBLE_ROWS), последняя страница просто может быть неполной
        // (тот же принцип, что у `window_picker`).
        let list_h = VISIBLE_ROWS as f64 * theme::BUTTON_SIZE + (VISIBLE_ROWS - 1) as f64 * ROW_GAP;
        panel.add_widget(ScrollBar::new(
            SCROLLBAR_ID,
            Box2D {
                cx: frame.cx + frame.w / 2.0 - PAD - theme::SCROLLBAR_WIDTH / 2.0,
                cy: top + list_h / 2.0,
                w: theme::SCROLLBAR_WIDTH,
                h: list_h,
                rotation: 0.0,
            },
            VISIBLE_ROWS,
            sorted.len(),
            scroll,
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
            resizable: true,
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
        assert!(p.panel.widget::<Button>(ROW_BASE).is_none());
    }

    /// Полироль: пустой список окон рисует подсказку вместо голого
    /// прямоугольника.
    #[test]
    fn empty_snapshot_shows_placeholder_message() {
        let sorted = sorted_snapshot(&[]);
        let p = build(&sorted, 0, frame(height(0)));
        assert_eq!(labels(&p.panel), vec!["No windows available"]);
    }

    /// Непустой список не показывает подсказку «нет окон».
    #[test]
    fn nonempty_snapshot_has_no_placeholder_message() {
        let snapshot = [window(r"C:\Apps\a.exe", "A", 0)];
        let sorted = sorted_snapshot(&snapshot);
        let p = build(&sorted, 0, frame(height(1)));
        assert!(!labels(&p.panel).contains(&"No windows available".to_string()));
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
        assert_eq!(labels(&p.panel), vec!["Untitled"]);
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

    /// Регрессия на живой репорт пользователя: длинный список окон для
    /// закрепления обрезался без намёка на то, что его можно листать.
    #[test]
    fn scrollbar_appears_only_when_list_overflows_visible_rows() {
        let short: Vec<WindowInfo> = (0..3)
            .map(|i| window(&format!(r"C:\Apps\app{i}.exe"), &format!("Win {i}"), i))
            .collect();
        let sorted_short = sorted_snapshot(&short);
        let p_short = build(&sorted_short, 0, frame(height(sorted_short.len())));
        assert!(
            p_short.panel.widget::<ScrollBar>(SCROLLBAR_ID).is_none(),
            "нечего листать — полосы скролла быть не должно"
        );

        let long: Vec<WindowInfo> = (0..15)
            .map(|i| window(&format!(r"C:\Apps\app{i}.exe"), &format!("Win {i}"), i))
            .collect();
        let sorted_long = sorted_snapshot(&long);
        let p_long = build(&sorted_long, 0, frame(height(VISIBLE_ROWS)));
        assert!(
            p_long.panel.widget::<ScrollBar>(SCROLLBAR_ID).is_some(),
            "список длиннее видимой части — полоса скролла обязана появиться"
        );
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
        let snapshot = [window(
            r"C:\Apps\app.exe",
            &"очень-длинный-заголовок-".repeat(10),
            0,
        )];
        let sorted = sorted_snapshot(&snapshot);
        let p = build(&sorted, 0, frame(height(1)));
        let text = &labels(&p.panel)[0];
        assert!(
            text.ends_with("..."),
            "длинный заголовок усечён многоточием"
        );
        assert!(text.len() < snapshot[0].title.len());
    }

    /// Полироль: строка без иконки окна рисует плейсхолдер-квадрат (тот же
    /// приём, что `window_picker::RowLabel`), не оставляет колонку иконки
    /// пустой/невидимой.
    #[test]
    fn row_without_icon_draws_placeholder_fill() {
        let snapshot = [window(r"C:\Apps\app.exe", "App", 0)];
        let sorted = sorted_snapshot(&snapshot);
        let p = build(&sorted, 0, frame(height(1)));
        let mut out = Vec::new();
        p.panel.draw(&mut out);
        let has_icon_placeholder = out.iter().any(|prim| {
            matches!(prim, Primitive::Fill { rect, .. } if rect.w == ICON_SIZE && rect.h == ICON_SIZE)
        });
        assert!(
            has_icon_placeholder,
            "нет иконки — рисуется плейсхолдер-квадрат"
        );
    }

    /// Строка с реальной иконкой окна рисует её растром, не плейсхолдером.
    #[test]
    fn row_with_icon_draws_rgba_primitive() {
        let mut w = window(r"C:\Apps\app.exe", "App", 0);
        w.icon = Some(WindowIcon {
            width: 16,
            height: 16,
            rgba: vec![0u8; 16 * 16 * 4],
        });
        let sorted = sorted_snapshot(&[w]);
        let p = build(&sorted, 0, frame(height(1)));
        let mut out = Vec::new();
        p.panel.draw(&mut out);
        let has_rgba_icon = out.iter().any(|prim| {
            matches!(
                prim,
                Primitive::Rgba {
                    width: 16,
                    height: 16,
                    ..
                }
            )
        });
        assert!(
            has_rgba_icon,
            "с иконкой — рисуется реальный растр, не плейсхолдер"
        );
    }

    #[test]
    fn height_grows_with_visible_count_but_caps_at_visible_rows() {
        assert!(height(3) > height(1));
        assert_eq!(
            height(0),
            height(1),
            "нулевой список не даёт нулевую высоту"
        );
        assert_eq!(
            height(VISIBLE_ROWS),
            height(VISIBLE_ROWS + 50),
            "высота не растёт за пределы видимых строк — список скроллится"
        );
    }

    #[test]
    fn eligible_snapshot_removes_denylisted_windows() {
        let allowed = window(r"C:\Apps\good.exe", "Good", 0);
        let denied_by_name = window(r"C:\Apps\bad.exe", "Bad", 1);
        let denied_by_title = window(r"C:\Apps\other.exe", "Secret * window", 2);
        let snapshot = [
            allowed.clone(),
            denied_by_name.clone(),
            denied_by_title.clone(),
        ];
        let denylist = vec![
            OverlapRule {
                process_name: Some("bad.exe".to_string()),
                title_pattern: None,
            },
            OverlapRule {
                process_name: None,
                title_pattern: Some("Secret * window".to_string()),
            },
        ];
        let eligible = eligible_snapshot(&snapshot, &denylist);
        assert_eq!(
            eligible,
            vec![allowed.clone()],
            "денайлистовые окна не в списке вовсе"
        );
        // Порядок исходного снимка сохранён (сортировка — отдельный шаг).
        let eligible_full = eligible_snapshot(&snapshot, &[]);
        assert_eq!(
            eligible_full,
            vec![allowed, denied_by_name, denied_by_title],
            "пустой денайлист пропускает все окна"
        );
    }

    #[test]
    fn eligible_snapshot_matches_process_by_file_name_or_path() {
        let by_full_path = window(r"C:\Apps\chrome.exe", "Tab", 0);
        let denylist_full = vec![OverlapRule {
            process_name: Some(r"C:\Apps\chrome.exe".to_string()),
            title_pattern: None,
        }];
        assert!(eligible_snapshot(std::slice::from_ref(&by_full_path), &denylist_full).is_empty());

        let denylist_name = vec![OverlapRule {
            process_name: Some("chrome.exe".to_string()),
            title_pattern: None,
        }];
        assert!(
            eligible_snapshot(&[by_full_path], &denylist_name).is_empty(),
            "правило с одним именем файла матчит полный путь (path_eq_ignore_case)"
        );
    }

    #[test]
    fn eligible_snapshot_removes_iconic_windows() {
        let mut minimized = window(r"C:\Apps\app.exe", "Min", 0);
        minimized.iconic = true;
        let normal = window(r"C:\Apps\app.exe", "Normal", 1);
        let eligible = eligible_snapshot(&[minimized, normal.clone()], &[]);
        assert_eq!(
            eligible,
            vec![normal],
            "свёрнутые окна не предлагаются к закреплению"
        );
    }

    #[test]
    fn rows_stack_top_to_bottom_inside_frame() {
        let snapshot = [
            window(r"C:\Apps\a.exe", "A", 0),
            window(r"C:\Apps\b.exe", "B", 1),
        ];
        let sorted = sorted_snapshot(&snapshot);
        let f = frame(height(sorted.len()));
        let p = build(&sorted, 0, f);
        let b0 = p.panel.widget::<Button>(ROW_BASE).unwrap().bounds();
        let b1 = p.panel.widget::<Button>(ROW_BASE + 1).unwrap().bounds();
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
}
