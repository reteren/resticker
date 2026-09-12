//! Тулбар выделения (SPEC.md 3.6, docs/M2_UI_NOTES.md §8): сборка панели из
//! виджетов rst-render и её позиционирование.
//!
//! Состав панели ОДИНАКОВ для одиночного выделения и для нескольких стикеров
//! (запрос пользователя 2026-09-06): ползунок прозрачности, числовое поле и
//! кнопки есть всегда, а видео-виджеты появляются, как только в выделении
//! есть хоть одно видео. Прежнее правило SPEC 3.6 «в мульти-режиме только
//! кнопки» отменено самим пользователем: «если я выделяю 2 стикера, чтобы
//! одновременно менять их прозрачность». Значение показывается по последнему
//! выделенному, а применяется ко всем — после правки прозрачность у них
//! общая, даже если до неё была разной.
//!
//! Модуль только конструирует [`Panel`]: роутинг событий, действия кнопок
//! и перерисовка — у ядра редактирования (overlay_manager). Кнопка «Слои
//! видимости» открывает панель выбора окон (SPEC 3.6, п. 3) — её сборка
//! и состояние живут в [`crate::window_picker`] (docs/M4_WINDOW_PICKER_DESIGN.md).

use rst_core::hittest::DipRect;
use rst_render::{
    Box2D, Button, Icon, NumericField, Panel, Slider, VolumeControl, WidgetId, theme,
};

/// Идентификаторы виджетов тулбара — для опроса состояния ядром через
/// [`Panel::widget`]/[`Panel::widget_mut`].
pub const TB_PANEL: WidgetId = 0;
pub const TB_SLIDER: WidgetId = 1;
pub const TB_FIELD: WidgetId = 2;
pub const TB_LAYERS: WidgetId = 8;
pub const TB_EYE: WidgetId = 3;
pub const TB_ORDER_UP: WidgetId = 4;
pub const TB_ORDER_DOWN: WidgetId = 5;
pub const TB_DUPLICATE: WidgetId = 6;
pub const TB_DELETE: WidgetId = 7;
/// «Играть/пауза» видео-стикера (M5b) — есть, когда в выделении есть
/// видео; иконка отражает действие (`Icon::Play` на паузе, `Icon::Pause`
/// во время игры).
pub const TB_PLAY_PAUSE: WidgetId = 9;
/// Переключатель «показывать полосу перемотки вне режима редактирования»
/// (запрос пользователя 2026-08-22).
pub const TB_TIMELINE: WidgetId = 12;
/// Громкость видео-стикера (M5b) — [`VolumeControl`]: кнопка-динамик с
/// выпадающей вертикальной шкалой 0..=100 процентов.
pub const TB_VOLUME: WidgetId = 10;
/// «Сбросить масштаб» (фидбэк пользователя 2026-08-09): размер/поворот/
/// отражения стикера обратно к натуральным (то же действие, что
/// `OverlayCommand::ResetStickerTransform` из окна настроек), позиция и
/// прозрачность не трогаются.
pub const TB_RESET_SCALE: WidgetId = 11;

/// Отступ тулбара от рамки выделения, DIP.
pub const TOOLBAR_GAP_Y: f64 = 8.0;
/// Внутренний отступ панели, DIP. Не `theme::PAD_PANEL` (§3): тот — 14 DIP,
/// для узкого тулбара высотой [`TOOLBAR_HEIGHT`] он съел бы всю панель.
pub const TOOLBAR_PAD: f64 = 4.0;
/// Зазор между виджетами, DIP. Не `theme::GAP_ROW` (§3): тот — расстояние
/// между строками, а здесь ползунок, поле и кнопки стоят в один ряд вплотную.
pub const TOOLBAR_WIDGET_GAP: f64 = 4.0;
/// Ширина ползунка прозрачности, DIP.
pub const TOOLBAR_SLIDER_W: f64 = 96.0;
/// Ширина числового поля, DIP.
pub const TOOLBAR_FIELD_W: f64 = 40.0;
/// Высота тулбара: кнопка + двойной отступ, DIP.
pub const TOOLBAR_HEIGHT: f64 = theme::BUTTON_SIZE + 2.0 * TOOLBAR_PAD;
/// Ширина тулбара: отступы + ползунок + поле + 7 кнопок + зазоры
/// (седьмая штатная — «сбросить масштаб», фидбэк 2026-08-09), DIP.
pub const TOOLBAR_WIDTH: f64 = 2.0 * TOOLBAR_PAD
    + TOOLBAR_SLIDER_W
    + TOOLBAR_WIDGET_GAP
    + TOOLBAR_FIELD_W
    + TOOLBAR_WIDGET_GAP
    + 7.0 * theme::BUTTON_SIZE
    + 6.0 * TOOLBAR_WIDGET_GAP;
