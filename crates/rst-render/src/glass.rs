//! Генератор растра материала Dark Liquid Glass для resticker (DESIGN_LIQUID_GLASS.md).
//!
//! Реализует процедурный рендеринг слоёв стекла по спецификации §4:
//! 1. Тело панели / контрола (GLASS_INK / CTRL_BG* / SUNKEN_BG).
//! 2. Вертикальный градиент (GLASS_SHEEN_TOP -> GLASS_SHEEN_BOTTOM).
//! 3. Зеркальный блик с волновым жидким искажением (GLASS_SPECULAR).
//! 4. Внутренние кромки по контуру (RIM_TOP / RIM_SIDE / RIM_BOTTOM / инверсия для Sunken).
//! 5. Внешняя обводка (STROKE / STROKE_STRONG).
//! 6. Внешнее гало (HOVER_GLOW).
//!
//! Соглашение об альфе — straight alpha (RGBA8), антиалиасинг краёв через 2×2 суперсемплинг.

// ============================================================================
// Константы палитры и геометрии Liquid Glass (DESIGN_LIQUID_GLASS.md)
// ============================================================================

/// §2.1 GLASS_INK — цвет тела панели и карточки (#07070A).
pub const GLASS_INK_RGB: [u8; 3] = [0x07, 0x07, 0x0a];
/// §2.1 GLASS_INK — непрозрачность тела панели (0.62).
pub const GLASS_INK_ALPHA: f64 = 0.62;

/// §2.1 GLASS_INK_DEEP — цвет тела меню трея и модальных диалогов (#050507).
pub const GLASS_INK_DEEP_RGB: [u8; 3] = [0x05, 0x05, 0x07];
/// §2.1 GLASS_INK_DEEP — непрозрачность тела модальных диалогов (0.74).
pub const GLASS_INK_DEEP_ALPHA: f64 = 0.74;

/// §2.1 GLASS_SHEEN_TOP — цвет верха вертикального градиента корпуса (#FFFFFF).
pub const GLASS_SHEEN_TOP_RGB: [u8; 3] = [0xff, 0xff, 0xff];
/// §2.1 GLASS_SHEEN_TOP — непрозрачность верха градиента (0.055).
pub const GLASS_SHEEN_TOP_ALPHA: f64 = 0.055;

/// §2.1 GLASS_SHEEN_BOTTOM — цвет низа вертикального градиента корпуса (#FFFFFF).
pub const GLASS_SHEEN_BOTTOM_RGB: [u8; 3] = [0xff, 0xff, 0xff];
/// §2.1 GLASS_SHEEN_BOTTOM — непрозрачность низа градиента (0.016).
pub const GLASS_SHEEN_BOTTOM_ALPHA: f64 = 0.016;

/// Тело тревожной плашки (`Surface::Danger`) — тот же красный, что у
/// остальных «нельзя» в программе (`theme::DANGER`, #D0463C).
///
/// Исключение из §2.4 наравне с бирюзой выделения и фуксией шахматки, и по
/// той же причине: сообщение об отказе обязано читаться как отказ ДО того,
/// как его прочитают буквами. Запрос пользователя 2026-09-01 — «сделай эту
/// плашку красного цвета».
pub const DANGER_BG_RGB: [u8; 3] = [0xd0, 0x46, 0x3c];
/// Непрозрачность тела тревожной плашки. Заметно плотнее обычного стекла
/// (`GLASS_INK_ALPHA` 0.62): красный на просвет вылинял бы в грязно-розовый
/// поверх произвольного кадра, а плашка должна читаться одинаково над
/// любым содержимым экрана.
pub const DANGER_BG_ALPHA: f64 = 0.88;

/// §2.1 GLASS_SPECULAR — цвет зеркального блика (#FFFFFF).
pub const GLASS_SPECULAR_RGB: [u8; 3] = [0xff, 0xff, 0xff];
/// §2.1 GLASS_SPECULAR — максимальная непрозрачность зеркального блика (0.20).
pub const GLASS_SPECULAR_ALPHA: f64 = 0.20;

/// §4 п. 3 — ширина пятна зеркального блика относительно ширины корпуса (65%).
pub const SPECULAR_WIDTH_FRAC: f64 = 0.65;
/// §4 п. 3 — высота пятна зеркального блика относительно высоты корпуса (45%).
pub const SPECULAR_HEIGHT_FRAC: f64 = 0.45;
/// §4 п. 3 — амплитуда синусоидального искривления границы блика относительно высоты корпуса (3.5%).
pub const SPECULAR_WAVE_AMP_FRAC: f64 = 0.035;
/// §4 п. 3 — пространственная частота синусоиды искривления границы блика.
pub const SPECULAR_WAVE_FREQ: f64 = 2.5;

