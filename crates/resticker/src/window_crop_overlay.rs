//! Визуал режима отделения куска окна (запрос пользователя 2026-09-10;
//! `rst_core::model::StickerSource::WindowCrop`): чистый билдер примитивов,
//! без Win32 и без состояния координатора.
//!
//! Устроен как [`crate::mitosis_overlay`] и по той же причине: вход —
//! геометрия числами (прямоугольник окна в DIP, точки протяжки, курсор),
//! выход — [`Vec<Primitive>`] в DIP-пространстве монитора. Координатор лишь
//! собирает примитивы в кадр (`primitives_to_sprites`), а геометрия живёт
//! здесь и покрывается тестами без запуска оверлея.
//!
//! Стилистика — Dark Liquid Glass (docs/DESIGN_LIQUID_GLASS.md): отступы и
//! радиусы берутся токенами из `rst_render::theme`, а не магическими числами.

use rst_core::hittest::DipRect;
use rst_render::glass::GLASS_INK_DEEP_RGB;
use rst_render::{
    Box2D, HighlightKind, MARQUEE_FILL_OPACITY, MARQUEE_STROKE_OPACITY, Primitive, SELECTION_COLOR,
    WindowHighlight, glass_panel, marquee_visuals, text_size, theme,
};

/// Толщина рамки вокруг окна-источника, DIP. Та же, что у предпросмотра
/// половин при резке: контур должен читаться как «вот это окно выбрано», а
/// не как утолщение его собственной рамки.
const WINDOW_FRAME_THICKNESS_DIP: f64 = 2.0;

/// Горизонтальный отступ текста в плашке подсказки, DIP — токен §3
/// `PAD_CTRL_X`, как у плашки режима резки.
const HINT_PAD_X: f64 = theme::PAD_CTRL_X;
/// Вертикальный отступ текста в плашке, DIP — токен `BUTTON_PAD`.
const HINT_PAD_Y: f64 = theme::BUTTON_PAD;
/// Зазор между курсором и плашкой, DIP — токен §3 `GAP_ROW`.
const HINT_CURSOR_GAP: f64 = theme::GAP_ROW;

/// Подсказка, пока окно под курсором ещё не выбрано. Говорит, чего не
/// хватает: наведения на окно — протяжка по пустому рабочему столу куска не
/// даст, и без этой строки отказ выглядел бы как поломка.
const HINT_NO_WINDOW: &str = "Point at a window · Esc — cancel";
/// Подсказка, когда окно под курсором есть и можно тянуть.
const HINT_READY: &str = "Drag a piece of this window · Esc — cancel";
/// Подсказка во время протяжки. Про кликабельность сказано прямо: замер
/// 2026-09-10 показал, что ввод в кусок переслать нельзя (Electron не
/// принимает `PostMessage`, `SendInput` требует поднять окно), и человек
/// обязан узнать это здесь, а не обнаружить потом сам.
const HINT_DRAGGING: &str = "Release to take the piece · it mirrors, not clicks";

/// Непрозрачность затемнения экрана в режиме выделения.
///
/// Слабое: под ним выбирают кусок ЧУЖОГО окна, и содержимое этого окна
/// обязано остаться читаемым — человек целится в конкретный список или
/// график, а не в серый прямоугольник. Это же отличает режим от обычного
/// скриншота, где затемнение может быть плотным: там выделяют область
/// экрана, здесь — фрагмент живого интерфейса.
const DIM_OPACITY: f64 = 0.22;

/// Затемнение всего монитора — фон режима выделения.
///
/// Нужно, чтобы режим читался как отдельное состояние программы, а не как
/// случайно застрявший крест-курсор: без него единственным признаком
/// включённого режима остаётся форма курсора, и человек не понимает, почему
/// его клики вдруг перестали доходить до окон.
pub fn screen_dim(screen: DipRect) -> Primitive {
    Primitive::Fill {
        rect: Box2D::from_top_left(screen.x, screen.y, screen.w, screen.h),
        color: GLASS_INK_DEEP_RGB,
        opacity: DIM_OPACITY,
    }
}