/// Добавочная ширина видео-виджетов (M5b): играть/пауза + полоса перемотки
/// + громкость, каждый со своим зазором слева. Все три — квадраты размера
///   кнопки: громкость с 2026-09-06 не ползунок, а динамик с выпадающей
///   шкалой ([`VolumeControl`]), и тулбар похудел на 66 DIP.
pub const TOOLBAR_VIDEO_EXTRA_W: f64 = 3.0 * (TOOLBAR_WIDGET_GAP + theme::BUTTON_SIZE);

/// Состояние воспроизведения для тулбара (M5b): играть/пауза, полоса
/// перемотки и громкость справа от обычных семи кнопок — когда в выделении
/// есть хоть одно видео. Позиция «мотать»
/// (перемотка) технически готова в `rst_video::VideoSource::seek`, но
/// требует непрерывной перестройки панели во время игры/перетаскивания
/// (в отличие от play/pause и громкости — дискретных действий) — отдельный
/// срез, отложено сознательно (docs/M5B_VIDEO_DESIGN.md §6).
pub struct VideoToolbarState {
    /// `true` — воспроизведение на паузе (кнопка показывает `Icon::Play`).
    /// При нескольких выделенных видео — «на паузе ВСЕ»: пока играет хоть
    /// одно, кнопка обязана предлагать паузу.
    pub paused: bool,
    /// Полоса перемотки показывается вне режима редактирования (запрос
    /// пользователя 2026-08-22) — состояние переключателя `TB_TIMELINE`.
    /// При нескольких выделенных — «включена у ВСЕХ».
    pub show_timeline: bool,
    /// Громкость в процентах, `0..=100` (то же зеркалирование, что у
    /// прозрачности: модель хранит `0.0..=1.0`, виджет — целые проценты).
    pub volume_pct: u32,
    /// Звук выключен (`playback.muted`).
    pub muted: bool,
}

/// Состояние тулбара выделения: всё, что панель обязана показать про
/// выделение — одиночное оно или нет.
pub struct ToolbarState {
    /// Прозрачность для ползунка и поля, `0.0..=1.0`. При нескольких
    /// выделенных — прозрачность ПОСЛЕДНЕГО выделенного: он и есть тот, на
    /// который человек смотрел последним.
    pub opacity: f64,
    /// Все стикеры выделения видимы — иконка глаза отражает СОСТОЯНИЕ, как
    /// кнопка «показать/скрыть всё» на панели у курсора (запрос
    /// пользователя 2026-09-06). Стоит хоть одному быть скрытым — глаз
    /// закрыт, и клик показывает всех.
    pub visible: bool,
    /// Видео-виджеты — есть, если в выделении есть хоть одно видео (в том
    /// числе вперемешку с картинками: пользователь просил, чтобы в смешанном
    /// выделении были доступны и общие действия, и видео-специфичные).
    pub video: Option<VideoToolbarState>,
}

/// Верхняя координата Y тулбара (SPEC 3.6): под рамкой; если снизу до низа
/// экрана не хватает [`TOOLBAR_HEIGHT`] — над рамкой. Третий случай SPEC
/// («прижимается к ближайшему свободному краю», когда нет места и сверху) —
/// задел: сейчас тулбар уходит над рамку даже в отрицательные координаты.
fn toolbar_top(bounds: &DipRect, screen_h: f64) -> f64 {
    let below = bounds.y + bounds.h + TOOLBAR_GAP_Y;
    if below + TOOLBAR_HEIGHT <= screen_h {
        below
    } else {
        bounds.y - TOOLBAR_GAP_Y - TOOLBAR_HEIGHT
    }
}

