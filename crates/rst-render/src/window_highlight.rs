//! Подсветка окна под курсором (ROADMAP.md M6, «Режим выбора окна с
//! подсветкой под курсором»): полупрозрачная рамка вокруг экранного
//! прямоугольника окна.
//!
//! Вход — экранный прямоугольник окна в **физических** пикселях (`x/y` —
//! левый верхний угол, `w/h` — размер; такие координаты отдаёт слой
//! rst-win32) и DPI-масштаб монитора; выход — геометрия в DIP (ADR-010):
//! в физические пиксели её переводит рендерер умножением на масштаб
//! ([`crate::WindowTarget`]). rst-win32 сюда не подключается — модуль
//! принимает координаты числами.
//!
//! Модуль не создаёт текстур: он выдаёт прямоугольники [`Box2D`] тем же
//! паттерном, что рамка выделения M2 ([`crate::SelectionBox::outline_rects`]
//! — по одному прямоугольнику на ребро, центр прямоугольника лежит на
//! ребре, толщина поровну внутрь и наружу), которые вызывающий слой
//! превращает в `Sprite` через [`crate::solid_sprite`].
//!
//! Отдельный явный API для отрисовки — [`WindowHighlight::primitives`],
//! выдающий существующие [`Primitive::Fill`] (цвет/прозрачность из
//! [`HighlightKind`]). Новый вариант [`Primitive`] намеренно НЕ добавлен:
//! вызывающий слой (`overlay_manager.rs`) разбирает `Primitive` исчерпывающим
//! match, ломать его вне скоупа этой задачи нельзя.

use crate::selection::Box2D;
use crate::widgets::Primitive;

/// Толщина рамки подсветки окна, DIP (в физические пиксели переводит
/// рендерер — та же конвенция, что [`crate::OUTLINE_THICKNESS_DIP`]).
pub const HIGHLIGHT_THICKNESS_DIP: f64 = 3.0;

/// Цвет и прозрачность рамки подсветки (M6): пин — один цвет, обычная
/// подсветка при наведении — другой. Enum, не строка — цвета валидны
/// по построению, в шейдер/текстуру уходит готовый RGB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HighlightKind {
    /// Закреплённое (пин) окно: постоянная рамка.
    Pin,
    /// Обычная подсветка окна под курсором в режиме выбора.
    Hover,
}

impl HighlightKind {
    /// Цвет рамки, RGB.
    pub const fn color(self) -> [u8; 3] {
        match self {
            // Янтарный — пин.
            Self::Pin => [0xff, 0xb3, 0x00],
            // Акцент проекта (тот же синий, что SLIDER_FILL) — наведение.
            Self::Hover => [0x4f, 0x9c, 0xff],
        }
    }

    /// Непрозрачность рамки: полупрозрачная — окно под рамкой остаётся
    /// видимым (выбор окна без «закрашивания»).
    pub const fn opacity(self) -> f64 {
        match self {
            Self::Pin => 0.9,
            Self::Hover => 0.65,
        }
    }
}

/// Рамка подсветки экранного прямоугольника окна.
///
/// Геометрия считается лениво и чисто: состояние — только входные данные
/// (прямоугольник окна и масштаб монитора), всё остальное — функции.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowHighlight {
    /// Прямоугольник окна в физических пикселях: `x`/`y` — левый верхний
    /// угол, `w`/`h` — размер.
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    /// Масштаб монитора (физические пиксели на DIP) — `WindowTarget::dpi_scale`.
    dpi_scale: f64,
}

impl WindowHighlight {
    /// Рамка вокруг прямоугольника окна `(x, y, w, h)` — физические
    /// пиксели (координаты от слоя rst-win32). `dpi_scale` — масштаб
    /// монитора, на котором окно (перевод физических → DIP, ADR-010);
    /// обязан быть положительным (это константа рендерера, не данные ОС).
    /// Вырожденные `w`/`h` (≤ 0) допустимы — прямоугольники обводки
    /// получаются нулевого размера, рендерер их пропускает.
    pub fn new(x: f64, y: f64, w: f64, h: f64, dpi_scale: f64) -> Self {
        assert!(
            dpi_scale > 0.0,
            "dpi_scale обязан быть положительным, получено {dpi_scale}"
        );
        Self {
            x,
            y,
            w: w.max(0.0),
            h: h.max(0.0),
            dpi_scale,
        }
    }

    /// Прямоугольник окна в DIP: левый верхний угол и размер.
    fn dip_rect(&self) -> (f64, f64, f64, f64) {
        (
            self.x / self.dpi_scale,
            self.y / self.dpi_scale,
            self.w / self.dpi_scale,
            self.h / self.dpi_scale,
        )
    }