/// §2.1 RIM_TOP — цвет внутренней верхней кромки (#FFFFFF).
pub const RIM_TOP_RGB: [u8; 3] = [0xff, 0xff, 0xff];
/// §2.1 RIM_TOP — непрозрачность внутренней верхней кромки (0.34).
pub const RIM_TOP_ALPHA: f64 = 0.34;

/// §2.1 RIM_SIDE — цвет внутренних боковых кромок (#FFFFFF).
pub const RIM_SIDE_RGB: [u8; 3] = [0xff, 0xff, 0xff];
/// §2.1 RIM_SIDE — непрозрачность внутренних боковых кромок (0.10).
pub const RIM_SIDE_ALPHA: f64 = 0.10;

/// §2.1 RIM_BOTTOM — цвет внутренней нижней кромки (#FFFFFF).
pub const RIM_BOTTOM_RGB: [u8; 3] = [0xff, 0xff, 0xff];
/// §2.1 RIM_BOTTOM — непрозрачность внутренней нижней кромки (0.055).
pub const RIM_BOTTOM_ALPHA: f64 = 0.055;

/// §5 «Как именно продавливается кнопка» — RIM_TOP для ControlHover / ControlActive (0.14).
pub const RIM_TOP_HOVER_ALPHA: f64 = 0.14;
/// §5 «Как именно продавливается кнопка» — RIM_BOTTOM для ControlHover / ControlActive (0.22).
pub const RIM_BOTTOM_HOVER_ALPHA: f64 = 0.22;

/// §4 п. 5, §2.2 — цвет тёмной верхней кромки для Sunken (#000000).
pub const RIM_SUNKEN_TOP_RGB: [u8; 3] = [0x00, 0x00, 0x00];
/// §4 п. 5, §2.2 — непрозрачность тёмной верхней кромки для Sunken (0.34).
pub const RIM_SUNKEN_TOP_ALPHA: f64 = 0.34;
/// §4 п. 5, §2.2 — непрозрачность тёмной боковой кромки для Sunken (0.10).
pub const RIM_SUNKEN_SIDE_ALPHA: f64 = 0.10;

/// §2.1 STROKE — цвет внешней обводки (#FFFFFF).
pub const STROKE_RGB: [u8; 3] = [0xff, 0xff, 0xff];
/// §2.1 STROKE — непрозрачность обычной внешней обводки (0.12).
pub const STROKE_ALPHA: f64 = 0.12;

/// §2.1 STROKE_STRONG — цвет усиленной внешней обводки (#FFFFFF).
pub const STROKE_STRONG_RGB: [u8; 3] = [0xff, 0xff, 0xff];
/// §2.1 STROKE_STRONG — непрозрачность усиленной внешней обводки (0.30).
pub const STROKE_STRONG_ALPHA: f64 = 0.30;

/// §2.2 CTRL_BG — цвет фона контрола в покое (#FFFFFF).
pub const CTRL_BG_RGB: [u8; 3] = [0xff, 0xff, 0xff];
/// §2.2 CTRL_BG — непрозрачность фона контрола в покое (0.045).
pub const CTRL_BG_ALPHA: f64 = 0.045;

/// §2.2 CTRL_BG_HOVER — цвет фона контрола под курсором (#FFFFFF).
pub const CTRL_BG_HOVER_RGB: [u8; 3] = [0xff, 0xff, 0xff];
/// §2.2 CTRL_BG_HOVER — непрозрачность фона контрола под курсором (0.105).
pub const CTRL_BG_HOVER_ALPHA: f64 = 0.105;

/// §2.2 CTRL_BG_ACTIVE — цвет фона зажатого контрола (#FFFFFF).
pub const CTRL_BG_ACTIVE_RGB: [u8; 3] = [0xff, 0xff, 0xff];
/// §2.2 CTRL_BG_ACTIVE — непрозрачность фона зажатого контрола (0.165).
pub const CTRL_BG_ACTIVE_ALPHA: f64 = 0.165;