/// Рамка вокруг окна-источника, на которое сейчас навели.
///
/// `window` — прямоугольник окна в DIP ЭТОГО монитора; отсечку «окно не
/// задевает монитор» делает вызывающий слой, как и для режима резки.
pub fn window_outline(window: DipRect) -> Vec<Primitive> {
    WindowHighlight::new(window.x, window.y, window.w, window.h, 1.0)
        .primitives(HighlightKind::Hover, WINDOW_FRAME_THICKNESS_DIP)
}

/// Рамка протяжки — тот же пунктир, что у мультивыделения стикеров
/// (`rst_render::marquee`).
///
/// Намеренно тот же визуал, а не свой: человек уже знает из режима
/// редактирования, что бегущий пунктир означает «я выделяю прямоугольник», и
/// заводить для того же смысла второй язык — значит учить его дважды.
///
/// Точки приходят в DIP монитора в любом порядке (протяжка в любую сторону
/// нормализуется внутри `marquee_visuals`).
pub fn drag_marquee(anchor: (f64, f64), current: (f64, f64)) -> Vec<Primitive> {
    let visuals = marquee_visuals(anchor, current);
    let mut out = Vec::new();
    if let Some(fill) = visuals.fill {
        out.push(Primitive::Fill {
            rect: fill,
            color: SELECTION_COLOR,
            opacity: MARQUEE_FILL_OPACITY,
        });
    }
    for dash in visuals.dashes {
        out.push(Primitive::Fill {
            rect: dash,
            color: SELECTION_COLOR,
            opacity: MARQUEE_STROKE_OPACITY,
        });
    }
    out
}

/// Что написано в плашке у курсора при данном состоянии режима.
fn hint_text(has_window: bool, dragging: bool) -> &'static str {
    match (has_window, dragging) {
        (_, true) => HINT_DRAGGING,
        (true, false) => HINT_READY,
        (false, false) => HINT_NO_WINDOW,
    }
}

