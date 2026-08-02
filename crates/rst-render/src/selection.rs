//! Визуалы выделения (ROADMAP.md M2): рамка выделения с 8 ручками ресайза,
//! полноэкранное затемнение режима редактирования и шахматка скрытых стикеров.
//!
//! Вся геометрия — в логических пикселях (DIP), экран-центричная (ADR-010):
//! в физические пиксели координаты превращает уже рендерер умножением на
//! масштаб монитора ([`Renderer::set_dpi_scale`]). Модуль не создаёт текстур —
//! он выдаёт прямоугольники [`Box2D`], которые вызывающий слой превращает в
//! `Sprite` через [`solid_sprite`].

use rst_core::model::{MonitorId, Placement, Transform};

use crate::sprite::Sprite;
use crate::texture::Texture;

// Единый тип ручек — в rst-core (docs/M2_INTEGRATION_REVIEW.md, §1);
// реэкспорт сохраняет прежний путь `selection::HandleKind`.
pub use rst_core::hittest::HandleKind;

/// Толщина рамки выделения, DIP.
pub const OUTLINE_THICKNESS_DIP: f64 = 2.0;

/// Сторона квадратной ручки ресайза, DIP.
pub const HANDLE_SIZE_DIP: f64 = 10.0;

/// Прозрачность полноэкранного затемнения режима редактирования (50% чёрного).
pub const EDIT_OVERLAY_OPACITY: f64 = 0.5;

/// Фуксия шахматки скрытых стикеров (ROADMAP.md M2), RGB.
pub const CHECKER_MAGENTA: [u8; 3] = [0xff, 0x00, 0xff];

/// Чёрный шахматки скрытых стикеров (ROADMAP.md M2), RGB.
pub const CHECKER_BLACK: [u8; 3] = [0x00, 0x00, 0x00];

/// Прямоугольник в DIP с поворотом: центр `(cx, cy)`, размер `(w, h)`,
/// поворот `rotation` в радианах. Координаты — логические, относительно левого
/// верхнего угла монитора (ADR-010); в физические пиксели их переводит
/// рендерер по `set_dpi_scale`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Box2D {
    pub cx: f64,
    pub cy: f64,
    pub w: f64,
    pub h: f64,
    pub rotation: f64,
}

impl Box2D {
    /// Прямоугольник по центру и размеру, без поворота.
    pub const fn from_center(cx: f64, cy: f64, w: f64, h: f64) -> Self {
        Self {
            cx,
            cy,
            w,
            h,
            rotation: 0.0,
        }
    }

    /// Прямоугольник по левому верхнему углу и размеру, без поворота.
    pub const fn from_top_left(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self::from_center(x + w / 2.0, y + h / 2.0, w, h)
    }
}

/// Визуалы рамки выделения: четыре ребра и восемь ручек.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SelectionVisuals {
    /// Четыре тонких прямоугольника по рёбрам рамки, по часовой стрелке
    /// (верх, право, низ, лево).
    pub outline: [Box2D; 4],
    /// Восемь ручек, в порядке [`HandleKind`] от северо-западной.
    pub handles: [(HandleKind, Box2D); 8],
}

/// Геометрия рамки выделения стикера: центр, размер и поворот.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SelectionBox {
    center: (f64, f64),
    size: (f64, f64),
    rotation: f64,
}

impl SelectionBox {
    /// Собрать рамку выделения из модели стикера: центр и размер — из
    /// `placement` (DIP, ADR-010), поворот — из `transform`. Отражения
    /// (`flip_h`/`flip_v`) и прозрачность на границы рамки не влияют.
    pub fn new(placement: &Placement, transform: &Transform) -> Self {
        Self {
            center: (placement.cx, placement.cy),
            size: (placement.w, placement.h),
            rotation: transform.rotation,
        }
    }

    /// Центр рамки в DIP.
    pub fn center(&self) -> (f64, f64) {
        self.center
    }

    /// Размер рамки в DIP.
    pub fn size(&self) -> (f64, f64) {
        self.size
    }