/// §2.2 CTRL_BG_PRIMARY — цвет фона подтверждающей кнопки (#FFFFFF).
pub const CTRL_BG_PRIMARY_RGB: [u8; 3] = [0xff, 0xff, 0xff];
/// §2.2 CTRL_BG_PRIMARY — непрозрачность фона подтверждающей кнопки (0.16).
pub const CTRL_BG_PRIMARY_ALPHA: f64 = 0.16;

/// §2.2 CTRL_BG_ON — цвет фона включённого переключателя (#FFFFFF).
pub const CTRL_BG_ON_RGB: [u8; 3] = [0xff, 0xff, 0xff];
/// §2.2 CTRL_BG_ON — непрозрачность фона включённого переключателя (0.22).
pub const CTRL_BG_ON_ALPHA: f64 = 0.22;

/// §2.2 SUNKEN_BG — цвет фона утопленного поля ввода / жёлоба (#000000).
pub const SUNKEN_BG_RGB: [u8; 3] = [0x00, 0x00, 0x00];
/// §2.2 SUNKEN_BG — непрозрачность фона утопленного поля ввода / жёлоба (0.34).
pub const SUNKEN_BG_ALPHA: f64 = 0.34;

/// §2.3 HOVER_GLOW — цвет внешнего гало (#FFFFFF).
pub const HOVER_GLOW_RGB: [u8; 3] = [0xff, 0xff, 0xff];
/// §2.3 HOVER_GLOW — максимальная непрозрачность внешнего гало (0.22).
pub const HOVER_GLOW_ALPHA: f64 = 0.22;
/// §2.3 HOVER_GLOW — базовый радиус спада внешнего гало в пикселях/DIP (6.0).
pub const HOVER_GLOW_RADIUS_PX: f64 = 6.0;

/// §3 HAIRLINE — толщина внутренней кромки (1.0 px).
pub const HAIRLINE_PX: f64 = 1.0;
/// §3 HAIRLINE — толщина внешней обводки (1.0 px).
pub const STROKE_THICKNESS_PX: f64 = 1.0;

/// Смещения суперсемплинга 2×2 внутри пикселя (как в `crate::icons`).
const SUB_SAMPLES: [(f64, f64); 4] = [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)];

/// Тип поверхности стекла для выбора палитры и режимов освещения (§2, §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Surface {
    /// Корпус большой панели / окно настроек.
    Panel,
    /// Карточка внутри панели.
    Card,
    /// Корпус меню трея и модального диалога (§2.1 `GLASS_INK_DEEP`).
    Modal,
    /// Кнопка / переключатель в состоянии покоя.
    Control,
    /// Кнопка под курсором.
    ControlHover,
    /// Зажатая кнопка.
    ControlActive,
    /// Подтверждающая / акцентная кнопка.
    ControlPrimary,
    /// Включённый чекбокс / переключатель.
    ControlOn,
    /// Утопленное поле ввода / жёлоб ползунка (перевёрнутый свет).
    Sunken,
    /// Тревожная плашка: отказ, который надо заметить (`DANGER_BG_RGB`).
    /// Освещение — как у панели, меняется только цвет тела: плашка обязана
    /// остаться тем же материалом, иначе она выпадет из интерфейса и будет
    /// читаться как чужеродная наклейка.
    Danger,
}

/// Насколько растр обязан быть больше самого прямоугольника, чтобы гало
/// поместилось (в пикселях с каждой стороны) — вызывающий раздувает rect.
pub fn glow_pad_px(glow: f64) -> u32 {
    if glow <= 0.0 {
        0
    } else {
        HOVER_GLOW_RADIUS_PX.ceil() as u32
    }
}

/// Геометрические параметры тела скруглённого прямоугольника.
#[derive(Clone, Copy, Debug)]
struct GlassRect {
    x0: f64,
    y0: f64,
    w: f64,
    h: f64,
    radius: f64,
}