    /// Четыре ребра рамки как тонкие прямоугольники толщиной `thickness`
    /// (DIP), по часовой стрелке (верх, право, низ, лево) — тот же паттерн,
    /// что [`crate::SelectionBox::outline_rects`]: центр каждого
    /// прямоугольника лежит на ребре, толщина поровну внутрь и наружу.
    /// Выход — DIP; физические пиксели получает рендерер умножением на
    /// масштаб (ADR-010).
    pub fn outline_rects(&self, thickness_dip: f64) -> [Box2D; 4] {
        let thickness = thickness_dip.max(0.0);
        let (x, y, w, h) = self.dip_rect();
        [
            // Верхнее ребро.
            Box2D {
                cx: x + w / 2.0,
                cy: y,
                w,
                h: thickness,
                rotation: 0.0,
            },
            // Правое ребро.
            Box2D {
                cx: x + w,
                cy: y + h / 2.0,
                w: h,
                h: thickness,
                rotation: std::f64::consts::FRAC_PI_2,
            },
            // Нижнее ребро.
            Box2D {
                cx: x + w / 2.0,
                cy: y + h,
                w,
                h: thickness,
                rotation: std::f64::consts::PI,
            },
            // Левое ребро.
            Box2D {
                cx: x,
                cy: y + h / 2.0,
                w: h,
                h: thickness,
                rotation: -std::f64::consts::FRAC_PI_2,
            },
        ]
    }

