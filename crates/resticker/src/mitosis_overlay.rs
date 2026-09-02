//! Визуал режима резки окна (M9, «Митоз окон» — docs/M9_WINDOW_MITOSIS_DESIGN.md
//! §4.3): чистый билдер примитивов предпросмотра разреза, без Win32 и без
//! состояния координатора.
//!
//! Вход — геометрия числами (прямоугольник окна в DIP, ось, доля разреза,
//! координаты курсора), выход — [`Vec<Primitive>`] в DIP-пространстве
//! монитора. Это сознательное отделение от `overlay_manager.rs`: координатор
//! лишь собирает примитивы в кадр (`primitives_to_sprites`), а вся геометрия
//! и отказ-стилистика живут здесь и покрываются тестами без запуска оверлея.
//!
//! Стилистика — Dark Liquid Glass (docs/DESIGN_LIQUID_GLASS.md): отступы,
//! радиусы и непрозрачности текста берутся токенами из `rst_render::theme`,
//! а не магическими числами. Единственные цветные акценты здесь —
//! бирюза [`SELECTION_COLOR`] (тот же акцент, что рамка выделения) и красный
//! `theme::DANGER` (предпросмотр отказа, см. [`split_preview`]).

use rst_core::hittest::DipRect;
use rst_core::mitosis::SplitAxis;
use rst_render::{
    Box2D, Primitive, SELECTION_COLOR, WindowHighlight, glass_panel, text_size, theme,
};

/// Толщина линии разреза, DIP (задана задачей — 2 DIP): поперёк окна должна
/// читаться как место будущего шва, а не как утолщение рамки.
const CUT_THICKNESS_DIP: f64 = 2.0;
/// Толщина рамки каждой из двух будущих половин, DIP. Та же, что у линии
/// разреза: половин должно читаться «два окна», а не «второй контур вокруг
/// одного».
const PREVIEW_FRAME_THICKNESS_DIP: f64 = 2.0;
/// Непрозрачность рамки половины: полупрозрачная, чтобы окно под ней
/// оставалось видимым (тот же принцип, что `HighlightKind::Hover`).
const PREVIEW_FRAME_OPACITY: f64 = 0.8;
/// Непрозрачность линии разреза: почти сплошная — это единственный элемент,
/// на который пользователь нацеливает клик.
const CUT_OPACITY: f64 = 0.9;
/// Доля окна от каждого края режущей оси, куда разрез поставить нельзя.
/// Совпадает с `rst_core::mitosis::MAX_OFFSET_FRAC` (0.25) — визуал обязан
/// показывать ровно те запретные зоны, в которые `split_fraction` не пустит
/// долю; расхождение означало бы, что предпросмотр и реальный разрез
/// противоречат друг другу.
const FORBIDDEN_EDGE_FRAC: f64 = 0.25;
/// Непрозрачность полосы запретной зоны: приглушённая тёмная заливка, чтобы
/// не спорить с рамками и линией, но читаться как «сюда резать нельзя».
const FORBIDDEN_OPACITY: f64 = 0.30;

/// Горизонтальный отступ текста в плашке подсказки, DIP — токен §3
/// (`PAD_CTRL_X`, отступ подписи в кнопке): плашка — маленькая кнопкоподобная
/// плата, и её текст садится тем же полем, что у контрола.
const HINT_PAD_X: f64 = theme::PAD_CTRL_X;
/// Вертикальный отступ текста в плашке, DIP — токен `BUTTON_PAD` (отступ
/// иконки в кнопке): вертикали нужно меньше, чем горизонтали, однострочная
/// плашка не должна распухать в высоту.
const HINT_PAD_Y: f64 = theme::BUTTON_PAD;
/// Зазор между курсором и плашкой подсказки, DIP — токен §3 `GAP_ROW`:
/// плашка «висит» рядом с курсором на том же расстоянии, что строки друг от
/// друга, и не наезжает на курсор.
const HINT_CURSOR_GAP: f64 = theme::GAP_ROW;
/// Текст подсказки у курсора. Про ось здесь не сказано ни слова намеренно:
/// она выбирается сама по тому, к какой стороне окна ушёл курсор
/// (`rst_core::mitosis::axis_for_cursor`), и объяснять словами то, что видно
/// на предпросмотре при первом же движении мыши, — лишний шум. Осталось
/// только то, что действием не показать: как выйти.
///
/// Английский, как и весь нативный слой resticker (i18n.rs, доккомент модуля).
const HINT_TEXT: &str = "Esc — cancel";