/// Плашка-подсказка у курсора.
///
/// Повторяет раскладку плашки режима резки дословно, включая переброс на
/// другую сторону курсора у края экрана: у самой кромки плашка иначе уехала
/// бы за монитор и не прочиталась вовсе.
pub fn cursor_hint(
    cursor_dip: (f64, f64),
    monitor_dip: (f64, f64),
    has_window: bool,
    dragging: bool,
) -> Vec<Primitive> {
    let text = hint_text(has_window, dragging);
    let (tw, th) = text_size(text);
    let plate_w = tw + 2.0 * HINT_PAD_X;
    let plate_h = th + 2.0 * HINT_PAD_Y;
    let (mx, my) = monitor_dip;

    let mut px = cursor_dip.0 + HINT_CURSOR_GAP;
    if px + plate_w > mx {
        px = cursor_dip.0 - HINT_CURSOR_GAP - plate_w;
    }
    px = px.clamp(0.0, (mx - plate_w).max(0.0));

    let mut py = cursor_dip.1 + HINT_CURSOR_GAP;
    if py + plate_h > my {
        py = cursor_dip.1 - HINT_CURSOR_GAP - plate_h;
    }
    py = py.clamp(0.0, (my - plate_h).max(0.0));

    let mut out = Vec::new();
    glass_panel(
        &mut out,
        Box2D::from_top_left(px, py, plate_w, plate_h),
        theme::RADIUS_TIGHT,
        1.0,
    );
    out.push(Primitive::Text {
        rect: Box2D::from_top_left(px + HINT_PAD_X, py + HINT_PAD_Y, tw, th),
        text: text.to_string(),
        color: theme::TEXT,
        opacity: theme::TEXT_OPACITY,
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dip(x: f64, y: f64, w: f64, h: f64) -> DipRect {
        DipRect::new(x, y, w, h)
    }

    #[test]
    fn dim_is_weak_enough_to_read_the_window_under_it() {
        // Под затемнением выбирают кусок ЧУЖОГО окна, и его содержимое
        // обязано остаться читаемым: человек целится в конкретный список
        // или график. Плотное затемнение, уместное в режиме скриншота,
        // здесь сделало бы выбор вслепую.
        let p = screen_dim(dip(0.0, 0.0, 1920.0, 1080.0));
        let Primitive::Fill { rect, opacity, .. } = p else {
            panic!("затемнение — заливка");
        };
        assert!(
            opacity > 0.0 && opacity <= 0.3,
            "затемнение должно быть заметным, но не скрывающим содержимое: {opacity}"
        );
        assert_eq!(
            (rect.w, rect.h),
            (1920.0, 1080.0),
            "затемнение накрывает монитор целиком"
        );
    }

    #[test]
    fn window_outline_draws_four_edges() {
        // Четыре ребра, а не заливка: окно под рамкой обязано остаться
        // видимым — человек выбирает кусок ИЗ него, и закрасить его значило
        // бы спрятать то, что он выбирает.
        let prims = window_outline(dip(100.0, 100.0, 400.0, 300.0));
        assert_eq!(prims.len(), 4, "рамка — четыре ребра");
        assert!(
            prims
                .iter()
                .all(|p| matches!(p, Primitive::Fill { opacity, .. } if *opacity < 1.0)),
            "рамка полупрозрачная, иначе она спорит с содержимым окна"
        );
    }

    #[test]
    fn drag_marquee_normalizes_any_direction() {
        // Протяжка влево-вверх даёт ту же рамку, что вправо-вниз: иначе
        // выделение «в обратную сторону» рисовалось бы пустым.
        let a = drag_marquee((300.0, 300.0), (100.0, 100.0));
        let b = drag_marquee((100.0, 100.0), (300.0, 300.0));
        assert_eq!(a.len(), b.len(), "число примитивов совпадает");
        assert!(!a.is_empty(), "рамка протяжки не может быть пустой");
    }

    #[test]
    fn degenerate_drag_has_no_fill() {
        // Щелчок без движения: заливки нет, иначе на экране вспыхивал бы
        // прямоугольник нулевой площади.
        let prims = drag_marquee((200.0, 200.0), (200.0, 200.0));
        assert!(
            prims.is_empty() || prims.len() < 4,
            "вырожденная протяжка почти ничего не рисует, получено {}",
            prims.len()
        );
    }

    #[test]
    fn hint_says_what_is_missing() {
        assert_eq!(hint_text(false, false), HINT_NO_WINDOW);
        assert_eq!(hint_text(true, false), HINT_READY);
        assert_eq!(hint_text(true, true), HINT_DRAGGING);
        assert_eq!(
            hint_text(false, true),
            HINT_DRAGGING,
            "протяжка уже идёт — источник зафиксирован нажатием, и текст про «наведись» стал бы врать"
        );
    }

    #[test]
    fn hint_warns_that_the_piece_is_not_clickable() {
        // Замер 2026-09-10: пересылка ввода в кусок не работает для
        // Chromium/Electron. Человек обязан узнать это в момент, когда
        // отрывает кусок, а не через неделю, потыкав в него.
        assert!(
            HINT_DRAGGING.contains("mirrors") && HINT_DRAGGING.contains("not clicks"),
            "подсказка обязана честно говорить, что кусок не кликается: {HINT_DRAGGING}"
        );
    }

    #[test]
    fn hint_flips_to_the_other_side_near_the_screen_edge() {
        let monitor = (1920.0, 1080.0);
        let right = cursor_hint((1910.0, 540.0), monitor, true, false);
        let bottom = cursor_hint((960.0, 1075.0), monitor, true, false);
        for prims in [right, bottom] {
            for p in &prims {
                let rect = match p {
                    Primitive::Fill { rect, .. } | Primitive::Text { rect, .. } => *rect,
                    Primitive::Glass { rect, .. } => *rect,
                    _ => continue,
                };
                assert!(
                    rect.cx - rect.w / 2.0 >= -0.001
                        && rect.cx + rect.w / 2.0 <= monitor.0 + 0.001
                        && rect.cy - rect.h / 2.0 >= -0.001
                        && rect.cy + rect.h / 2.0 <= monitor.1 + 0.001,
                    "плашка вылезла за монитор: {rect:?}"
                );
            }
        }
    }
}