/// Собрать тулбар выделения: ползунок прозрачности, зеркалирующее числовое
/// поле и семь кнопок (SPEC 3.6: слои видимости, глаз, выше, ниже,
/// дублировать, сбросить масштаб, удалить), а справа — видео-виджеты, если
/// в выделении есть видео.
///
/// `bounds` — ось-выровненный bbox выделения в DIP: для одиночного — aabb
/// рамки, для нескольких — union `selection.bounds()`; вычисляет его
/// вызывающий код. `screen_h` — высота монитора в DIP. По горизонтали
/// тулбар центрируется под bbox; зажим по краям экрана — задел вместе
/// с третьим случаем SPEC 3.6.
pub fn build_toolbar(bounds: &DipRect, state: &ToolbarState, screen_h: f64) -> Panel {
    let width = TOOLBAR_WIDTH + state.video.as_ref().map_or(0.0, |_| TOOLBAR_VIDEO_EXTRA_W);
    let top = toolbar_top(bounds, screen_h);
    let left = bounds.x + bounds.w / 2.0 - width / 2.0;
    let cy = top + TOOLBAR_HEIGHT / 2.0;

    // Корпус — плита чёрного стекла (§4): материал, кромки и обводку рисует
    // сам `Panel::draw` через `glass_panel`. Малый радиус — единственное
    // исключение §3: тулбар узкий, и большое скругление съело бы крайние
    // кнопки.
    let mut panel = Panel::new(
        TB_PANEL,
        Box2D {
            cx: left + width / 2.0,
            cy,
            w: width,
            h: TOOLBAR_HEIGHT,
            rotation: 0.0,
        },
    )
    .with_corner_radius(theme::RADIUS_TIGHT);

    let mut x = left + TOOLBAR_PAD;

    // Прозрачность: модель хранит 0.0–1.0, виджеты — целые проценты.
    let value = (state.opacity.clamp(0.0, 1.0) * 100.0).round() as u32;

    let mut slider = Slider::opacity(TB_SLIDER, x + TOOLBAR_SLIDER_W / 2.0, cy, TOOLBAR_SLIDER_W);
    slider.set_value(value);
    panel.add_widget(slider);
    x += TOOLBAR_SLIDER_W + TOOLBAR_WIDGET_GAP;

    let mut field = NumericField::opacity(TB_FIELD, x + TOOLBAR_FIELD_W / 2.0, cy, TOOLBAR_FIELD_W);
    field.set_value(value);
    panel.add_widget(field);
    x += TOOLBAR_FIELD_W + TOOLBAR_WIDGET_GAP;

    // Глаз показывает СОСТОЯНИЕ видимости (и рисунком — тем же, что на
    // панели у курсора), а не предстоящее действие: запрос пользователя
    // 2026-09-06 «сделай её такой же, как на основном тулбаре, и чтобы при
    // нажатии менялась иконка на закрытый глаз».
    let eye_icon = if state.visible {
        Icon::Eye
    } else {
        Icon::EyeOff
    };
    let buttons = [
        (TB_LAYERS, Icon::Layers),
        (TB_EYE, eye_icon),
        (TB_ORDER_UP, Icon::OrderUp),
        (TB_ORDER_DOWN, Icon::OrderDown),
        (TB_DUPLICATE, Icon::Duplicate),
        (TB_RESET_SCALE, Icon::ResetScale),
        (TB_DELETE, Icon::Delete),
    ];
    for (id, icon) in buttons {
        panel.add_widget(Button::icon(id, x + theme::BUTTON_SIZE / 2.0, cy, icon));
        x += theme::BUTTON_SIZE + TOOLBAR_WIDGET_GAP;
    }

    if let Some(video) = &state.video {
        // Иконка кнопки отражает действие, а не текущее состояние (как
        // Eye/EyeOff): на паузе показываем «играть», во время игры —
        // «пауза» — то же соглашение, что у большинства плееров.
        let play_icon = if video.paused {
            Icon::Play
        } else {
            Icon::Pause
        };
        panel.add_widget(Button::icon(
            TB_PLAY_PAUSE,
            x + theme::BUTTON_SIZE / 2.0,
            cy,
            play_icon,
        ));
        x += theme::BUTTON_SIZE + TOOLBAR_WIDGET_GAP;

        // Переключатель полосы перемотки вне режима редактирования: иконка
        // отражает СОСТОЯНИЕ (как Eye/EyeOff), а не разовое действие. С
        // 2026-09-06 включённое состояние ещё и НАЛИТО светом (`toggled`):
        // до этого состояния отличались только тоном рисунка, и на сером
        // стекле выключенное читалось как «кнопка недоступна» — дословная
        // жалоба пользователя.
        let timeline_icon = if video.show_timeline {
            Icon::Timeline
        } else {
            Icon::TimelineOff
        };
        panel.add_widget(
            Button::icon(TB_TIMELINE, x + theme::BUTTON_SIZE / 2.0, cy, timeline_icon)
                .toggled(video.show_timeline),
        );
        x += theme::BUTTON_SIZE + TOOLBAR_WIDGET_GAP;

        panel.add_widget(VolumeControl::new(
            TB_VOLUME,
            x + theme::BUTTON_SIZE / 2.0,
            cy,
            video.volume_pct,
            video.muted,
        ));
    }

    panel
}