    /// Поворот рамки в радианах.
    pub fn rotation(&self) -> f64 {
        self.rotation
    }

    /// Локальную точку повернуть и сдвинуть к центру рамки (в DIP).
    fn rotate(&self, (lx, ly): (f64, f64)) -> (f64, f64) {
        let (sin, cos) = self.rotation.sin_cos();
        (
            self.center.0 + lx * cos - ly * sin,
            self.center.1 + lx * sin + ly * cos,
        )
    }

    /// Четыре угла рамки в DIP, по часовой стрелке от левого верхнего
    /// в локальных координатах.
    pub fn corners(&self) -> [(f64, f64); 4] {
        let half = (self.size.0 / 2.0, self.size.1 / 2.0);
        [
            self.rotate((-half.0, -half.1)),
            self.rotate((half.0, -half.1)),
            self.rotate((half.0, half.1)),
            self.rotate((-half.0, half.1)),
        ]
    }

    /// Центр ручки `kind` в DIP.
    pub fn handle_center(&self, kind: HandleKind) -> (f64, f64) {
        let (sx, sy) = kind.local_sign();
        self.rotate((sx * self.size.0 / 2.0, sy * self.size.1 / 2.0))
    }

    /// Четыре ребра рамки как тонкие прямоугольники толщиной `thickness`
    /// (DIP): каждый выровнен по своему ребру и повёрнут вместе с рамкой.
    pub fn outline_rects(&self, thickness: f64) -> [Box2D; 4] {
        let c = self.corners();
        [(c[0], c[1]), (c[1], c[2]), (c[2], c[3]), (c[3], c[0])].map(|(a, b)| {
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            Box2D {
                cx: (a.0 + b.0) / 2.0,
                cy: (a.1 + b.1) / 2.0,
                w: dx.hypot(dy),
                h: thickness,
                rotation: dy.atan2(dx),
            }
        })
    }

    /// Восемь ручек: квадраты стороной `size` (DIP) по углам и серединам
    /// рёбер, ориентированные по осям экрана (не поворачиваются с рамкой).
    pub fn handle_rects(&self, size: f64) -> [(HandleKind, Box2D); 8] {
        HandleKind::ALL.map(|kind| {
            let (cx, cy) = self.handle_center(kind);
            (
                kind,
                Box2D {
                    cx,
                    cy,
                    w: size,
                    h: size,
                    rotation: 0.0,
                },
            )
        })
    }

    /// Полный набор визуалов выделения с толщинами по умолчанию.
    pub fn visuals(&self) -> SelectionVisuals {
        SelectionVisuals {
            outline: self.outline_rects(OUTLINE_THICKNESS_DIP),
            handles: self.handle_rects(HANDLE_SIZE_DIP),
        }
    }

    /// Все прямоугольники визуалов в порядке отрисовки: четыре ребра,
    /// затем восемь ручек.
    pub fn all_rects(&self) -> Vec<Box2D> {
        let v = self.visuals();
        let mut out: Vec<Box2D> = Vec::with_capacity(v.outline.len() + v.handles.len());
        out.extend_from_slice(&v.outline);
        out.extend(v.handles.into_iter().map(|(_, r)| r));
        out
    }
}

/// Прямоугольник полноэкранного затемнения режима редактирования в DIP для
/// экрана `w`×`h` (логические пиксели). Рисуется первым (нижним) слоем кадра
/// с прозрачностью [`EDIT_OVERLAY_OPACITY`], чтобы стикеры оставались яркими.
pub fn edit_overlay(screen_w_dip: f64, screen_h_dip: f64) -> Box2D {
    Box2D {
        cx: screen_w_dip / 2.0,
        cy: screen_h_dip / 2.0,
        w: screen_w_dip,
        h: screen_h_dip,
        rotation: 0.0,
    }
}

