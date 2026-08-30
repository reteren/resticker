//! Оффскрин-превью панелей оверлея в PNG — инструмент разработки, а не
//! часть продукта (весь модуль под `#[cfg(test)]`).
//!
//! Зачем: панели рисуются композитором поверх произвольного окна, и увидеть
//! их можно только запустив программу и войдя в режим редактирования — то
//! есть на живой машине пользователя. Для работы над оформлением этого мало:
//! нужно смотреть на результат правки сразу и на разных подложках. Здесь
//! примитивы [`rst_render::Primitive`] тех же билдеров, что идут в продукт
//! ([`crate::toolbar::build_toolbar`], [`crate::cursor_panel::build_cursor_panel`]),
//! растрируются на CPU в PNG.
//!
//! Запуск (тест помечен `#[ignore]`, в обычном прогоне не участвует):
//!
//! ```text
//! cargo test -p resticker ui_preview -- --ignored --nocapture
//! ```
//!
//! Путь к файлу печатается в stdout. Растеризация здесь СВОЯ, простая
//! (альфа-блендинг прямоугольников), и совпадает с GPU-конвейером только по
//! смыслу: цвет, геометрия, порядок примитивов. Субпиксельных гарантий она
//! не даёт и заменой живой проверке не является.

use rst_core::hittest::DipRect;
use rst_render::{Box2D, Primitive, Widget, icon_rgba, rasterize, text_size};

use crate::cursor_panel::build_cursor_panel;
use crate::toolbar::{VideoToolbarState, build_toolbar};

/// Холст RGBA с прямым (straight) альфа-каналом.
struct Canvas {
    w: u32,
    h: u32,
    px: Vec<u8>,
}

impl Canvas {
    fn new(w: u32, h: u32) -> Self {
        Self {
            w,
            h,
            px: vec![0; (w * h * 4) as usize],
        }
    }

    /// Смешать пиксель `color` с альфой `a` поверх текущего (source-over).
    fn blend(&mut self, x: u32, y: u32, color: [u8; 3], a: u8) {
        if x >= self.w || y >= self.h || a == 0 {
            return;
        }
        let i = ((y * self.w + x) * 4) as usize;
        let sa = f64::from(a) / 255.0;
        for (c, src) in color.iter().enumerate() {
            let dst = f64::from(self.px[i + c]);
            let src = f64::from(*src);
            self.px[i + c] = (src * sa + dst * (1.0 - sa)).round() as u8;
        }
        let da = f64::from(self.px[i + 3]) / 255.0;
        self.px[i + 3] = ((sa + da * (1.0 - sa)) * 255.0).round() as u8;
    }

    /// Наложить RGBA-битмап (straight alpha) на прямоугольник `rect` —
    /// с учётом ПОВОРОТА: для каждого пикселя описанного прямоугольника
    /// точка переводится в локальные координаты `rect` (та же обратная
    /// аффинная математика, что у хит-теста) и берётся ближайший тексель.
    /// Без этого повёрнутая рамка выделения в превью выглядела бы набором
    /// горизонтальных полос.
    fn blit(&mut self, rect: &Box2D, src: &[u8], sw: u32, sh: u32, opacity: f64) {
        if sw == 0 || sh == 0 {
            return;
        }
        self.for_each_pixel(rect, |u, v| {
            let sx = ((u * f64::from(sw)) as i64).clamp(0, i64::from(sw) - 1) as u32;
            let sy = ((v * f64::from(sh)) as i64).clamp(0, i64::from(sh) - 1) as u32;
            let i = ((sy * sw + sx) * 4) as usize;
            let a = (f64::from(src[i + 3]) * opacity).round().clamp(0.0, 255.0) as u8;
            ([src[i], src[i + 1], src[i + 2]], a)
        });
    }