#[cfg(test)]
mod tests {
    use super::*;
    use rst_render::{Primitive, Widget};

    const SCREEN_H: f64 = 1080.0;

    /// Ось-выровненный bbox по центру и размеру — как его передаёт
    /// вызывающий код (`selection.bounds()` / aabb рамки выделения).
    fn aabb(cx: f64, cy: f64, w: f64, h: f64) -> DipRect {
        DipRect::from_center(cx, cy, w, h)
    }

    /// Обычное состояние: видимый стикер, без видео.
    fn plain(opacity: f64) -> ToolbarState {
        ToolbarState {
            opacity,
            visible: true,
            video: None,
        }
    }

    fn with_video(video: VideoToolbarState) -> ToolbarState {
        ToolbarState {
            opacity: 1.0,
            visible: true,
            video: Some(video),
        }
    }

    fn video_state(paused: bool, show_timeline: bool, volume_pct: u32) -> VideoToolbarState {
        VideoToolbarState {
            paused,
            show_timeline,
            volume_pct,
            muted: false,
        }
    }

    /// Центр строки виджетов тулбара (совпадает с cy рамки панели).
    fn toolbar_cy(p: &Panel) -> f64 {
        p.frame().cy
    }

    #[test]
    fn toolbar_below_selection_when_space() {
        // Рамка y ∈ [350, 450]; снизу места достаточно — тулбар под ней.
        let p = build_toolbar(&aabb(960.0, 400.0, 200.0, 100.0), &plain(1.0), SCREEN_H);
        assert_eq!(toolbar_cy(&p), 450.0 + TOOLBAR_GAP_Y + TOOLBAR_HEIGHT / 2.0);
        assert!(p.hit_test((960.0, 476.0)));
        assert!(!p.hit_test((960.0, 457.0)), "зазор между рамкой и тулбаром");
    }

    #[test]
    fn toolbar_centered_horizontally_on_bbox() {
        let bounds = aabb(960.0, 400.0, 200.0, 100.0);
        let p = build_toolbar(&bounds, &plain(1.0), SCREEN_H);
        assert!(
            (p.frame().cx - 960.0).abs() < 1e-9,
            "центр тулбара под центром рамки"
        );
        let left = 960.0 - TOOLBAR_WIDTH / 2.0;
        assert!(p.hit_test((left + 1.0, 476.0)), "левый край тулбара");
        assert!(!p.hit_test((left - 1.0, 476.0)));
    }

    #[test]
    fn toolbar_above_when_no_space_below() {
        // Рамка y ∈ [1010, 1070]; снизу всего 10 DIP — тулбар над рамкой.
        let p = build_toolbar(&aabb(960.0, 1040.0, 200.0, 60.0), &plain(1.0), SCREEN_H);
        assert_eq!(
            toolbar_cy(&p),
            1010.0 - TOOLBAR_GAP_Y - TOOLBAR_HEIGHT / 2.0
        );
        assert!(p.hit_test((960.0, 984.0)));
    }