/// Растр скруглённого стеклянного прямоугольника, straight alpha (RGBA8),
/// размер `w_px` × `h_px`. `radius_px` — радиус скругления в физических пикселях.
/// `glow` 0..1 — сила внешнего белого гало (§2.3 HOVER_GLOW).
pub fn glass_rgba(w_px: u32, h_px: u32, radius_px: f64, surface: Surface, glow: f64) -> Vec<u8> {
    if w_px == 0 || h_px == 0 {
        return Vec::new();
    }

    let pad = glow_pad_px(glow) as f64;
    let w = f64::from(w_px);
    let h = f64::from(h_px);

    // Геометрические границы тела стекла с учётом отступа под внешнее гало
    let rect_x0 = pad;
    let rect_y0 = pad;
    let rect_x1 = (w - pad).max(rect_x0);
    let rect_y1 = (h - pad).max(rect_y0);
    let rect_w = rect_x1 - rect_x0;
    let rect_h = rect_y1 - rect_y0;

    let rect = GlassRect {
        x0: rect_x0,
        y0: rect_y0,
        w: rect_w,
        h: rect_h,
        radius: radius_px,
    };

    let mut out = Vec::with_capacity((w_px * h_px * 4) as usize);

    for py in 0..h_px {
        for px in 0..w_px {
            let mut acc_r = 0.0;
            let mut acc_g = 0.0;
            let mut acc_b = 0.0;
            let mut acc_a = 0.0;

            for (ox, oy) in SUB_SAMPLES {
                let x = f64::from(px) + ox;
                let y = f64::from(py) + oy;
                let sample = sample_glass(x, y, &rect, surface, glow);
                acc_r += sample.r;
                acc_g += sample.g;
                acc_b += sample.b;
                acc_a += sample.a;
            }

            let count = SUB_SAMPLES.len() as f64;
            let avg = PremulColor {
                r: acc_r / count,
                g: acc_g / count,
                b: acc_b / count,
                a: acc_a / count,
            };
            out.extend_from_slice(&avg.to_straight_rgba());
        }
    }

    out
}

/// Промежуточный цвет в премультиплицированном представлении для корректного
/// композитинга слоёв через оператор Porter-Duff «Source Over Destination».
#[derive(Clone, Copy, Debug)]
struct PremulColor {
    r: f64,
    g: f64,
    b: f64,
    a: f64,
}

impl PremulColor {
    const ZERO: Self = Self {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
    };

    /// Создать премультиплицированный цвет из straight RGB (0..1) и alpha (0..1).
    #[inline]
    fn from_straight(r: f64, g: f64, b: f64, a: f64) -> Self {
        Self {
            r: r * a,
            g: g * a,
            b: b * a,
            a,
        }
    }

    /// Наложить `src` поверх `self` (композитинг Porter-Duff Over).
    #[inline]
    fn over(self, src: PremulColor) -> Self {
        let inv_src_a = 1.0 - src.a;
        Self {
            r: src.r + self.r * inv_src_a,
            g: src.g + self.g * inv_src_a,
            b: src.b + self.b * inv_src_a,
            a: src.a + self.a * inv_src_a,
        }
    }

    /// Преобразовать в итоговый straight RGBA [u8; 4].
    #[inline]
    fn to_straight_rgba(self) -> [u8; 4] {
        if self.a <= 1e-6 {
            [0, 0, 0, 0]
        } else {
            let inv_a = 1.0 / self.a;
            let r = (self.r * inv_a * 255.0).round().clamp(0.0, 255.0) as u8;
            let g = (self.g * inv_a * 255.0).round().clamp(0.0, 255.0) as u8;
            let b = (self.b * inv_a * 255.0).round().clamp(0.0, 255.0) as u8;
            let a = (self.a * 255.0).round().clamp(0.0, 255.0) as u8;
            [r, g, b, a]
        }
    }
}

/// Вычисление знакового расстояния (SDF) и внешней нормали скруглённого прямоугольника.
/// Отрицательное `d` — внутри, положительное — снаружи. `(nx, ny)` — единичная нормаль.
#[inline]
fn rounded_rect_sdf_and_normal(x: f64, y: f64, rect: &GlassRect) -> (f64, f64, f64) {
    let w = rect.w;
    let h = rect.h;
    let r = rect.radius.min(w * 0.5).min(h * 0.5).max(0.0);
    let cx = rect.x0 + w * 0.5;
    let cy = rect.y0 + h * 0.5;
    let hx = w * 0.5;
    let hy = h * 0.5;

    let px = (x - cx).abs();
    let py = (y - cy).abs();
    let sx = if x >= cx { 1.0 } else { -1.0 };
    let sy = if y >= cy { 1.0 } else { -1.0 };

    let qx = px - (hx - r);
    let qy = py - (hy - r);

    if qx > 0.0 && qy > 0.0 {
        // Угловой сектор скругления: нормаль направлена радиально от центра дуги
        let len = (qx * qx + qy * qy).sqrt();
        let d = len - r;
        if len > 1e-9 {
            (d, sx * (qx / len), sy * (qy / len))
        } else {
            let inv_sqrt2 = std::f64::consts::FRAC_1_SQRT_2;
            (d, sx * inv_sqrt2, sy * inv_sqrt2)
        }
    } else {
        // Прямолинейные участки кромок: нормаль перпендикулярна ближайшей стороне
        let d = qx.max(qy) - r;
        if qx > qy { (d, sx, 0.0) } else { (d, 0.0, sy) }
    }
}

