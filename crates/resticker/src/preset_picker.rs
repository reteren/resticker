//! Панель пресетов режима редактирования (M7, SPEC.md §3.8 «Загрузить
//! пресет»; ROADMAP.md — «быстрое переключение… из панели редактирования»).
//!
//! Не просто список для переключения: с 2026-08-23 отсюда можно сохранить
//! текущую расстановку под именем, импортировать пресет из файла и удалить
//! ненужный — запрос пользователя «хочу импортировать и сохранять прямо в
//! режиме редактирования, отдельной менюшкой, а не открывая настройки».
//! Раньше кнопка на панели инструментов при пустом списке вообще ничего не
//! делала (показывала тост), и это выглядело как сломанная кнопка.
//!
//! Модальная (для мыши и клавиатуры) — тем же паттерном, что
//! `confirm_dialog.rs`: открывается кнопкой `cursor_panel::BTN_PRESETS`,
//! закрывается кнопкой «Close», кликом мимо или `Esc`. В отличие от действий
//! со списком, панель при них НЕ закрывается: сохранил — видишь новую
//! строку, удалил — строка исчезла.
//!
//! Оформление — Dark Liquid Glass (docs/DESIGN_LIQUID_GLASS.md): корпус —
//! плита чёрного стекла с радиусом окна (§3 `RADIUS_WINDOW`), строки и
//! кнопки действий — стекло с фазами наведения/нажатия (кнопки rst-render
//! анимируют сами). Крестик удаления — единственный цвет интерфейса
//! [`DANGER`] (§2.4): тонкое предупреждение, не заливка.

use rst_core::model::Preset;
use rst_render::glass::STROKE_ALPHA;
use rst_render::{
    Box2D, Button, ButtonContent, LINE_HEIGHT, Label, Panel, Primitive, TextField, Widget,
    WidgetId, text_size, theme,
};

use crate::window_picker::truncate_to_width;

/// Идентификатор панели. Диапазон 400+: тулбар 0-8, панель у курсора 100+,
/// панель выбора окон 200+.
pub const PANEL_ID: WidgetId = 400;
/// Первая строка списка пресетов; `ROW_BASE + индекс` в `Config.presets`.
pub const ROW_BASE: WidgetId = 401;
/// Кнопка удаления строки; `DELETE_BASE + индекс` в `Config.presets`.
pub const DELETE_BASE: WidgetId = 450;
/// Поле имени нового пресета.
pub const FIELD_NAME: WidgetId = 410;
/// Кнопка «сохранить текущую расстановку».
pub const BTN_SAVE: WidgetId = 411;
/// Кнопка «импортировать из файла».
pub const BTN_IMPORT: WidgetId = 412;
/// Кнопка «закрыть панель».
pub const BTN_CLOSE: WidgetId = 413;
/// Заголовок панели.
const ID_TITLE: WidgetId = 414;
/// Подпись пустого списка.
const ID_EMPTY: WidgetId = 415;
/// Разделитель между списком и действиями.
const ID_DIVIDER: WidgetId = 416;

/// Ширина панели, DIP. В §3 токена нет — панель пресетов не менялась,
/// оставлена именованной константой модуля.
pub const WIDTH: f64 = 320.0;
/// Максимум строк без прокрутки: дальше список просто обрезается — панель
/// не должна перерастать экран, а десятки пресетов редактируются в
/// настройках.
pub const VISIBLE_ROWS: usize = 8;

/// Подписи кнопок.
pub const SAVE_LABEL: &str = "Save current";
pub const IMPORT_LABEL: &str = "Import…";
pub const CLOSE_LABEL: &str = "Close";
const TITLE_LABEL: &str = "Presets";
const EMPTY_LABEL: &str = "No presets yet — save the current layout below.";
const NAME_PLACEHOLDER: &str = "New preset name";
/// Предел длины имени пресета в поле — столько же, сколько принимает поле
/// в окне настроек (там ограничение ставит сам браузер по ширине).
const NAME_MAX_LEN: usize = 64;
const DELETE_MARK: &str = "x";

/// §2.4 `DANGER` — единственный цвет во всём интерфейсе (#D0463C), только
/// тонкие предупреждения. Псевдоним общего токена: константа заведена в
/// `rst_render::theme`, дублировать её по модулям нельзя (§9.1).
use rst_render::theme::DANGER;

/// Сколько строк реально показывается для `count` пресетов (пустой список
/// занимает одну строку под подпись).
fn visible_rows(count: usize) -> usize {
    count.clamp(1, VISIBLE_ROWS)
}

/// Высота панели под `count` пресетов.
pub fn height(count: usize) -> f64 {
    let rows = visible_rows(count) as f64;
    2.0 * theme::PAD_PANEL
        + LINE_HEIGHT
        + theme::GAP_ROW
        + rows * theme::BUTTON_SIZE
        + (rows - 1.0) * theme::GAP_ROW
        + theme::GAP_ROW
        + theme::HAIRLINE
        + theme::GAP_ROW
        + theme::BUTTON_SIZE
        + theme::GAP_ROW
        + theme::BUTTON_SIZE
}