    #[test]
    fn toolbar_below_boundary_is_inclusive() {
        // Высота экрана ровно «низ рамки + зазор + высота тулбара» — ещё снизу.
        let screen_h = 450.0 + TOOLBAR_GAP_Y + TOOLBAR_HEIGHT;
        let p = build_toolbar(&aabb(960.0, 400.0, 200.0, 100.0), &plain(1.0), screen_h);
        assert_eq!(toolbar_cy(&p), 450.0 + TOOLBAR_GAP_Y + TOOLBAR_HEIGHT / 2.0);
        // Один DIP меньше — уже сверху.
        let p = build_toolbar(
            &aabb(960.0, 400.0, 200.0, 100.0),
            &plain(1.0),
            screen_h - 1.0,
        );
        assert_eq!(toolbar_cy(&p), 350.0 - TOOLBAR_GAP_Y - TOOLBAR_HEIGHT / 2.0);
    }

    #[test]
    fn toolbar_uses_passed_aabb() {
        // aabb повёрнутого стикера (для 90° поворота 100×40 это 40×100,
        // y ∈ [850, 950]) вычисляет вызывающий код — билдер позиционируется
        // под переданным прямоугольником.
        let p = build_toolbar(&aabb(960.0, 900.0, 40.0, 100.0), &plain(1.0), SCREEN_H);
        assert_eq!(toolbar_cy(&p), 950.0 + TOOLBAR_GAP_Y + TOOLBAR_HEIGHT / 2.0);
    }

    #[test]
    fn toolbar_opacity_mirrored_in_slider_and_field() {
        let p = build_toolbar(&aabb(960.0, 400.0, 200.0, 100.0), &plain(0.85), SCREEN_H);
        assert_eq!(p.widget::<Slider>(TB_SLIDER).unwrap().value(), 85);
        assert_eq!(p.widget::<NumericField>(TB_FIELD).unwrap().value(), 85);
    }

    #[test]
    fn toolbar_opacity_clamped_to_percent_range() {
        let p = build_toolbar(&aabb(960.0, 400.0, 200.0, 100.0), &plain(1.5), SCREEN_H);
        assert_eq!(p.widget::<Slider>(TB_SLIDER).unwrap().value(), 100);
        let p = build_toolbar(&aabb(960.0, 400.0, 200.0, 100.0), &plain(-0.5), SCREEN_H);
        assert_eq!(p.widget::<Slider>(TB_SLIDER).unwrap().value(), 0);
        // Поле по SPEC 3.6 живёт в 1–100: нулевой стикер зеркалится единицей.
        assert_eq!(p.widget::<NumericField>(TB_FIELD).unwrap().value(), 1);
    }

    /// Ползунок и поле есть и над несколькими стикерами: прежнее правило
    /// SPEC 3.6 «в мульти-режиме только кнопки» отменил сам пользователь
    /// (2026-09-06) — прозрачность нужна именно у группы.
    #[test]
    fn toolbar_over_many_stickers_still_has_opacity_controls() {
        let p = build_toolbar(&aabb(960.0, 400.0, 400.0, 200.0), &plain(0.4), SCREEN_H);
        assert_eq!(p.widget::<Slider>(TB_SLIDER).unwrap().value(), 40);
        assert_eq!(p.widget::<NumericField>(TB_FIELD).unwrap().value(), 40);
        assert_eq!(p.frame().w, TOOLBAR_WIDTH, "ширина одна на любое выделение");
    }

    /// Иконка кнопки: ищем примитив по типу, а не по индексу — под иконкой
    /// лежит растр стекла (`Primitive::Glass`), и его позиция в списке
    /// зависит от фазы наведения/нажатия (§5) — считать её хрупко.
    fn button_icon(p: &Panel, id: WidgetId) -> Icon {
        let b = p.widget::<Button>(id).unwrap();
        let mut out = Vec::new();
        b.draw(&mut out);
        let Some(Primitive::Icon { icon, .. }) = out
            .into_iter()
            .find(|prim| matches!(prim, Primitive::Icon { .. }))
        else {
            panic!("у кнопки тулбара нет примитива-иконки")
        };
        icon
    }