/// Сгенерировать RGBA-пиксели шахматки (straight alpha) для текстуры
/// `size`×`size` с клетками `cell` пикселей: фуксия [`CHECKER_MAGENTA`] и
/// чёрный [`CHECKER_BLACK`], левый верхний угол — фуксия. Массив подаётся в
/// `Renderer::create_texture_from_rgba` и рисуется поверх скрытого стикера
/// (ROADMAP.md M2: «Шахматка для скрытых стикеров»).
pub fn checkerboard_tile(cell: u32, size: u32) -> Vec<u8> {
    assert!(cell > 0, "размер клетки шахматки должен быть больше нуля");
    let mut out = Vec::with_capacity(size as usize * size as usize * 4);
    for y in 0..size {
        for x in 0..size {
            let color = if (x / cell + y / cell) % 2 == 0 {
                CHECKER_MAGENTA
            } else {
                CHECKER_BLACK
            };
            out.extend_from_slice(&[color[0], color[1], color[2], 0xff]);
        }
    }
    out
}

/// HLSL-заливка шахматкой без текстуры: клетки считаются от абсолютной
/// позиции `SV_Position`, поэтому узор стабилен при перемещении и ресайзе
/// стикера. Константный буфер — как у спрайта (три float4), но `misc` —
/// `(cell_dip, opacity, 0, 0)`, а `misc2` — `(scale, screen_w, screen_h, 0)`.
/// На текущий момент рендерером не задействован — точка расширения для M2.
pub const CHECKERBOARD_HLSL: &str = r#"
cbuffer Cb : register(b0) {
    float4 tr;
    float4 misc;
    float4 misc2;
};
struct VSOut {
    float4 pos : SV_Position;
};
static const float2 corners[6] = { float2(0,0), float2(1,0), float2(0,1),
                                   float2(1,0), float2(1,1), float2(0,1) };
VSOut mainVS(uint vid : SV_VertexID) {
    float2 c = corners[vid];
    VSOut o;
    float2 local = (c - 0.5) * tr.zw;
    o.pos = float4((tr.x + local.x) / misc2.y * 2.0 - 1.0,
                   1.0 - (tr.y + local.y) / misc2.z * 2.0, 0.0, 1.0);
    return o;
}
static const float3 magenta = float3(1.0, 0.0, 1.0);
static const float3 black = float3(0.0, 0.0, 0.0);
float4 mainPS(VSOut i) : SV_Target {
    float2 cell = misc.xy * misc2.x;          // клетка в физических пикселях
    float2 cellidx = floor(i.pos.xy / cell);
    float2 within = frac(i.pos.xy / cell);
    float2 dist = min(within, 1.0 - within);  // до ближайшей границы клетки
    float edge = min(dist.x, dist.y) * cell;  // в физических пикселях
    float aa = smoothstep(0.0, 1.0, edge);    // антиалиасинг границы
    float parity = fmod(cellidx.x + cellidx.y, 2.0);
    float3 my = parity < 0.5 ? magenta : black;
    float3 nbr = parity < 0.5 ? black : magenta;
    float3 color = lerp(my, nbr, 1.0 - aa);
    return float4(color * misc.y, misc.y);    // premultiplied opacity
}
"#;