    /// Обойти пиксели, накрытые прямоугольником `rect` (с поворотом), и
    /// смешать то, что вернёт `pixel` по нормированным координатам внутри
    /// прямоугольника (`u`, `v` в `[0, 1)`).
    fn for_each_pixel(&mut self, rect: &Box2D, mut pixel: impl FnMut(f64, f64) -> ([u8; 3], u8)) {
        let (cx, cy) = (rect.cx * SCALE, rect.cy * SCALE);
        let (hw, hh) = (rect.w * SCALE / 2.0, rect.h * SCALE / 2.0);
        if !(hw > 0.0 && hh > 0.0) {
            return;
        }
        let (sin, cos) = rect.rotation.sin_cos();
        // Описанный прямоугольник повёрнутого: по полуразмерам проекций.
        let ex = hw * cos.abs() + hh * sin.abs();
        let ey = hw * sin.abs() + hh * cos.abs();
        let x0 = (cx - ex).floor().max(0.0) as u32;
        let x1 = (cx + ex).ceil().min(f64::from(self.w)) as u32;
        let y0 = (cy - ey).floor().max(0.0) as u32;
        let y1 = (cy + ey).ceil().min(f64::from(self.h)) as u32;
        for y in y0..y1 {
            for x in x0..x1 {
                let (dx, dy) = (f64::from(x) + 0.5 - cx, f64::from(y) + 0.5 - cy);
                let lx = dx * cos + dy * sin;
                let ly = -dx * sin + dy * cos;
                if lx.abs() > hw || ly.abs() > hh {
                    continue;
                }
                let (color, a) = pixel((lx + hw) / (2.0 * hw), (ly + hh) / (2.0 * hh));
                self.blend(x, y, color, a);
            }
        }
    }
}

/// Масштаб превью: 1 DIP = столько пикселей PNG. Оверлей на 100% DPI
/// рисует 1:1, но разглядывать грани в один пиксель на скриншоте нечем —
/// смотрим увеличенно, как в макете.
const SCALE: f64 = 2.0;

/// Нарисовать примитивы панели на холст в порядке их выдачи билдером.
fn draw_primitives(canvas: &mut Canvas, prims: &[Primitive]) {
    for prim in prims {
        match prim {
            Primitive::Fill {
                rect,
                color,
                opacity,
            } => {
                let a = (opacity * 255.0).round().clamp(0.0, 255.0) as u8;
                canvas.for_each_pixel(rect, |_, _| (*color, a));
            }
            // Стекло в превью рисуется настоящим растром `rst_render::glass`
            // — иначе картинка врала бы про самое заметное: материал панели.
            Primitive::Glass {
                rect,
                radius,
                surface,
                glow,
                opacity,
            } => {
                let pad = rst_render::glass::glow_pad_px(*glow);
                let w = ((rect.w * SCALE).round().max(1.0) as u32) + 2 * pad;
                let h = ((rect.h * SCALE).round().max(1.0) as u32) + 2 * pad;
                let rgba = rst_render::glass::glass_rgba(w, h, radius * SCALE, *surface, *glow);
                let pad_dip = f64::from(pad) / SCALE;
                let inflated = Box2D {
                    w: rect.w + 2.0 * pad_dip,
                    h: rect.h + 2.0 * pad_dip,
                    ..*rect
                };
                canvas.blit(&inflated, &rgba, w, h, *opacity);
            }
            Primitive::Text {
                rect,
                text,
                color,
                opacity,
            } => {
                if text.is_empty() {
                    continue;
                }
                let (rgba, w, h) = rasterize(text, *color, SCALE as u32 * 2);
                canvas.blit(rect, &rgba, w, h, *opacity);
            }
            Primitive::Icon {
                rect,
                icon,
                opacity,
            } => {
                let size = (rect.w.max(rect.h) * SCALE * 2.0).round().max(1.0) as u32;
                let rgba = icon_rgba(*icon, size);
                canvas.blit(rect, &rgba, size, size, *opacity);
            }
            Primitive::Rgba {
                rect,
                width,
                height,
                rgba,
                opacity,
                ..
            } => canvas.blit(rect, rgba, *width, *height, *opacity),
        }
    }
}

