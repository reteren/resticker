//! Маленькая панель настройки зазора снап-зон (запрос пользователя
//! 2026-08-25: «сделай просто квадратик с цифрой, на колесо мыши менять или
//! тыкнуть и вписать»).
//!
//! Почему панель, а не пункт меню трея. Меню трея — это `HMENU`, набор
//! статических строк: Windows не позволяет положить туда поле ввода, и любая
//! попытка выразить «произвольное число» пунктами меню сводится к списку
//! готовых значений, от которого пользователь и отказался. Поэтому в трее
//! остался один пункт, открывающий эту панель.
//!
//! Строитель чистый, как [`crate::preset_picker`]: на вход текущее значение и
//! рамка, на выход [`Panel`]. Состояние (открыта ли панель и на каком
//! мониторе) живёт в координаторе.

use rst_render::{
    Box2D, Button, ButtonContent, LINE_HEIGHT, Label, NumericField, Panel, WidgetId, WidgetStyle,
    theme,
};

/// Идентификатор панели. Диапазон 700+: тулбар 0-8, панель у курсора 100+,
/// панель выбора окон 200+, панель пресетов 400+.
pub const PANEL_ID: WidgetId = 700;
/// Поле процента.
pub const FIELD_GAP: WidgetId = 701;
/// Кнопка закрытия.
pub const BTN_CLOSE: WidgetId = 702;
/// Заголовок.
const ID_TITLE: WidgetId = 703;
/// Подсказка под полем.
const ID_HINT: WidgetId = 704;

/// Ширина панели, DIP.
pub const WIDTH: f64 = 260.0;
/// Внутренний отступ, DIP.
const PAD: f64 = 12.0;
/// Зазор между блоками, DIP.
const SECTION_GAP: f64 = 10.0;
/// Высота поля и кнопки, DIP.
const ROW_H: f64 = 28.0;
/// Ширина поля с числом: три цифры и каретка, не больше — широкое поле под
/// двузначное число выглядит как ошибка вёрстки.
const FIELD_W: f64 = 64.0;

const TITLE_LABEL: &str = "Snap gap";
const HINT_LABEL: &str = "Scroll to change, click to type";

/// Высота панели, DIP. Считается так же, как ширина строк: заголовок, поле,
/// подсказка, кнопка — чтобы правка любого отступа не разъезжалась с рамкой.
pub fn height() -> f64 {
    2.0 * PAD + LINE_HEIGHT + SECTION_GAP + ROW_H + SECTION_GAP + LINE_HEIGHT + SECTION_GAP + ROW_H
}

/// Собрать панель. `gap_pct` — текущее значение, 0..=35.
pub fn build(gap_pct: u8, frame: Box2D) -> Panel {
    let mut panel = Panel::new(PANEL_ID, frame)
        .with_style(WidgetStyle::Settings)
        .with_corner_radius(theme::settings::CORNER_RADIUS);
    let left = frame.cx - frame.w / 2.0 + PAD;
    let top = frame.cy - frame.h / 2.0 + PAD;

    let title_cy = top + LINE_HEIGHT / 2.0;
    panel.add_widget(Label::new(ID_TITLE, left, title_cy, TITLE_LABEL));

    // Поле по центру панели: это единственный смысл окна, и подписи слева
    // или справа от него ничего не добавили бы к заголовку.
    let field_cy = title_cy + LINE_HEIGHT / 2.0 + SECTION_GAP + ROW_H / 2.0;
    let mut field = NumericField::snap_gap(FIELD_GAP, frame.cx, field_cy, FIELD_W);
    field.set_value(u32::from(gap_pct));
    panel.add_widget(field);

    // Подсказка нужна ровно потому, что виджет необычный: квадратик с числом
    // сам по себе не говорит, что его крутят колесом.
    let hint_cy = field_cy + ROW_H / 2.0 + SECTION_GAP + LINE_HEIGHT / 2.0;
    let mut hint = Label::new(ID_HINT, left, hint_cy, HINT_LABEL);
    hint.set_dim(true);
    panel.add_widget(hint);

    let btn_cy = hint_cy + LINE_HEIGHT / 2.0 + SECTION_GAP + ROW_H / 2.0;
    panel.add_widget(
        Button::new(
            BTN_CLOSE,
            Box2D {
                cx: frame.cx,
                cy: btn_cy,
                w: 96.0,
                h: ROW_H,
                rotation: 0.0,
            },
            ButtonContent::Label("Close".to_string()),
        )
        .with_style(WidgetStyle::Settings),
    );

    panel
}

#[cfg(test)]
mod tests {
    use super::*;
    // `bounds()` приходит из типажа виджета — без него метод не виден.
    use rst_render::Widget;

    fn frame() -> Box2D {
        Box2D {
            cx: 960.0,
            cy: 540.0,
            w: WIDTH,
            h: height(),
            rotation: 0.0,
        }
    }

    #[test]
    fn field_shows_the_current_value() {
        let mut panel = build(17, frame());
        let field = panel
            .widget_mut::<NumericField>(FIELD_GAP)
            .expect("поле процента");
        assert_eq!(field.value(), 17);
    }

    #[test]
    fn the_bottom_button_fits_inside_the_panel() {
        // Высота панели считается формулой из тех же отступов, что и
        // раскладка. Забыть в формуле один зазор — значит выпустить нижнюю
        // кнопку за край, и заметить это можно было бы только глазами.
        // Проверяем самый нижний виджет: если влез он, влезли и все выше.
        let f = frame();
        let mut panel = build(5, f);
        let btn = panel
            .widget_mut::<Button>(BTN_CLOSE)
            .expect("кнопка закрытия");
        let b = btn.bounds();
        assert!(
            b.cy + b.h / 2.0 <= f.cy + f.h / 2.0 - PAD + 0.5,
            "кнопка вылезла за нижний край панели: низ {} при границе {}",
            b.cy + b.h / 2.0,
            f.cy + f.h / 2.0 - PAD
        );
    }

    #[test]
    fn value_above_the_ceiling_is_clamped_by_the_field() {
        // В config.json можно вписать руками что угодно; панель обязана
        // показать то же число, которое реально применится.
        let mut panel = build(200, frame());
        let field = panel
            .widget_mut::<NumericField>(FIELD_GAP)
            .expect("поле процента");
        assert_eq!(field.value(), 35);
    }
}