/// Собрать `Sprite` из прямоугольника геометрии: заливка — текстура `fill`
/// (например, 1×1 белая для рамки/ручек или чёрная для затемнения),
/// прозрачность `opacity`. Координаты DIP из `r` в физические переводит сам
/// рендерер через `set_dpi_scale` (ADR-010).
pub fn solid_sprite(fill: &Texture, monitor_id: &MonitorId, r: &Box2D, opacity: f64) -> Sprite {
    Sprite::new(
        fill.clone(),
        Placement {
            monitor_id: monitor_id.clone(),
            cx: r.cx,
            cy: r.cy,
            w: r.w,
            h: r.h,
        },
        Transform {
            rotation: r.rotation,
            opacity,
            flip_h: false,
            flip_v: false,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(cx: f64, cy: f64, w: f64, h: f64, rotation: f64) -> SelectionBox {
        SelectionBox {
            center: (cx, cy),
            size: (w, h),
            rotation,
        }
    }

    #[test]
    fn corners_unrotated_match_axis_aligned_box() {
        let c = sel(100.0, 80.0, 60.0, 40.0, 0.0).corners();
        assert_eq!(c[0], (70.0, 60.0));
        assert_eq!(c[1], (130.0, 60.0));
        assert_eq!(c[2], (130.0, 100.0));
        assert_eq!(c[3], (70.0, 100.0));
    }

    #[test]
    fn corners_quarter_turn_rotates_nw_to_ne() {
        let c = sel(0.0, 0.0, 60.0, 40.0, std::f64::consts::FRAC_PI_2).corners();
        let (x, y) = c[0];
        assert!((x - 20.0).abs() < 1e-9, "x = {x}");
        assert!((y - -30.0).abs() < 1e-9, "y = {y}");
    }

    #[test]
    fn corners_half_turn_preserves_radius() {
        let r2 = 30.0f64 * 30.0 + 20.0 * 20.0;
        for (x, y) in sel(0.0, 0.0, 60.0, 40.0, std::f64::consts::PI).corners() {
            assert!((x * x + y * y - r2).abs() < 1e-9, "({x}, {y})");
        }
    }

    #[test]
    fn handle_centers_cover_eight_zones() {
        let b = sel(0.0, 0.0, 20.0, 10.0, 0.0);
        assert_eq!(b.handle_center(HandleKind::NorthWest), (-10.0, -5.0));
        assert_eq!(b.handle_center(HandleKind::North), (0.0, -5.0));
        assert_eq!(b.handle_center(HandleKind::NorthEast), (10.0, -5.0));
        assert_eq!(b.handle_center(HandleKind::East), (10.0, 0.0));
        assert_eq!(b.handle_center(HandleKind::SouthEast), (10.0, 5.0));
        assert_eq!(b.handle_center(HandleKind::South), (0.0, 5.0));
        assert_eq!(b.handle_center(HandleKind::SouthWest), (-10.0, 5.0));
        assert_eq!(b.handle_center(HandleKind::West), (-10.0, 0.0));
    }

    #[test]
    fn handle_center_rotates_with_box() {
        let b = sel(0.0, 0.0, 20.0, 10.0, std::f64::consts::FRAC_PI_2);
        let (x, y) = b.handle_center(HandleKind::NorthWest);
        assert!((x - 5.0).abs() < 1e-9, "x = {x}");
        assert!((y - -10.0).abs() < 1e-9, "y = {y}");
    }

    #[test]
    fn outline_rects_follow_edges() {
        let o = sel(100.0, 80.0, 60.0, 40.0, 0.0).outline_rects(2.0);
        assert_eq!(
            o[0],
            Box2D {
                cx: 100.0,
                cy: 60.0,
                w: 60.0,
                h: 2.0,
                rotation: 0.0
            }
        );
        assert_eq!(
            o[1],
            Box2D {
                cx: 130.0,
                cy: 80.0,
                w: 40.0,
                h: 2.0,
                rotation: std::f64::consts::FRAC_PI_2
            }
        );
        assert_eq!(
            o[2],
            Box2D {
                cx: 100.0,
                cy: 100.0,
                w: 60.0,
                h: 2.0,
                rotation: std::f64::consts::PI
            }
        );
        assert_eq!(
            o[3],
            Box2D {
                cx: 70.0,
                cy: 80.0,
                w: 40.0,
                h: 2.0,
                rotation: -std::f64::consts::FRAC_PI_2
            }
        );
    }

    #[test]
    fn handle_rects_in_clockwise_order() {
        let handles = sel(0.0, 0.0, 20.0, 10.0, 0.0).handle_rects(10.0);
        let kinds: Vec<HandleKind> = handles.iter().map(|(k, _)| *k).collect();
        assert_eq!(
            kinds,
            vec![
                HandleKind::NorthWest,
                HandleKind::North,
                HandleKind::NorthEast,
                HandleKind::East,
                HandleKind::SouthEast,
                HandleKind::South,
                HandleKind::SouthWest,
                HandleKind::West,
            ]
        );
        for (_, r) in &handles {
            assert_eq!(r.w, 10.0);
            assert_eq!(r.h, 10.0);
            assert_eq!(r.rotation, 0.0);
        }
    }

    #[test]
    fn visuals_bundle_outline_and_handles() {
        let v = sel(0.0, 0.0, 20.0, 10.0, 0.0).visuals();
        assert_eq!(v.outline.len(), 4);
        assert_eq!(v.handles.len(), 8);
        assert_eq!(v.outline[0].h, OUTLINE_THICKNESS_DIP);
        assert_eq!(v.handles[0].1.w, HANDLE_SIZE_DIP);
    }

    #[test]
    fn all_rects_flattens_outline_then_handles() {
        let rects = sel(0.0, 0.0, 20.0, 10.0, 0.0).all_rects();
        assert_eq!(rects.len(), 12);
        assert_eq!(
            rects[0],
            Box2D {
                cx: 0.0,
                cy: -5.0,
                w: 20.0,
                h: OUTLINE_THICKNESS_DIP,
                rotation: 0.0
            }
        );
    }

    #[test]
    fn from_sticker_takes_placement_and_rotation() {
        let p = Placement {
            monitor_id: MonitorId::default(),
            cx: 50.0,
            cy: 60.0,
            w: 100.0,
            h: 40.0,
        };
        let t = Transform {
            rotation: 1.0,
            opacity: 0.5,
            flip_h: true,
            flip_v: false,
        };
        let b = SelectionBox::new(&p, &t);
        assert_eq!(b.center(), (50.0, 60.0));
        assert_eq!(b.size(), (100.0, 40.0));
        assert_eq!(b.rotation(), 1.0);
    }

    #[test]
    fn edit_overlay_covers_full_screen_at_half_dim() {
        let o = edit_overlay(1920.0, 1080.0);
        assert_eq!(
            o,
            Box2D {
                cx: 960.0,
                cy: 540.0,
                w: 1920.0,
                h: 1080.0,
                rotation: 0.0
            }
        );
        assert_eq!(EDIT_OVERLAY_OPACITY, 0.5);
    }

    #[test]
    fn checkerboard_tile_two_pixel_cells() {
        let tile = checkerboard_tile(2, 4);
        assert_eq!(tile.len(), 4 * 4 * 4);
        let px = |x: u32, y: u32| -> [u8; 4] {
            let i = ((y * 4 + x) * 4) as usize;
            [tile[i], tile[i + 1], tile[i + 2], tile[i + 3]]
        };
        assert_eq!(px(0, 0), [255, 0, 255, 255]); // фуксия
        assert_eq!(px(1, 0), [255, 0, 255, 255]);
        assert_eq!(px(2, 0), [0, 0, 0, 255]); // чёрный
        assert_eq!(px(0, 1), [255, 0, 255, 255]);
        assert_eq!(px(3, 1), [0, 0, 0, 255]);
        assert_eq!(px(0, 2), [0, 0, 0, 255]);
        assert_eq!(px(3, 3), [255, 0, 255, 255]);
    }

    #[test]
    fn checkerboard_tile_single_pixel_cells() {
        let tile = checkerboard_tile(1, 2);
        assert_eq!(
            tile,
            vec![
                255, 0, 255, 255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 0, 255, 255
            ]
        );
    }

    #[test]
    #[should_panic(expected = "больше нуля")]
    fn checkerboard_tile_rejects_zero_cell() {
        checkerboard_tile(0, 4);
    }

    #[test]
    fn checkerboard_hlsl_has_both_entry_points() {
        assert!(CHECKERBOARD_HLSL.contains("mainVS"));
        assert!(CHECKERBOARD_HLSL.contains("mainPS"));
        assert!(CHECKERBOARD_HLSL.contains("magenta"));
        assert!(CHECKERBOARD_HLSL.contains("black"));
    }
}