/// Две половины разреза: `.0` — оригинал (левая/верхняя), `.1` — клон
/// (правая/нижняя). `fraction` зажимается в `[0, 1]`, чтобы разрез гарантированно
/// остался внутри окна (входное значение уже зажато `split_fraction`, но
/// билдер сам защищается от вырожденных входов — плавающая арифметика у
/// вызывающего слоя могла бы дать чуть выпадающую долю).
fn halves(rect: DipRect, axis: SplitAxis, fraction: f64) -> (DipRect, DipRect) {
    let f = fraction.clamp(0.0, 1.0);
    match axis {
        SplitAxis::Vertical => {
            let cut = rect.x + rect.w * f;
            (
                DipRect::new(rect.x, rect.y, cut - rect.x, rect.h),
                DipRect::new(cut, rect.y, rect.x + rect.w - cut, rect.h),
            )
        }
        SplitAxis::Horizontal => {
            let cut = rect.y + rect.h * f;
            (
                DipRect::new(rect.x, rect.y, rect.w, cut - rect.y),
                DipRect::new(rect.x, cut, rect.w, rect.y + rect.h - cut),
            )
        }
    }
}

/// Прямоугольник линии разреза: тонкая полоса поперёк всего окна на доле
/// `fraction` вдоль режущей оси.
fn cut_line_rect(rect: DipRect, axis: SplitAxis, fraction: f64) -> Box2D {
    let f = fraction.clamp(0.0, 1.0);
    match axis {
        SplitAxis::Vertical => Box2D::from_center(
            rect.x + rect.w * f,
            rect.y + rect.h / 2.0,
            CUT_THICKNESS_DIP,
            rect.h,
        ),
        SplitAxis::Horizontal => Box2D::from_center(
            rect.x + rect.w / 2.0,
            rect.y + rect.h * f,
            rect.w,
            CUT_THICKNESS_DIP,
        ),
    }
}

/// Приглушённые полосы запретных зон: по 25 % окна с каждого края режущей оси
/// (см. [`FORBIDDEN_EDGE_FRAC`]). Полупрозрачная тёмная заливка (`PANEL_BG`) —
/// она глушит фон, но не перетягивает внимание с рамок и линии разреза.
fn push_forbidden_bands(out: &mut Vec<Primitive>, rect: DipRect, axis: SplitAxis) {
    let mut band = |b: DipRect| {
        out.push(Primitive::Fill {
            rect: Box2D::from_top_left(b.x, b.y, b.w, b.h),
            color: theme::PANEL_BG,
            opacity: FORBIDDEN_OPACITY,
        });
    };
    match axis {
        SplitAxis::Vertical => {
            let bw = rect.w * FORBIDDEN_EDGE_FRAC;
            band(DipRect::new(rect.x, rect.y, bw, rect.h));
            band(DipRect::new(rect.x + rect.w - bw, rect.y, bw, rect.h));
        }
        SplitAxis::Horizontal => {
            let bh = rect.h * FORBIDDEN_EDGE_FRAC;
            band(DipRect::new(rect.x, rect.y, rect.w, bh));
            band(DipRect::new(rect.x, rect.y + rect.h - bh, rect.w, bh));
        }
    }
}

/// Рамка одной половины через [`WindowHighlight::primitives_custom`] — те же
/// четыре ребра, что у обычной подсветки окна, но с цветом и непрозрачностью
/// предпросмотра разреза. Масштаб 1.0: вход уже в DIP, и рендерер умножит
/// геометрию на масштаб монитора, как и всё остальное.
fn push_half_frame(out: &mut Vec<Primitive>, half: DipRect, color: [u8; 3]) {
    let highlight = WindowHighlight::new(half.x, half.y, half.w, half.h, 1.0);
    out.extend(highlight.primitives_custom(
        color,
        PREVIEW_FRAME_OPACITY,
        PREVIEW_FRAME_THICKNESS_DIP,
    ));
}