/// Вычисление цвета отдельного субсемпла в точке `(x, y)` путём послойного наложения по §4.
#[inline]
fn sample_glass(x: f64, y: f64, rect: &GlassRect, surface: Surface, glow: f64) -> PremulColor {
    if rect.w <= 0.0 || rect.h <= 0.0 {
        return PremulColor::ZERO;
    }

    let (d, nx, ny) = rounded_rect_sdf_and_normal(x, y, rect);

    // ------------------------------------------------------------------------
    // Слой 1 (§4 п. 1): Тело — сплошная заливка по скруглённому контуру
    // ------------------------------------------------------------------------
    let (body_rgb, body_alpha) = match surface {
        Surface::Panel | Surface::Card => (GLASS_INK_RGB, GLASS_INK_ALPHA),
        Surface::Modal => (GLASS_INK_DEEP_RGB, GLASS_INK_DEEP_ALPHA),
        Surface::Control => (CTRL_BG_RGB, CTRL_BG_ALPHA),
        Surface::ControlHover => (CTRL_BG_HOVER_RGB, CTRL_BG_HOVER_ALPHA),
        Surface::ControlActive => (CTRL_BG_ACTIVE_RGB, CTRL_BG_ACTIVE_ALPHA),
        Surface::ControlPrimary => (CTRL_BG_PRIMARY_RGB, CTRL_BG_PRIMARY_ALPHA),
        Surface::ControlOn => (CTRL_BG_ON_RGB, CTRL_BG_ON_ALPHA),
        Surface::Sunken => (SUNKEN_BG_RGB, SUNKEN_BG_ALPHA),
        Surface::Danger => (DANGER_BG_RGB, DANGER_BG_ALPHA),
    };

    let mut color = if d <= 0.0 {
        PremulColor::from_straight(
            f64::from(body_rgb[0]) / 255.0,
            f64::from(body_rgb[1]) / 255.0,
            f64::from(body_rgb[2]) / 255.0,
            body_alpha,
        )
    } else {
        PremulColor::ZERO
    };

    // ------------------------------------------------------------------------
    // Слой 2 (§4 п. 2): Вертикальный градиент (GLASS_SHEEN_TOP -> GLASS_SHEEN_BOTTOM)
    // Делает плиту стеклом; для Sunken градиент выключен (§4 п. 5).
    // ------------------------------------------------------------------------
    if d <= 0.0 && surface != Surface::Sunken {
        let ty = ((y - rect.y0) / rect.h).clamp(0.0, 1.0);
        let sheen_alpha = GLASS_SHEEN_TOP_ALPHA * (1.0 - ty) + GLASS_SHEEN_BOTTOM_ALPHA * ty;
        let sheen = PremulColor::from_straight(
            f64::from(GLASS_SHEEN_TOP_RGB[0]) / 255.0,
            f64::from(GLASS_SHEEN_TOP_RGB[1]) / 255.0,
            f64::from(GLASS_SHEEN_TOP_RGB[2]) / 255.0,
            sheen_alpha,
        );
        color = color.over(sheen);
    }

    // ------------------------------------------------------------------------
    // Слой 3 (§4 п. 3): Зеркальный блик в левом верхнем углу
    // Ширина ~65%, высота ~45%, синусоидальное жидкое искажение границы 3-4% высоты.
    // ------------------------------------------------------------------------
    if d <= 0.0 && surface != Surface::Sunken {
        let dx = x - rect.x0;
        let dy = y - rect.y0;
        let spec_w = SPECULAR_WIDTH_FRAC * rect.w;
        let spec_h = SPECULAR_HEIGHT_FRAC * rect.h;
        if spec_w > 0.0 && spec_h > 0.0 && dx >= 0.0 && dy >= 0.0 {
            let u = dx / spec_w;
            // Искривление границы синусоидой создаёт эффект жидкого блика на стекле
            let wave = (SPECULAR_WAVE_AMP_FRAC * rect.h / spec_h)
                * (u * std::f64::consts::PI * SPECULAR_WAVE_FREQ).sin();
            let v = (dy / spec_h) + wave;
            let r_spec = (u * u + v * v).sqrt();
            if r_spec < 1.0 {
                // Плавный спад интенсивности блика к нулю
                let falloff = 0.5 * (1.0 + (r_spec * std::f64::consts::PI).cos());
                let spec_alpha = GLASS_SPECULAR_ALPHA * falloff;
                let spec = PremulColor::from_straight(
                    f64::from(GLASS_SPECULAR_RGB[0]) / 255.0,
                    f64::from(GLASS_SPECULAR_RGB[1]) / 255.0,
                    f64::from(GLASS_SPECULAR_RGB[2]) / 255.0,
                    spec_alpha,
                );
                color = color.over(spec);
            }
        }
    }

    // ------------------------------------------------------------------------
    // Слой 4 (§4 п. 4): Внутренние кромки толщиной HAIRLINE (1 px)
    // Повторяют скругление контура; плавно интерполируются нормалями (nx, ny).
    // ------------------------------------------------------------------------
    let dist_inside = -d;
    if (0.0..=HAIRLINE_PX).contains(&dist_inside) {
        let abs_nx = nx.abs();
        let (rim_rgb, rim_alpha) = match surface {
            Surface::Sunken => {
                // Sunken переворачивает свет (§4 п. 5): тёмная кромка сверху, светлая снизу
                if ny < 0.0 {
                    let abs_ny = -ny;
                    let alpha = abs_ny * RIM_SUNKEN_TOP_ALPHA + abs_nx * RIM_SUNKEN_SIDE_ALPHA;
                    (RIM_SUNKEN_TOP_RGB, alpha)
                } else {
                    let abs_ny = ny;
                    let alpha = abs_ny * RIM_BOTTOM_ALPHA + abs_nx * RIM_SUNKEN_SIDE_ALPHA;
                    (RIM_BOTTOM_RGB, alpha)
                }
            }
            Surface::ControlHover | Surface::ControlActive => {
                // §5 «Как именно продавливается кнопка»: RIM_TOP гаснет, RIM_BOTTOM разгорается
                if ny < 0.0 {
                    let abs_ny = -ny;
                    let alpha = abs_ny * RIM_TOP_HOVER_ALPHA + abs_nx * RIM_SIDE_ALPHA;
                    (RIM_TOP_RGB, alpha)
                } else {
                    let abs_ny = ny;
                    let alpha = abs_ny * RIM_BOTTOM_HOVER_ALPHA + abs_nx * RIM_SIDE_ALPHA;
                    (RIM_BOTTOM_RGB, alpha)
                }
            }
            _ => {
                // Стандартный свет сверху: RIM_TOP сверху, RIM_SIDE по бокам, RIM_BOTTOM снизу
                if ny < 0.0 {
                    let abs_ny = -ny;
                    let alpha = abs_ny * RIM_TOP_ALPHA + abs_nx * RIM_SIDE_ALPHA;
                    (RIM_TOP_RGB, alpha)
                } else {
                    let abs_ny = ny;
                    let alpha = abs_ny * RIM_BOTTOM_ALPHA + abs_nx * RIM_SIDE_ALPHA;
                    (RIM_BOTTOM_RGB, alpha)
                }
            }
        };

        let rim = PremulColor::from_straight(
            f64::from(rim_rgb[0]) / 255.0,
            f64::from(rim_rgb[1]) / 255.0,
            f64::from(rim_rgb[2]) / 255.0,
            rim_alpha,
        );
        color = color.over(rim);
    }

    // ------------------------------------------------------------------------
    // Слой 5 (§4 п. 5): Внешняя обводка-волосинка (STROKE / STROKE_STRONG)
    // ------------------------------------------------------------------------
    if d.abs() <= STROKE_THICKNESS_PX * 0.5 {
        let (stroke_rgb, stroke_alpha) = match surface {
            Surface::ControlActive | Surface::ControlPrimary | Surface::ControlOn => {
                (STROKE_STRONG_RGB, STROKE_STRONG_ALPHA)
            }
            _ => (STROKE_RGB, STROKE_ALPHA),
        };
        let stroke = PremulColor::from_straight(
            f64::from(stroke_rgb[0]) / 255.0,
            f64::from(stroke_rgb[1]) / 255.0,
            f64::from(stroke_rgb[2]) / 255.0,
            stroke_alpha,
        );
        color = color.over(stroke);
    }

    // ------------------------------------------------------------------------
    // Слой 6 (§4 п. 6, §2.3): Гало наружу от контура (HOVER_GLOW)
    // Мягкий белый гауссоподобный спад за пределы скруглённого контура.
    // ------------------------------------------------------------------------
    if d > 0.0 && glow > 0.0 {
        let r_glow = HOVER_GLOW_RADIUS_PX;
        if d <= r_glow {
            let u = (d / r_glow).clamp(0.0, 1.0);
            let falloff = (-2.5 * u * u).exp() * (1.0 - u * u).powi(2);
            let halo_alpha = HOVER_GLOW_ALPHA * glow.clamp(0.0, 1.0) * falloff;
            let halo = PremulColor::from_straight(
                f64::from(HOVER_GLOW_RGB[0]) / 255.0,
                f64::from(HOVER_GLOW_RGB[1]) / 255.0,
                f64::from(HOVER_GLOW_RGB[2]) / 255.0,
                halo_alpha,
            );
            color = color.over(halo);
        }
    }

    color
}