    /// Примитивы отрисовки рамки: четыре [`Primitive::Fill`] по рёбрам
    /// (порядок — верх, право, низ, лево) с цветом и прозрачностью `kind`.
    /// Полупрозрачная заливка: вызывающий слой превращает их в спрайты тем
    /// же путём, что и остальные `Primitive` панелей (`Fill` → solid-текстура
    /// цвета → [`crate::solid_sprite`]).
    pub fn primitives(&self, kind: HighlightKind, thickness_dip: f64) -> Vec<Primitive> {
        self.outline_rects(thickness_dip)
            .into_iter()
            .map(|rect| Primitive::Fill {
                rect,
                color: kind.color(),
                opacity: kind.opacity(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::{FRAC_PI_2, PI};

    #[test]
    fn outline_rects_follow_edges_at_scale_one() {
        // Масштаб 1:1 — геометрия совпадает с физическими пикселями.
        let h = WindowHighlight::new(100.0, 80.0, 60.0, 40.0, 1.0);
        assert_eq!(
            h.outline_rects(2.0),
            [
                Box2D {
                    cx: 130.0,
                    cy: 80.0,
                    w: 60.0,
                    h: 2.0,
                    rotation: 0.0
                },
                Box2D {
                    cx: 160.0,
                    cy: 100.0,
                    w: 40.0,
                    h: 2.0,
                    rotation: FRAC_PI_2
                },
                Box2D {
                    cx: 130.0,
                    cy: 120.0,
                    w: 60.0,
                    h: 2.0,
                    rotation: PI
                },
                Box2D {
                    cx: 100.0,
                    cy: 100.0,
                    w: 40.0,
                    h: 2.0,
                    rotation: -FRAC_PI_2
                },
            ]
        );
    }

    #[test]
    fn outline_rects_convert_physical_to_dip() {
        // Масштаб 2.0 (125%—200% дисплеи): окно 1920×1080 физических →
        // 960×540 DIP; левый верхний угол (100, 200) → (50, 100).
        let h = WindowHighlight::new(100.0, 200.0, 1920.0, 1080.0, 2.0);
        let o = h.outline_rects(3.0);
        assert_eq!(
            o[0],
            Box2D {
                cx: 50.0 + 960.0 / 2.0,
                cy: 100.0,
                w: 960.0,
                h: 3.0,
                rotation: 0.0
            }
        );
        assert_eq!(
            o[1],
            Box2D {
                cx: 50.0 + 960.0,
                cy: 100.0 + 540.0 / 2.0,
                w: 540.0,
                h: 3.0,
                rotation: FRAC_PI_2
            }
        );
        // Толщина — в DIP: 3 DIP при масштабе 2 = 6 физических пикселей
        // (переводит рендерер).
        assert_eq!(o[0].h, 3.0);
    }

    #[test]
    fn outline_rects_non_integer_scale() {
        // Масштаб 1.25 (125%): физический прямоугольник делится на 1.25.
        let h = WindowHighlight::new(125.0, 250.0, 500.0, 300.0, 1.25);
        let o = h.outline_rects(2.0);
        assert_eq!(
            o[0],
            Box2D {
                cx: 100.0 + 200.0,
                cy: 200.0,
                w: 400.0,
                h: 2.0,
                rotation: 0.0
            }
        );
    }

    #[test]
    fn outline_rects_degenerate_window_is_graceful() {
        // Нулевой размер окна: прямоугольники обводки вырождаются в линии/
        // точки — рендерер пропускает спрайты нулевого размера
        // (draw_common: placement.w/h <= 0 → continue).
        let h = WindowHighlight::new(10.0, 10.0, 0.0, 0.0, 1.0);
        let o = h.outline_rects(2.0);
        assert_eq!(o[0].w, 0.0, "верхнее ребро нулевой ширины");
        assert_eq!(o[1].w, 0.0, "правое ребро нулевой длины");
        // Отрицательный размер не уходит в геометрию (клампится в 0).
        let h = WindowHighlight::new(10.0, 10.0, -5.0, -5.0, 1.0);
        assert_eq!(h.outline_rects(2.0)[0].w, 0.0);
    }

    #[test]
    fn outline_rects_zero_thickness_is_zero_height() {
        let h = WindowHighlight::new(0.0, 0.0, 100.0, 80.0, 1.0);
        let o = h.outline_rects(0.0);
        assert_eq!(o[0].h, 0.0, "нулевая толщина — линия, рендерер пропустит");
        // Отрицательная толщина клампится к 0, не ломает геометрию.
        assert_eq!(h.outline_rects(-1.0)[0].h, 0.0);
    }

    #[test]
    #[should_panic(expected = "dpi_scale")]
    fn zero_dpi_scale_is_rejected() {
        WindowHighlight::new(0.0, 0.0, 10.0, 10.0, 0.0);
    }

    #[test]
    fn default_thickness_matches_dip_contract() {
        let h = WindowHighlight::new(0.0, 0.0, 100.0, 100.0, 1.0);
        assert_eq!(h.outline_rects(HIGHLIGHT_THICKNESS_DIP)[0].h, 3.0);
    }

    #[test]
    fn highlight_kind_colors_are_distinct_enums() {
        // Минимум два цвета через enum — не строка: пин и наведение разные.
        assert_ne!(HighlightKind::Pin.color(), HighlightKind::Hover.color());
        assert_eq!(
            HighlightKind::Pin.color(),
            [0xff, 0xb3, 0x00],
            "пин — янтарный"
        );
        assert_eq!(
            HighlightKind::Hover.color(),
            [0x4f, 0x9c, 0xff],
            "наведение — акцентный синий проекта"
        );
        // Оба варианта полупрозрачны (окно под рамкой видно).
        assert!(HighlightKind::Pin.opacity() < 1.0);
        assert!(HighlightKind::Hover.opacity() < 1.0);
    }

    #[test]
    fn primitives_emit_fills_with_kind_style() {
        let h = WindowHighlight::new(100.0, 80.0, 60.0, 40.0, 1.0);
        let prims = h.primitives(HighlightKind::Hover, 2.0);
        assert_eq!(prims.len(), 4);
        let rects = h.outline_rects(2.0);
        for (i, p) in prims.iter().enumerate() {
            let Primitive::Fill {
                rect,
                color,
                opacity,
            } = p
            else {
                panic!("примитив {i} — Fill");
            };
            assert_eq!(rect, &rects[i]);
            assert_eq!(*color, HighlightKind::Hover.color());
            assert_eq!(*opacity, HighlightKind::Hover.opacity());
        }
        // Пин — другой цвет/прозрачность.
        let pin = h.primitives(HighlightKind::Pin, 2.0);
        let Primitive::Fill { color, .. } = &pin[0] else {
            panic!("пин — Fill");
        };
        assert_eq!(*color, HighlightKind::Pin.color());
    }

    #[test]
    fn primitives_match_solid_sprite_contract() {
        // Примитивы — обычные Fill с прямоугольниками Box2D: путь вызова
        // `solid_sprite` для них тот же, что у виджетов панелей.
        let h = WindowHighlight::new(0.0, 0.0, 640.0, 480.0, 1.0);
        for p in h.primitives(HighlightKind::Hover, HIGHLIGHT_THICKNESS_DIP) {
            let Primitive::Fill { rect, .. } = p else {
                panic!("ожидался Fill");
            };
            assert!(rect.w >= 0.0 && rect.h >= 0.0, "размеры неотрицательны");
        }
    }
}