    #[test]
    fn toolbar_seven_buttons_in_spec_order() {
        let p = build_toolbar(&aabb(960.0, 400.0, 200.0, 100.0), &plain(1.0), SCREEN_H);
        let expected = [
            (TB_LAYERS, Icon::Layers),
            (TB_EYE, Icon::Eye),
            (TB_ORDER_UP, Icon::OrderUp),
            (TB_ORDER_DOWN, Icon::OrderDown),
            (TB_DUPLICATE, Icon::Duplicate),
            (TB_RESET_SCALE, Icon::ResetScale),
            (TB_DELETE, Icon::Delete),
        ];
        for (id, icon) in expected {
            assert_eq!(button_icon(&p, id), icon);
        }
        // Порядок слева направо: ползунок, поле, затем кнопки.
        let centers = [
            p.widget::<Slider>(TB_SLIDER).unwrap().bounds().cx,
            p.widget::<NumericField>(TB_FIELD).unwrap().bounds().cx,
            p.widget::<Button>(TB_LAYERS).unwrap().bounds().cx,
            p.widget::<Button>(TB_EYE).unwrap().bounds().cx,
            p.widget::<Button>(TB_ORDER_UP).unwrap().bounds().cx,
            p.widget::<Button>(TB_ORDER_DOWN).unwrap().bounds().cx,
            p.widget::<Button>(TB_DUPLICATE).unwrap().bounds().cx,
            p.widget::<Button>(TB_RESET_SCALE).unwrap().bounds().cx,
            p.widget::<Button>(TB_DELETE).unwrap().bounds().cx,
        ];
        assert!(
            centers.windows(2).all(|w| w[0] < w[1]),
            "виджеты слева направо"
        );
    }

    /// Глаз показывает состояние: скрытому выделению — закрытый глаз
    /// (запрос пользователя 2026-09-06).
    #[test]
    fn toolbar_eye_reflects_visibility() {
        let visible = ToolbarState {
            opacity: 1.0,
            visible: true,
            video: None,
        };
        let hidden = ToolbarState {
            opacity: 1.0,
            visible: false,
            video: None,
        };
        let bounds = aabb(960.0, 400.0, 200.0, 100.0);
        assert_eq!(
            button_icon(&build_toolbar(&bounds, &visible, SCREEN_H), TB_EYE),
            Icon::Eye
        );
        assert_eq!(
            button_icon(&build_toolbar(&bounds, &hidden, SCREEN_H), TB_EYE),
            Icon::EyeOff
        );
    }

    #[test]
    fn toolbar_without_video_has_no_play_pause_or_volume() {
        let p = build_toolbar(&aabb(960.0, 400.0, 200.0, 100.0), &plain(1.0), SCREEN_H);
        assert!(p.widget::<Button>(TB_PLAY_PAUSE).is_none());
        assert!(p.widget::<VolumeControl>(TB_VOLUME).is_none());
        assert!(p.widget::<Button>(TB_TIMELINE).is_none());
        assert_eq!(p.frame().w, TOOLBAR_WIDTH, "без видео — обычная ширина");
    }

    #[test]
    fn toolbar_video_adds_play_pause_and_volume_after_buttons() {
        let p = build_toolbar(
            &aabb(960.0, 400.0, 200.0, 100.0),
            &with_video(video_state(true, false, 70)),
            SCREEN_H,
        );
        assert_eq!(
            p.frame().w,
            TOOLBAR_WIDTH + TOOLBAR_VIDEO_EXTRA_W,
            "видео добавляет ширину"
        );
        assert_eq!(
            button_icon(&p, TB_PLAY_PAUSE),
            Icon::Play,
            "на паузе — играть"
        );
        let volume = p.widget::<VolumeControl>(TB_VOLUME).unwrap();
        assert_eq!(volume.value(), 70);
        // Видео-виджеты правее последней обычной кнопки (TB_DELETE).
        let delete_cx = p.widget::<Button>(TB_DELETE).unwrap().bounds().cx;
        let play_cx = p.widget::<Button>(TB_PLAY_PAUSE).unwrap().bounds().cx;
        let volume_cx = volume.bounds().cx;
        assert!(delete_cx < play_cx, "играть/пауза правее удалить");
        assert!(play_cx < volume_cx, "громкость правее играть/пауза");
    }