/// Минимальный кодировщик PNG (RGBA8, без фильтров, zlib «store») — чтобы
/// не тащить `image` в зависимости бинарного крейта ради инструмента
/// разработки.
fn encode_png(canvas: &Canvas) -> Vec<u8> {
    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xffff_ffffu32;
        for &b in data {
            crc ^= u32::from(b);
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xedb8_8320 & mask);
            }
        }
        !crc
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
    }

    // Сырые строки со стандартным префиксом фильтра 0.
    let mut raw = Vec::with_capacity((canvas.h * (canvas.w * 4 + 1)) as usize);
    for y in 0..canvas.h {
        raw.push(0);
        let row = ((y * canvas.w * 4) as usize)..(((y + 1) * canvas.w * 4) as usize);
        raw.extend_from_slice(&canvas.px[row]);
    }

    // zlib: заголовок, несжатые блоки по 65535 байт, adler32.
    let mut z = vec![0x78, 0x01];
    for (i, block) in raw.chunks(65_535).enumerate() {
        let last = u8::from((i + 1) * 65_535 >= raw.len());
        z.push(last);
        z.extend_from_slice(&(block.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
        z.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in &raw {
        a = (a + u32::from(byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());

    let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&canvas.w.to_be_bytes());
    ihdr.extend_from_slice(&canvas.h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8 бит, RGBA, без интерлейса
    chunk(&mut png, b"IHDR", &ihdr);
    chunk(&mut png, b"IDAT", &z);
    chunk(&mut png, b"IEND", &[]);
    png
}

/// Подложка: горизонтальный градиент от тёмного к светлому плюс полосы —
/// панель полупрозрачная, и её читаемость надо видеть на обоих концах.
fn backdrop(canvas: &mut Canvas) {
    for y in 0..canvas.h {
        for x in 0..canvas.w {
            let t = f64::from(x) / f64::from(canvas.w.max(1));
            let base = (24.0 + t * 200.0).round() as u8;
            let stripe = if (x / 40 + y / 40) % 2 == 0 { 12 } else { 0 };
            canvas.blend(x, y, [base.saturating_add(stripe), base, base], 255);
        }
    }
}

#[test]
#[ignore = "инструмент разработки: пишет PNG, не проверяет инвариантов"]
fn ui_preview_png() {
    let (w, h) = (1120u32, 520u32);
    let mut canvas = Canvas::new((f64::from(w) * SCALE) as u32, (f64::from(h) * SCALE) as u32);
    backdrop(&mut canvas);

    // Тулбар одиночного выделения видео-стикера: ползунок прозрачности,
    // поле, семь кнопок, play/pause, полоса перемотки, громкость.
    let video_toolbar = build_toolbar(
        &DipRect::from_center(560.0, 40.0, 400.0, 40.0),
        Some(0.8),
        Some(VideoToolbarState {
            paused: false,
            show_timeline: true,
            volume_pct: 65,
        }),
        f64::from(h),
    );
    // Тот же тулбар без видео и с выключенной полосой — картинка-стикер.
    let image_toolbar = build_toolbar(
        &DipRect::from_center(400.0, 130.0, 200.0, 40.0),
        Some(0.35),
        None,
        f64::from(h),
    );
    // Мультивыделение: только кнопки.
    let multi_toolbar = build_toolbar(
        &DipRect::from_center(880.0, 130.0, 200.0, 40.0),
        None,
        None,
        f64::from(h),
    );
    let cursor = build_cursor_panel(
        &DipRect::new(0.0, 0.0, f64::from(w), f64::from(h)),
        true,
        1.0,
    );

    for panel in [&video_toolbar, &image_toolbar, &multi_toolbar, &cursor] {
        let mut prims = Vec::new();
        panel.draw(&mut prims);
        draw_primitives(&mut canvas, &prims);
    }

    // Тултип кнопки — тот же примитивный набор, что рисует координатор.
    let mut tip = Vec::new();
    let text = "Visibility layers";
    let (tw, th) = text_size(text);
    let frame = Box2D {
        cx: 700.0,
        cy: 250.0,
        w: tw + 12.0,
        h: th + 12.0,
        rotation: 0.0,
    };
    rst_render::tooltip_frame(&mut tip, frame, 1.0);
    tip.push(Primitive::Text {
        rect: Box2D {
            w: tw,
            h: th,
            ..frame
        },
        text: text.to_string(),
        color: rst_render::theme::TEXT,
        opacity: 1.0,
    });
    draw_primitives(&mut canvas, &tip);

    // Рабочий каталог теста — каталог крейта, а не корень workspace;
    // по умолчанию пишем во временный каталог, путь переопределяется
    // переменной окружения.
    // Рамка выделения: акцентная обводка, белые ручки ресайза и стрелки
    // поворота за углами — та же геометрия, что рисует координатор.
    {
        use rst_render::{
            ROTATE_HANDLE_GAP_DIP, ROTATE_HANDLE_SIZE_DIP, SELECTION_COLOR, SelectionBox,
        };
        let placement = rst_core::model::Placement {
            monitor_id: rst_core::model::MonitorId("preview".to_string()),
            cx: 900.0,
            cy: 250.0,
            w: 150.0,
            h: 100.0,
        };
        let transform = rst_core::model::Transform {
            rotation: 0.3,
            ..rst_core::model::Transform::default()
        };
        let sbox = SelectionBox::new(&placement, &transform);
        let mut prims = vec![Primitive::Fill {
            rect: Box2D {
                cx: placement.cx,
                cy: placement.cy,
                w: placement.w,
                h: placement.h,
                rotation: transform.rotation,
            },
            color: [0x20, 0x24, 0x30],
            opacity: 0.9,
        }];
        let visuals = sbox.visuals();
        for rect in visuals.outline {
            prims.push(Primitive::Fill {
                rect,
                color: SELECTION_COLOR,
                opacity: 1.0,
            });
        }
        for (_, rect) in visuals.handles {
            prims.push(Primitive::Fill {
                rect,
                color: [0xff, 0xff, 0xff],
                opacity: 1.0,
            });
        }
        for (_, rect) in sbox.rotate_handle_rects(ROTATE_HANDLE_SIZE_DIP, ROTATE_HANDLE_GAP_DIP) {
            prims.push(Primitive::Icon {
                rect,
                icon: rst_render::Icon::Rotate,
                opacity: 1.0,
            });
        }
        draw_primitives(&mut canvas, &prims);
    }

    // Модал подтверждения удаления — со своим наведением и нажатием, чтобы
    // на одной картинке были видны все три состояния кнопки.
    {
        let mut dialog = crate::confirm_dialog::build(3, (260.0, 210.0));
        // Курсор «наведён» на Cancel и «зажал» Delete.
        let cancel = dialog
            .widget::<rst_render::Button>(crate::confirm_dialog::ID_CANCEL)
            .expect("кнопка на месте")
            .bounds();
        dialog.pointer_event(rst_render::PointerEvent::Move {
            pos: (cancel.cx, cancel.cy),
        });
        let delete = dialog
            .widget::<rst_render::Button>(crate::confirm_dialog::ID_DELETE)
            .expect("кнопка на месте")
            .bounds();
        dialog.pointer_event(rst_render::PointerEvent::Down {
            pos: (delete.cx, delete.cy),
        });
        let mut prims = Vec::new();
        dialog.draw(&mut prims);
        draw_primitives(&mut canvas, &prims);
    }

    // Панель пресетов режима редактирования.
    {
        let presets: Vec<rst_core::model::Preset> = ["Work", "Stream", "Night"]
            .into_iter()
            .map(|name| rst_core::model::Preset {
                id: uuid::Uuid::new_v4(),
                name: name.to_string(),
                stickers: Vec::new(),
            })
            .collect();
        let frame = Box2D {
            cx: 880.0,
            cy: 230.0,
            w: crate::preset_picker::WIDTH,
            h: crate::preset_picker::height(presets.len()),
            rotation: 0.0,
        };
        let mut panel = crate::preset_picker::build(&presets, "Night mode", frame);
        let save = panel
            .widget::<rst_render::Button>(crate::preset_picker::BTN_SAVE)
            .expect("кнопка сохранения")
            .bounds();
        panel.pointer_event(rst_render::PointerEvent::Move {
            pos: (save.cx, save.cy),
        });
        let mut prims = Vec::new();
        panel.draw(&mut prims);
        draw_primitives(&mut canvas, &prims);
    }

    // Лист иконок крупным планом: пиктограммы читаются в размере кнопки
    // (20 DIP), но правку формы удобнее смотреть увеличенно.
    {
        let mut x = 40.0;
        for icon in rst_render::Icon::ALL {
            let rect = Box2D {
                cx: x + 24.0,
                cy: 360.0,
                w: 40.0,
                h: 40.0,
                rotation: 0.0,
            };
            draw_primitives(
                &mut canvas,
                &[Primitive::Fill {
                    rect,
                    color: rst_render::theme::PANEL_BG,
                    opacity: 1.0,
                }],
            );
            draw_primitives(
                &mut canvas,
                &[Primitive::Icon {
                    rect,
                    icon,
                    opacity: 1.0,
                }],
            );
            x += 48.0;
        }
    }

    let path = std::env::var("RESTICKER_UI_PREVIEW").unwrap_or_else(|_| {
        std::env::temp_dir()
            .join("resticker_ui_preview.png")
            .to_string_lossy()
            .into_owned()
    });
    std::fs::write(&path, encode_png(&canvas)).expect("записать превью");
    println!("превью UI: {path}");
}