/// Собрать панель: заголовок, список пресетов (строка = применить, «x» —
/// удалить), поле имени с кнопкой сохранения и ряд «импорт / закрыть».
///
/// `name_draft` — то, что пользователь уже успел набрать в поле имени
/// (панель пересобирается после каждого действия, состояние поля живёт у
/// вызывающего кода).
pub fn build(presets: &[Preset], name_draft: &str, frame: Box2D) -> Panel {
    let mut panel = Panel::new(PANEL_ID, frame)
        // Радиус окна (§3): панель пресетов — большая панель, как модал.
        .with_corner_radius(theme::RADIUS_WINDOW);
    let left = frame.cx - frame.w / 2.0 + theme::PAD_PANEL;
    let right = frame.cx + frame.w / 2.0 - theme::PAD_PANEL;
    let top = frame.cy - frame.h / 2.0 + theme::PAD_PANEL;
    let content_w = right - left;

    // Заголовок.
    let title_cy = top + LINE_HEIGHT / 2.0;
    panel.add_widget(Label::new(ID_TITLE, left, title_cy, TITLE_LABEL));

    // Список пресетов.
    let list_top = title_cy + LINE_HEIGHT / 2.0 + theme::GAP_ROW;
    if presets.is_empty() {
        // Подпись пустого списка — второстепенная: белый свет на
        // `TEXT_DIM_OPACITY` (§2.3), отдельных серых цветов больше нет.
        panel.add_widget(DimLabel::new(
            ID_EMPTY,
            left,
            list_top + theme::BUTTON_SIZE / 2.0,
            &truncate_to_width(EMPTY_LABEL, content_w),
        ));
    }
    for (i, preset) in presets.iter().take(VISIBLE_ROWS).enumerate() {
        let cy =
            list_top + i as f64 * (theme::BUTTON_SIZE + theme::GAP_ROW) + theme::BUTTON_SIZE / 2.0;
        let row_w = content_w - theme::BUTTON_SIZE - theme::GAP_ROW;
        let label = truncate_to_width(&preset.name, row_w - 2.0 * theme::PAD_CTRL_X);
        panel.add_widget(Button::new(
            ROW_BASE + i as WidgetId,
            Box2D {
                cx: left + row_w / 2.0,
                cy,
                w: row_w,
                h: theme::BUTTON_SIZE,
                rotation: 0.0,
            },
            ButtonContent::Label(label),
        ));
        // Крестик удаления — квадратная кнопка тулбара (§3 `BUTTON_SIZE`)
        // с опасным знаком: `DANGER` — только тонкие предупреждения.
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
            .with_label_color(DANGER),
        );
    }

    // Разделитель между списком и действиями.
    let rows = visible_rows(presets.len()) as f64;
    let list_bottom = list_top + rows * theme::BUTTON_SIZE + (rows - 1.0) * theme::GAP_ROW;
    let divider_cy = list_bottom + theme::GAP_ROW;
    panel.add_widget(Hairline::new(ID_DIVIDER, frame.cx, divider_cy, content_w));

    // Поле имени + «сохранить».
    let save_w = text_size(SAVE_LABEL).0 + 2.0 * theme::PAD_CTRL_X;
    let save_cy = divider_cy + theme::GAP_ROW + theme::BUTTON_SIZE / 2.0;
    let field_w = content_w - save_w - theme::GAP_ROW;
    panel.add_widget(
        TextField::with_placeholder(
            FIELD_NAME,
            Box2D {
                cx: left + field_w / 2.0,
                cy: save_cy,
                w: field_w,
                h: theme::BUTTON_SIZE,
                rotation: 0.0,
            },
            name_draft,
            NAME_MAX_LEN,
            NAME_PLACEHOLDER,
        )
        .keep_on_blur(),
    );
    panel.add_widget(Button::new(
        BTN_SAVE,
        Box2D {
            cx: right - save_w / 2.0,
            cy: save_cy,
            w: save_w,
            h: theme::BUTTON_SIZE,
            rotation: 0.0,
        },
        ButtonContent::Label(SAVE_LABEL.to_string()),
    ));

    // Импорт и закрытие.
    let actions_cy = save_cy + theme::BUTTON_SIZE / 2.0 + theme::GAP_ROW + theme::BUTTON_SIZE / 2.0;
    let import_w = text_size(IMPORT_LABEL).0 + 2.0 * theme::PAD_CTRL_X;
    let close_w = text_size(CLOSE_LABEL).0 + 2.0 * theme::PAD_CTRL_X;
    panel.add_widget(Button::new(
        BTN_IMPORT,
        Box2D {
            cx: left + import_w / 2.0,
            cy: actions_cy,
            w: import_w,
            h: theme::BUTTON_SIZE,
            rotation: 0.0,
        },
        ButtonContent::Label(IMPORT_LABEL.to_string()),
    ));
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