    /// Громкость — динамик размером с кнопку, а не 96-DIP ползунок: тулбар
    /// с видео сузился ровно на разницу (репорт 2026-09-06).
    #[test]
    fn video_widgets_are_three_button_squares() {
        assert_eq!(
            TOOLBAR_VIDEO_EXTRA_W,
            3.0 * (theme::BUTTON_SIZE + TOOLBAR_WIDGET_GAP)
        );
        let p = build_toolbar(
            &aabb(960.0, 400.0, 200.0, 100.0),
            &with_video(video_state(false, false, 50)),
            SCREEN_H,
        );
        assert_eq!(
            p.widget::<VolumeControl>(TB_VOLUME).unwrap().bounds().w,
            theme::BUTTON_SIZE
        );
    }

    /// Иконка динамика отражает уровень, а не только факт наличия звука:
    /// «интуитивно непонятно, что этот ползунок означает» (2026-09-06).
    #[test]
    fn volume_icon_reflects_level_and_mute() {
        let cases = [
            (0, false, Icon::VolumeMute),
            (10, false, Icon::VolumeLow),
            (49, false, Icon::VolumeLow),
            (50, false, Icon::VolumeHigh),
            (100, false, Icon::VolumeHigh),
            (80, true, Icon::VolumeMute),
        ];
        for (pct, muted, expected) in cases {
            let state = with_video(VideoToolbarState {
                paused: false,
                show_timeline: false,
                volume_pct: pct,
                muted,
            });
            let p = build_toolbar(&aabb(960.0, 400.0, 200.0, 100.0), &state, SCREEN_H);
            assert_eq!(
                p.widget::<VolumeControl>(TB_VOLUME).unwrap().icon(),
                expected,
                "{pct}% muted={muted}"
            );
        }
    }

    /// Переключатель полосы перемотки: есть только у видео, иконка
    /// отражает состояние в обе стороны, а сам он стоит между
    /// «играть/пауза» и громкостью (запрос пользователя 2026-08-22).
    #[test]
    fn toolbar_timeline_toggle_reflects_state_and_order() {
        for show in [false, true] {
            let p = build_toolbar(
                &aabb(960.0, 400.0, 200.0, 100.0),
                &with_video(video_state(false, show, 50)),
                SCREEN_H,
            );
            assert_eq!(
                button_icon(&p, TB_TIMELINE),
                if show {
                    Icon::Timeline
                } else {
                    Icon::TimelineOff
                },
                "иконка отражает состояние настройки"
            );
            let play_cx = p.widget::<Button>(TB_PLAY_PAUSE).unwrap().bounds().cx;
            let timeline_cx = p.widget::<Button>(TB_TIMELINE).unwrap().bounds().cx;
            let volume_cx = p.widget::<VolumeControl>(TB_VOLUME).unwrap().bounds().cx;
            assert!(
                play_cx < timeline_cx && timeline_cx < volume_cx,
                "порядок: играть/пауза -> таймлайн -> громкость"
            );
        }
    }

    /// Включённый переключатель НАЛИТ светом, выключенный — нет: до
    /// 2026-09-06 состояния отличались только тоном рисунка, и выключенное
    /// читалось как «недоступно» (дословная жалоба пользователя).
    #[test]
    fn timeline_toggle_is_lit_when_on() {
        let lit = |show: bool| {
            let p = build_toolbar(
                &aabb(960.0, 400.0, 200.0, 100.0),
                &with_video(video_state(false, show, 50)),
                SCREEN_H,
            );
            let mut out = Vec::new();
            p.widget::<Button>(TB_TIMELINE).unwrap().draw(&mut out);
            out.iter()
                .filter(|prim| {
                    matches!(
                        prim,
                        Primitive::Glass {
                            surface: rst_render::glass::Surface::ControlOn,
                            ..
                        }
                    )
                })
                .count()
        };
        assert_eq!(lit(true), 1, "включённый переключатель налит светом");
        assert_eq!(lit(false), 0, "выключенный — обычное стекло кнопки");
    }

    #[test]
    fn toolbar_video_playing_shows_pause_icon() {
        let p = build_toolbar(
            &aabb(960.0, 400.0, 200.0, 100.0),
            &with_video(video_state(false, false, 100)),
            SCREEN_H,
        );
        assert_eq!(
            button_icon(&p, TB_PLAY_PAUSE),
            Icon::Pause,
            "играет — пауза"
        );
    }
}