// ============================================================================
// Тесты (обязательный набор по задаче)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Вспомогательная функция для извлечения RGBA пикселя по координатам `(x, y)`.
    fn get_pixel(buf: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
        let idx = ((y * w + x) * 4) as usize;
        [buf[idx], buf[idx + 1], buf[idx + 2], buf[idx + 3]]
    }

    /// Вычисление эффективной излучаемой яркости пикселя с учётом straight альфы.
    fn pixel_effective_brightness(rgba: [u8; 4]) -> f64 {
        let lum =
            0.299 * f64::from(rgba[0]) + 0.587 * f64::from(rgba[1]) + 0.114 * f64::from(rgba[2]);
        lum * (f64::from(rgba[3]) / 255.0)
    }

    #[test]
    fn test_panel_center_darker_than_top_rim() {
        // Центр Panel-растра темнее его верхней кромки (верх освещён градиентом, бликом и RIM_TOP).
        let w = 120;
        let h = 80;
        let radius = 14.0;
        let buf = glass_rgba(w, h, radius, Surface::Panel, 0.0);

        let center_px = get_pixel(&buf, w, w / 2, h / 2);
        let top_rim_px = get_pixel(&buf, w, w / 2, 0);

        let center_lum = pixel_effective_brightness(center_px);
        let top_rim_lum = pixel_effective_brightness(top_rim_px);

        assert!(
            center_lum < top_rim_lum,
            "центр панели (яркость {center_lum:.2}) обязан быть темнее верхней кромки (яркость {top_rim_lum:.2})"
        );
    }

    #[test]
    fn test_modal_body_is_deeper_than_panel_body() {
        // Репорт пользователя 2026-09-06: текст терминала слишком явно
        // читался сквозь две модалки; Surface::Modal обязан быть плотнее
        // обычного Panel, сохраняя остальные слои материала.
        let w = 200;
        let h = 100;
        let panel = glass_rgba(w, h, 18.0, Surface::Panel, 0.0);
        let modal = glass_rgba(w, h, 18.0, Surface::Modal, 0.0);
        let panel_px = get_pixel(&panel, w, w / 2, h / 2);
        let modal_px = get_pixel(&modal, w, w / 2, h / 2);
        assert!(
            modal_px[3] > panel_px[3],
            "альфа модального тела ({}) должна быть выше Panel ({})",
            modal_px[3],
            panel_px[3]
        );
        assert!(
            pixel_effective_brightness(modal_px) < pixel_effective_brightness(panel_px),
            "модальное тело обязано пропускать меньше света"
        );
    }

    #[test]
    fn test_control_vs_sunken_rim_brightness() {
        // Верхняя кромка ярче нижней у Control, и НАОБОРОТ у Sunken (Sunken переворачивает свет).
        let w = 100;
        let h = 40;
        let radius = 10.0;

        // 1. Обычный Control
        let ctrl_buf = glass_rgba(w, h, radius, Surface::Control, 0.0);
        let ctrl_top_px = get_pixel(&ctrl_buf, w, w / 2, 0);
        let ctrl_bot_px = get_pixel(&ctrl_buf, w, w / 2, h - 1);
        let ctrl_top_lum = pixel_effective_brightness(ctrl_top_px);
        let ctrl_bot_lum = pixel_effective_brightness(ctrl_bot_px);

        assert!(
            ctrl_top_lum > ctrl_bot_lum,
            "у Control верхняя кромка ({ctrl_top_lum:.2}) обязана быть ярче нижней ({ctrl_bot_lum:.2})"
        );

        // 2. Утопленный Sunken
        let sunken_buf = glass_rgba(w, h, radius, Surface::Sunken, 0.0);
        let sunken_top_px = get_pixel(&sunken_buf, w, w / 2, 0);
        let sunken_bot_px = get_pixel(&sunken_buf, w, w / 2, h - 1);
        let sunken_top_lum = pixel_effective_brightness(sunken_top_px);
        let sunken_bot_lum = pixel_effective_brightness(sunken_bot_px);

        assert!(
            sunken_top_lum < sunken_bot_lum,
            "у Sunken верхняя кромка ({sunken_top_lum:.2}) обязана быть темнее нижней ({sunken_bot_lum:.2})"
        );
    }

    #[test]
    fn test_corner_pixel_transparent_and_side_pixel_opaque() {
        // Угловой пиксель при радиусе r полностью прозрачен, а пиксель в середине стороны — нет.
        let w = 50;
        let h = 50;
        let radius = 12.0;
        let buf = glass_rgba(w, h, radius, Surface::Panel, 0.0);

        let corner_px = get_pixel(&buf, w, 0, 0);
        let side_mid_px = get_pixel(&buf, w, w / 2, 0);

        assert_eq!(
            corner_px[3], 0,
            "угловой пиксель при радиусе {radius} обязан быть полностью прозрачным"
        );
        assert!(
            side_mid_px[3] > 0,
            "пиксель в середине стороны обязан иметь ненулевую альфу"
        );
    }

    #[test]
    fn test_control_hover_brighter_than_control_body() {
        // ControlHover ярче Control в теле (CTRL_BG_HOVER > CTRL_BG).
        let w = 80;
        let h = 40;
        let radius = 10.0;

        let ctrl_buf = glass_rgba(w, h, radius, Surface::Control, 0.0);
        let hover_buf = glass_rgba(w, h, radius, Surface::ControlHover, 0.0);

        let ctrl_center = get_pixel(&ctrl_buf, w, w / 2, h / 2);
        let hover_center = get_pixel(&hover_buf, w, w / 2, h / 2);

        // Сравниваем альфу/яркость белой заливки тела контрола
        assert!(
            hover_center[3] > ctrl_center[3],
            "ControlHover в теле (альфа {}) обязан быть ярче/плотнее Control (альфа {})",
            hover_center[3],
            ctrl_center[3]
        );
    }

    #[test]
    fn test_glow_transparency_outside_rect() {
        // При glow=0 растр за пределами прямоугольника прозрачен, при glow=1 — нет.
        let w = 60;
        let h = 60;
        let radius = 15.0;

        // 1. При glow=0 угловой пиксель за пределами скруглённого контура полностью прозрачен
        let no_glow_buf = glass_rgba(w, h, radius, Surface::Control, 0.0);
        let no_glow_corner_px = get_pixel(&no_glow_buf, w, 0, 0);
        assert_eq!(
            no_glow_corner_px[3], 0,
            "при glow=0 пиксель в углу за пределами скругления обязан быть полностью прозрачным"
        );

        // 2. При glow=1 с учётом glow_pad_px за пределами тела прямоугольника есть свечение гало
        let pad = glow_pad_px(1.0);
        assert!(pad > 0, "паддинг для гало обязан быть больше нуля");
        let total_w = w + 2 * pad;
        let total_h = h + 2 * pad;

        let glow_buf = glass_rgba(total_w, total_h, radius, Surface::Control, 1.0);
        // Точка (2, total_h / 2) находится в зоне отступа гало (за пределами скруглённого rect)
        let check_x = 2;
        let check_y = total_h / 2;
        let glow_px = get_pixel(&glow_buf, total_w, check_x, check_y);

        assert!(
            glow_px[3] > 0,
            "при glow=1 пиксель гало в отступе обязан иметь ненулевую альфу (получено {})",
            glow_px[3]
        );
    }

    #[test]
    fn test_output_buffer_size() {
        // Размер выхода ровно w * h * 4.
        for (w, h) in [(1, 1), (10, 20), (64, 64), (100, 50)] {
            let buf = glass_rgba(w, h, 8.0, Surface::Panel, 0.5);
            assert_eq!(
                buf.len(),
                (w * h * 4) as usize,
                "буфер для {w}x{h} должен иметь размер w*h*4"
            );
        }

        // Проверка пустого ввода
        assert_eq!(glass_rgba(0, 50, 8.0, Surface::Panel, 0.0).len(), 0);
        assert_eq!(glass_rgba(50, 0, 8.0, Surface::Panel, 0.0).len(), 0);
    }
}