/// Разделитель — волосинка белого света (`STROKE`, §2.1): у стекла нет
/// тёмной «рамки», разделитель живёт светом на малой непрозрачности.
/// Свой, а не [`rst_render::Divider`]: у того цвет зашит из старой палитры.
struct Hairline {
    id: WidgetId,
    rect: Box2D,
}

impl Hairline {
    /// Горизонтальная линия-разделитель шириной `w` с центром в `(cx, cy)`,
    /// толщина — волосинка [`theme::HAIRLINE`].
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

    /// Не интерактивна — клики/hover сквозь неё, как у [`Label`].
    fn hit_test(&self, _pos: (f64, f64)) -> bool {
        false
    }

    fn draw(&self, out: &mut Vec<Primitive>) {
        out.push(Primitive::Fill {
            rect: self.rect,
            color: theme::TEXT,
            opacity: STROKE_ALPHA,
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
    use rst_render::{Primitive, Widget};
    use uuid::Uuid;

    fn preset(name: &str) -> Preset {
        Preset {
            id: Uuid::new_v4(),
            name: name.to_string(),
            stickers: Vec::new(),
        }
    }

    fn frame(count: usize) -> Box2D {
        Box2D {
            cx: 400.0,
            cy: 300.0,
            w: WIDTH,
            h: height(count),
            rotation: 0.0,
        }
    }

    fn texts(panel: &Panel) -> Vec<String> {
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
    fn empty_list_still_offers_save_import_and_close() {
        // Регрессия на «кнопка пресетов ничего не делает»: пустой список —
        // не повод не показывать панель, сохранять-то есть что.
        let panel = build(&[], "", frame(0));
        let labels = texts(&panel);
        assert!(labels.iter().any(|t| t == SAVE_LABEL), "{labels:?}");
        assert!(labels.iter().any(|t| t == IMPORT_LABEL), "{labels:?}");
        assert!(labels.iter().any(|t| t == CLOSE_LABEL), "{labels:?}");
        assert!(
            labels.iter().any(|t| t.starts_with("No presets yet")),
            "{labels:?}"
        );
        assert!(panel.widget::<Button>(ROW_BASE).is_none());
    }

    #[test]
    fn one_row_and_one_delete_per_preset_in_order() {
        let presets = vec![preset("Work"), preset("Stream"), preset("Night")];
        let panel = build(&presets, "", frame(presets.len()));
        let mut prev_cy = f64::NEG_INFINITY;
        for (i, name) in ["Work", "Stream", "Night"].into_iter().enumerate() {
            let row = panel
                .widget::<Button>(ROW_BASE + i as WidgetId)
                .unwrap_or_else(|| panic!("строка {i}"))
                .bounds();
            let del = panel
                .widget::<Button>(DELETE_BASE + i as WidgetId)
                .unwrap_or_else(|| panic!("удаление {i}"))
                .bounds();
            assert!(row.cy > prev_cy, "строки сверху вниз");
            prev_cy = row.cy;
            assert_eq!(del.cy, row.cy, "крестик на своей строке");
            assert!(del.cx > row.cx, "крестик справа от имени");
            assert!(texts(&panel).iter().any(|t| t == name));
        }
    }

    #[test]
    fn long_names_are_truncated_to_the_row() {
        let presets = vec![preset(&"очень-длинное-имя-пресета-".repeat(10))];
        let panel = build(&presets, "", frame(1));
        let row = panel.widget::<Button>(ROW_BASE).unwrap().bounds();
        let label = texts(&panel)
            .into_iter()
            .find(|t| t.contains("длинное"))
            .expect("строка пресета");
        assert!(label.ends_with("..."), "длинное имя усечено: {label}");
        assert!(text_size(&label).0 <= row.w);
    }

    #[test]
    fn name_draft_survives_a_rebuild() {
        // Панель пересобирается после каждого действия — набранное имя не
        // должно теряться.
        let panel = build(&[], "Night mode", frame(0));
        let field = panel.widget::<TextField>(FIELD_NAME).expect("поле имени");
        assert_eq!(field.text(), "Night mode");
    }

    #[test]
    fn everything_fits_inside_the_panel() {
        for count in [0, 1, 3, VISIBLE_ROWS, VISIBLE_ROWS + 5] {
            let presets: Vec<Preset> = (0..count).map(|i| preset(&format!("p{i}"))).collect();
            let f = frame(count);
            let panel = build(&presets, "draft", f);
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
                    "примитив {rect:?} вылез за панель {f:?} (count={count})"
                );
            }
        }
    }

    #[test]
    fn list_is_capped_at_visible_rows() {
        let presets: Vec<Preset> = (0..VISIBLE_ROWS + 4)
            .map(|i| preset(&format!("p{i}")))
            .collect();
        let panel = build(&presets, "", frame(presets.len()));
        assert!(
            panel
                .widget::<Button>(ROW_BASE + VISIBLE_ROWS as WidgetId)
                .is_none(),
            "строк больше VISIBLE_ROWS быть не должно"
        );
        assert_eq!(height(presets.len()), height(VISIBLE_ROWS));
    }
}