/// Примитивы предпросмотра разреза в DIP-пространстве монитора.
/// `rect` — окно под курсором, `axis`/`fraction` — текущий разрез,
/// `blocked` — предпросмотр отказа (половина слишком мала, `TooSmall`):
/// тогда всё рисуется красным, чтобы отказ читался до клика, а не после.
///
/// Композиция (снизу вверх): запретные полосы, рамки обеих половин, линия
/// разреза поверх — на неё пользователь нацеливает клик, она и должна быть
/// самым контрастным элементом.
pub fn split_preview(
    rect: DipRect,
    axis: SplitAxis,
    fraction: f64,
    blocked: bool,
) -> Vec<Primitive> {
    // Цвет отказа — красный DANGER, то же значение, что у `PIN_FLASH_COLOR_UNPIN`
    // в overlay_manager.rs (#D0463C): «нельзя» во всём приложении говорит одним
    // цветом. Обычное состояние — акцент проекта SELECTION_COLOR (#3c9898).
    let color = if blocked {
        theme::DANGER
    } else {
        SELECTION_COLOR
    };
    let (first, second) = halves(rect, axis, fraction);
    let mut out = Vec::new();
    push_forbidden_bands(&mut out, rect, axis);
    push_half_frame(&mut out, first, color);
    push_half_frame(&mut out, second, color);
    out.push(Primitive::Fill {
        rect: cut_line_rect(rect, axis, fraction),
        color,
        opacity: CUT_OPACITY,
    });
    out
}

/// Подсказка у курсора: маленькая стеклянная плашка с текстом
/// «Esc — cancel» (см. [`HINT_TEXT`]). `cursor_dip` — позиция
/// курсора, `monitor_dip` — размер монитора в DIP (ширина, высота).
///
/// Плашка ОБЯЗАНА оставаться внутри монитора: она ставится правее и ниже
/// курсора, а при нехватке места справа/снизу переносится на другую сторону
/// курсора (влево/вверх). Плюс финальный кламп в границы монитора — защита от
/// плашки шире самого монитора и от выпадающей плавающей арифметики позиции.
pub fn cursor_hint(cursor_dip: (f64, f64), monitor_dip: (f64, f64)) -> Vec<Primitive> {
    let (tw, th) = text_size(HINT_TEXT);
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
    // Корпус плашки — плита чёрного стекла (§4) с малым радиусом (§3
    // RADIUS_TIGHT): плашка мелкая, большой радиус съел бы её целиком (тот же
    // случай, что тулбар стикера — §3, исключение).
    glass_panel(
        &mut out,
        Box2D::from_top_left(px, py, plate_w, plate_h),
        theme::RADIUS_TIGHT,
        1.0,
    );
    out.push(Primitive::Text {
        rect: Box2D::from_top_left(px + HINT_PAD_X, py + HINT_PAD_Y, tw, th),
        text: HINT_TEXT.to_string(),
        color: theme::TEXT,
        opacity: theme::TEXT_OPACITY,
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Линия разреза: единственный `Fill` толщиной [`CUT_THICKNESS_DIP`] во
    /// всю длину окна вдоль режущей оси И на непрозрачности [`CUT_OPACITY`].
    /// Проверка непрозрачности обязательна: у горизонтальной рамки верхней
    /// половины те же `w == ширина окна` и `h == CUT_THICKNESS_DIP` (она идёт
    /// по верхнему краю во всю ширину окна), а отличает линию от рамки
    /// именно непрозрачность — у рамок [`PREVIEW_FRAME_OPACITY`].
    fn find_cut(prims: &[Primitive], axis: SplitAxis, window_w: f64, window_h: f64) -> Box2D {
        prims
            .iter()
            .find_map(|p| match p {
                Primitive::Fill { rect, opacity, .. } if *opacity == CUT_OPACITY => match axis {
                    SplitAxis::Vertical
                        if rect.rotation == 0.0
                            && rect.w == CUT_THICKNESS_DIP
                            && rect.h == window_h =>
                    {
                        Some(*rect)
                    }
                    SplitAxis::Horizontal
                        if rect.rotation == 0.0
                            && rect.h == CUT_THICKNESS_DIP
                            && rect.w == window_w =>
                    {
                        Some(*rect)
                    }
                    _ => None,
                },
                _ => None,
            })
            .expect("линия разреза")
    }

    /// Рамка стеклянной плашки подсказки.
    fn glass_rect(prims: &[Primitive]) -> Box2D {
        prims
            .iter()
            .find_map(|p| match p {
                Primitive::Glass { rect, .. } => Some(*rect),
                _ => None,
            })
            .expect("стеклянная плашка")
    }

    /// Цвета рамок половин: `Fill` на непрозрачности [`PREVIEW_FRAME_OPACITY`].
    fn frame_colors(prims: &[Primitive]) -> Vec<[u8; 3]> {
        prims
            .iter()
            .filter_map(|p| match p {
                Primitive::Fill {
                    rect,
                    color,
                    opacity,
                } if *opacity == PREVIEW_FRAME_OPACITY && rect.rotation == 0.0 => Some(*color),
                _ => None,
            })
            .collect()
    }

    /// Линия разреза лежит внутри окна на нужной доле для обеих осей.
    #[test]
    fn cut_line_lies_at_fraction_inside_window_for_both_axes() {
        let rect = DipRect::new(100.0, 50.0, 400.0, 200.0);
        let f = 0.3;

        let prims = split_preview(rect, SplitAxis::Vertical, f, false);
        let cut = find_cut(&prims, SplitAxis::Vertical, rect.w, rect.h);
        assert!(
            (cut.cx - (rect.x + rect.w * f)).abs() < 1e-9,
            "вертикальная линия на доле {f}"
        );
        assert!(
            cut.cx > rect.x && cut.cx < rect.x + rect.w,
            "линия внутри окна"
        );
        assert_eq!(cut.w, CUT_THICKNESS_DIP);
        assert_eq!(cut.h, rect.h, "вертикальная линия во всю высоту");

        let prims = split_preview(rect, SplitAxis::Horizontal, f, false);
        let cut = find_cut(&prims, SplitAxis::Horizontal, rect.w, rect.h);
        assert!(
            (cut.cy - (rect.y + rect.h * f)).abs() < 1e-9,
            "горизонтальная линия на доле {f}"
        );
        assert!(
            cut.cy > rect.y && cut.cy < rect.y + rect.h,
            "линия внутри окна"
        );
        assert_eq!(cut.h, CUT_THICKNESS_DIP);
        assert_eq!(cut.w, rect.w, "горизонтальная линия во всю ширину");
    }

    /// Доля, выходящая за границы, зажимается в окно (защита от вырожденных
    /// входов) — линия не уходит за край.
    #[test]
    fn out_of_range_fraction_is_clamped_inside_window() {
        let rect = DipRect::new(0.0, 0.0, 400.0, 200.0);
        let prims = split_preview(rect, SplitAxis::Vertical, 1.5, false);
        let cut = find_cut(&prims, SplitAxis::Vertical, rect.w, rect.h);
        assert!(
            cut.cx <= rect.x + rect.w + 1e-9,
            "доля >1 зажата в правый край"
        );
    }

    /// Рамки половин не пересекаются и стыкуются по режущей оси, сумма их
    /// размеров равна исходному окну — «будет два окна» без нахлёста и без
    /// потери пикселя.
    #[test]
    fn half_frames_do_not_overlap() {
        let rect = DipRect::new(100.0, 50.0, 400.0, 200.0);
        for axis in [SplitAxis::Vertical, SplitAxis::Horizontal] {
            let (a, b) = halves(rect, axis, 0.35);
            match axis {
                SplitAxis::Vertical => {
                    assert!(
                        (a.x + a.w - b.x).abs() < 1e-9,
                        "левый кончается там, где правый начинается"
                    );
                    assert_eq!(a.y, b.y);
                    assert_eq!(a.h, b.h);
                    assert!(
                        (a.w + b.w - rect.w).abs() < 1e-9,
                        "ширины в сумме дают окно"
                    );
                }
                SplitAxis::Horizontal => {
                    assert!(
                        (a.y + a.h - b.y).abs() < 1e-9,
                        "верхний кончается там, где нижний начинается"
                    );
                    assert_eq!(a.x, b.x);
                    assert_eq!(a.w, b.w);
                    assert!(
                        (a.h + b.h - rect.h).abs() < 1e-9,
                        "высоты в сумме дают окно"
                    );
                }
            }
        }
    }

    /// `blocked` меняет цвет рамок и линии на красный DANGER (значение
    /// `PIN_FLASH_COLOR_UNPIN`), обычное состояние — акцент проекта.
    #[test]
    fn blocked_switches_color_to_danger() {
        let rect = DipRect::new(0.0, 0.0, 400.0, 200.0);
        let normal = split_preview(rect, SplitAxis::Vertical, 0.5, false);
        let blocked = split_preview(rect, SplitAxis::Vertical, 0.5, true);

        assert!(frame_colors(&normal).iter().all(|c| *c == SELECTION_COLOR));
        assert!(frame_colors(&blocked).iter().all(|c| *c == theme::DANGER));
        assert_ne!(
            SELECTION_COLOR,
            theme::DANGER,
            "акцент и отказ — разные цвета"
        );

        let normal_cut = find_cut(&normal, SplitAxis::Vertical, rect.w, rect.h);
        let blocked_cut = find_cut(&blocked, SplitAxis::Vertical, rect.w, rect.h);
        // Цвет линии ищем в СВОЁМ векторе: у normal и blocked геометрия одна
        // и та же (та же доля/прямоугольник), и поиск по склейке векторов
        // нашёл бы первую (нормальную) линию для обоих прямоугольников.
        let color_of = |prims: &[Primitive], cut: Box2D| {
            prims
                .iter()
                .find_map(|p| match p {
                    Primitive::Fill { rect, color, .. } if *rect == cut => Some(*color),
                    _ => None,
                })
                .expect("линия разреза в своём векторе")
        };
        assert_eq!(color_of(&normal, normal_cut), SELECTION_COLOR);
        assert_eq!(color_of(&blocked, blocked_cut), theme::DANGER);
    }

    /// Запретные зоны — две полосы по 25 % с каждого края режущей оси,
    /// тёмные и приглушённые.
    #[test]
    fn forbidden_bands_cover_edges_along_cutting_axis() {
        let rect = DipRect::new(100.0, 50.0, 400.0, 200.0);
        let prims = split_preview(rect, SplitAxis::Vertical, 0.5, false);
        let bands: Vec<Box2D> = prims
            .iter()
            .filter_map(|p| match p {
                Primitive::Fill {
                    rect,
                    color,
                    opacity,
                } if *opacity == FORBIDDEN_OPACITY && *color == theme::PANEL_BG => Some(*rect),
                _ => None,
            })
            .collect();
        assert_eq!(bands.len(), 2, "две запретные полосы");
        let mut xs: Vec<f64> = bands.iter().map(|r| r.cx - r.w / 2.0).collect();
        xs.sort_by(f64::total_cmp);
        assert!(
            (xs[0] - rect.x).abs() < 1e-9,
            "первая полоса от левого края"
        );
        assert!((xs[1] - (rect.x + rect.w * (1.0 - FORBIDDEN_EDGE_FRAC))).abs() < 1e-9);
        for b in &bands {
            assert!(
                (b.w - rect.w * FORBIDDEN_EDGE_FRAC).abs() < 1e-9,
                "полоса 25 % ширины"
            );
            assert_eq!(b.h, rect.h, "во всю высоту");
        }
    }

    /// Плашка подсказки не вылезает за монитор ни в одном из четырёх углов.
    #[test]
    fn hint_stays_inside_monitor_at_all_four_corners() {
        let monitor = (1920.0, 1080.0);
        let corners = [(0.0, 0.0), (1920.0, 0.0), (0.0, 1080.0), (1920.0, 1080.0)];
        for (cx, cy) in corners {
            let prims = cursor_hint((cx, cy), monitor);
            let g = glass_rect(&prims);
            assert!(g.cx - g.w / 2.0 >= 0.0, "левый край в мониторе ({cx},{cy})");
            assert!(
                g.cx + g.w / 2.0 <= monitor.0,
                "правый край в мониторе ({cx},{cy})"
            );
            assert!(g.cy - g.h / 2.0 >= 0.0, "верх в мониторе ({cx},{cy})");
            assert!(g.cy + g.h / 2.0 <= monitor.1, "низ в мониторе ({cx},{cy})");
        }
    }

    /// При нехватке места справа плашка переносится влево от курсора, при
    /// нехватке снизу — вверх (и всё ещё внутри монитора).
    #[test]
    fn hint_flips_to_the_other_side_when_no_room() {
        let monitor = (800.0, 600.0);
        let right = cursor_hint((790.0, 50.0), monitor);
        let rg = glass_rect(&right);
        assert!(
            rg.cx + rg.w / 2.0 <= monitor.0,
            "не вылезает за правый край"
        );
        assert!(
            rg.cx + rg.w / 2.0 < 790.0,
            "плашка слева от курсора, когда справа нет места"
        );

        let bottom = cursor_hint((400.0, 590.0), monitor);
        let bg = glass_rect(&bottom);
        assert!(
            bg.cy + bg.h / 2.0 <= monitor.1,
            "не вылезает за нижний край"
        );
        assert!(
            bg.cy + bg.h / 2.0 < 590.0,
            "плашка выше курсора, когда снизу нет места"
        );
    }
}
