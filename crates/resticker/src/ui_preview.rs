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
use crate::toolbar::{ToolbarState, VideoToolbarState, build_toolbar};

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

/// Контрастная «терминальная» подложка для замера плотности модала. Полосы
/// вертикальные, чтобы можно было выбрать чистый участок между строками
/// текста и не спутать просвет корпуса с яркостью глифов.
fn modal_density_backdrop(canvas: &mut Canvas) {
    let stripe_w = (16.0 * SCALE) as u32;
    for y in 0..canvas.h {
        for x in 0..canvas.w {
            let stripe = (x / stripe_w.max(1)) % 2 == 0;
            let mut color: [u8; 3] = if stripe {
                [196, 62, 76]
            } else {
                [54, 151, 196]
            };
            // Тонкие горизонтальные строки добавляют подложке характер
            // терминального текста, но не влияют на попарный замер вертикальных полос.
            if y % (9 * SCALE as u32).max(1) == 0 {
                color = [
                    color[0].saturating_add(22),
                    color[1].saturating_add(22),
                    color[2],
                ];
            }
            canvas.blend(x, y, color, 255);
        }
    }
}

/// Отрисовать менеджер групп, при необходимости убрав модальную накладку
/// для контрольного кадра со старым телом `GLASS_INK @ 0.62`.
fn draw_group_manager_density(canvas: &mut Canvas, frame: Box2D, old_body: bool) {
    let panel = crate::group_manager::build(&[], None, frame);
    let mut prims = Vec::new();
    panel.draw(&mut prims);
    if old_body {
        // Контрольный кадр — ТОТ ЖЕ корпус, но со старым телом
        // (`Surface::Panel`, `GLASS_INK` 0.62). Выбрасывать примитив совсем
        // нельзя: получилось бы сравнение «без корпуса против 0.74», и
        // просвет честно показывал бы 100 % — замер про плотность, а не
        // про наличие панели.
        for prim in &mut prims {
            if let Primitive::Glass { surface, .. } = prim {
                if *surface == rst_render::glass::Surface::Modal {
                    *surface = rst_render::glass::Surface::Panel;
                }
            }
        }
    }
    draw_primitives(canvas, &prims);
}

/// Средний перепад яркости между соседними вертикальными полосами в чистом
/// участке корпуса, в процентах от перепада на голой подложке.
fn modal_density_percent(canvas: &Canvas, bare: &Canvas, frame: Box2D) -> f64 {
    let y = ((frame.cy - frame.h / 2.0 + 34.0) * SCALE).round() as u32;
    let y0 = y.saturating_sub(3);
    let y1 = (y + 3).min(canvas.h.saturating_sub(1));
    let x0 = ((frame.cx - 150.0) * SCALE).round().max(0.0) as u32;
    let x1 = ((frame.cx + 150.0) * SCALE).round().min(canvas.w as f64) as u32;
    let stripe_w = (16.0 * SCALE) as u32;
    let luminance = |source: &Canvas, x: u32, y: u32| {
        let i = ((y * source.w + x) * 4) as usize;
        0.2126 * f64::from(source.px[i])
            + 0.7152 * f64::from(source.px[i + 1])
            + 0.0722 * f64::from(source.px[i + 2])
    };
    let mean = |source: &Canvas, left: u32, right: u32| {
        let mut total = 0.0;
        let mut count = 0u32;
        for yy in y0..=y1 {
            for xx in left..right.min(source.w) {
                total += luminance(source, xx, yy);
                count += 1;
            }
        }
        total / f64::from(count.max(1))
    };
    let mut covered = 0.0;
    let mut bare_contrast = 0.0;
    let mut left = x0;
    while left + 2 * stripe_w <= x1 {
        let middle = left + stripe_w;
        let right = middle + stripe_w;
        covered += (mean(canvas, left, middle) - mean(canvas, middle, right)).abs();
        bare_contrast += (mean(bare, left, middle) - mean(bare, middle, right)).abs();
        left = right;
    }
    if bare_contrast > 0.0 {
        100.0 * covered / bare_contrast
    } else {
        0.0
    }
}

#[test]
#[ignore = "инструмент разработки: пишет PNG и замеряет просвет модального корпуса"]
fn modal_density_preview_png() {
    let (w, h) = (1120u32, 360u32);
    let mut bare = Canvas::new((f64::from(w) * SCALE) as u32, (f64::from(h) * SCALE) as u32);
    modal_density_backdrop(&mut bare);
    let mut old_canvas = Canvas::new(bare.w, bare.h);
    old_canvas.px.copy_from_slice(&bare.px);
    let mut new_canvas = Canvas::new(bare.w, bare.h);
    new_canvas.px.copy_from_slice(&bare.px);

    let panel_h = crate::group_manager::height(0, 0);
    let old_frame = Box2D {
        cx: 280.0,
        cy: 180.0,
        w: crate::group_manager::WIDTH,
        h: panel_h,
        rotation: 0.0,
    };
    let new_frame = Box2D {
        cx: 840.0,
        cy: 180.0,
        ..old_frame
    };
    draw_group_manager_density(&mut old_canvas, old_frame, true);
    draw_group_manager_density(&mut new_canvas, new_frame, false);

    let old_percent = modal_density_percent(&old_canvas, &bare, old_frame);
    let new_percent = modal_density_percent(&new_canvas, &bare, new_frame);

    let mut preview = Canvas::new(bare.w, bare.h);
    preview.px.copy_from_slice(&bare.px);
    draw_group_manager_density(&mut preview, old_frame, true);
    draw_group_manager_density(&mut preview, new_frame, false);
    for (cx, label) in [
        (old_frame.cx, "old body 0.62"),
        (new_frame.cx, "new body 0.74"),
    ] {
        let (tw, th) = text_size(label);
        draw_primitives(
            &mut preview,
            &[Primitive::Text {
                rect: Box2D {
                    cx,
                    cy: 38.0,
                    w: tw,
                    h: th,
                    rotation: 0.0,
                },
                text: label.to_string(),
                color: rst_render::theme::TEXT,
                opacity: 1.0,
            }],
        );
    }
    let path = std::env::var("RESTICKER_MODAL_DENSITY_PREVIEW").unwrap_or_else(|_| {
        std::env::temp_dir()
            .join("resticker_modal_density_preview.png")
            .to_string_lossy()
            .into_owned()
    });
    std::fs::write(&path, encode_png(&preview)).expect("записать превью плотности модала");
    println!(
        "плотность модала: старое тело 0.62 — просвет {old_percent:.2}%; новое тело 0.74 — просвет {new_percent:.2}%; PNG: {path}"
    );
}

#[test]
#[ignore = "инструмент разработки: пишет PNG, не проверяет инвариантов"]
fn ui_preview_png() {
    let (w, h) = (1120u32, 520u32);
    let mut canvas = Canvas::new((f64::from(w) * SCALE) as u32, (f64::from(h) * SCALE) as u32);
    backdrop(&mut canvas);

    // Тулбар одиночного выделения видео-стикера: ползунок прозрачности,
    // поле, семь кнопок, play/pause, полоса перемотки, громкость.
    // Ниже верхнего края намеренно: у громкости выпадающая шкала растёт
    // ВВЕРХ, и у самой кромки холста её было бы не видно.
    let video_toolbar = build_toolbar(
        &DipRect::from_center(560.0, 190.0, 400.0, 40.0),
        &ToolbarState {
            opacity: 0.8,
            visible: true,
            video: Some(VideoToolbarState {
                paused: false,
                show_timeline: true,
                volume_pct: 65,
                muted: false,
            }),
        },
        f64::from(h),
    );
    // Тот же тулбар без видео — картинка-стикер.
    let image_toolbar = build_toolbar(
        &DipRect::from_center(300.0, 300.0, 200.0, 40.0),
        &ToolbarState {
            opacity: 0.35,
            visible: true,
            video: None,
        },
        f64::from(h),
    );
    // Скрытый стикер: тот же тулбар с закрытым глазом (2026-09-06).
    let multi_toolbar = build_toolbar(
        &DipRect::from_center(820.0, 300.0, 200.0, 40.0),
        &ToolbarState {
            opacity: 1.0,
            visible: false,
            video: None,
        },
        f64::from(h),
    );
    // Громкость показываем РАСКРЫТОЙ: в покое это просто кнопка-динамик,
    // а проверять надо именно выпадающую шкалу (2026-09-06). Наведение и
    // прогон анимации — то же, что делает живой указатель.
    let mut video_toolbar = video_toolbar;
    if let Some(volume) = video_toolbar.widget_mut::<rst_render::VolumeControl>(crate::toolbar::TB_VOLUME)
    {
        volume.set_hovered(true);
    }
    for _ in 0..40 {
        video_toolbar.animate(16.0);
    }
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

/// Превью ленты набора группы: иконка приложения поверх снимка окна.
///
/// Снимки взяты нарочно белый, тёмный и серый: иконка обязана читаться на
/// любом — ради этого под ней и стоит плашка.
#[test]
#[ignore = "инструмент разработки: пишет PNG, не проверяет инвариантов"]
fn strip_preview_png() {
    use crate::group_strip::{self, StripCard, StripImage};

    let (w, h) = (1200u32, 460u32);
    let mut canvas = Canvas::new((f64::from(w) * SCALE) as u32, (f64::from(h) * SCALE) as u32);
    backdrop(&mut canvas);

    // Снимки широкие (16:9) — настоящие окна такие и есть, и именно на
    // широком снимке видно, не свисает ли значок с угла карточки.
    let solid = |key: u64, w: u32, h: u32, rgba: [u8; 4]| StripImage {
        key,
        width: w,
        height: h,
        rgba: rgba.repeat((w * h) as usize),
    };
    // Иконка с прозрачным полем по краям — как настоящая.
    let icon = |key: u64, rgba: [u8; 4]| {
        let side = 32u32;
        let mut px = vec![0u8; (side * side * 4) as usize];
        for y in 4..side - 4 {
            for x in 4..side - 4 {
                let i = ((y * side + x) * 4) as usize;
                px[i..i + 4].copy_from_slice(&rgba);
            }
        }
        StripImage {
            key,
            width: side,
            height: side,
            rgba: px,
        }
    };

    let mut cards = vec![
        StripCard {
            hwnd: 1,
            title: "Пустая вкладка".to_string(),
            icon: Some(icon(1, [240, 240, 245, 255])),
            thumb: Some(solid(11, 320, 180, [255, 255, 255, 255])),
            slot: Some(1),
        },
        StripCard {
            hwnd: 2,
            title: "DaVinci Resolve S…".to_string(),
            icon: Some(icon(2, [90, 150, 245, 255])),
            thumb: Some(solid(12, 320, 180, [18, 18, 22, 255])),
            slot: None,
        },
        StripCard {
            hwnd: 3,
            title: "Steam".to_string(),
            icon: Some(icon(3, [70, 200, 160, 255])),
            thumb: Some(solid(13, 240, 240, [120, 120, 128, 255])),
            slot: Some(2),
        },
        StripCard {
            hwnd: 4,
            title: "Без снимка".to_string(),
            icon: Some(icon(4, [230, 120, 90, 255])),
            thumb: None,
            slot: None,
        },
    ];

    // Догоняем число карточек до четырнадцати — столько окон было на
    // скриншоте пользователя: ровно тот случай, ради которого появились ряды.
    for k in 5..=14u64 {
        cards.push(StripCard {
            hwnd: k as usize,
            title: format!("Окно {k}"),
            icon: Some(icon(k, [140, 140, 150, 255])),
            thumb: Some(solid(100 + k, 320, 180, [40, 40, 46, 255])),
            slot: None,
        });
    }

    let strip = group_strip::build(
        &cards,
        Box2D {
            cx: f64::from(w) / 2.0,
            cy: f64::from(h) / 2.0,
            w: f64::from(w),
            h: f64::from(h),
            rotation: 0.0,
        },
    );
    // Карточки въезжают с задержкой-каскадом (`Phase` + `stagger_delay_ms`),
    // и на нулевом кадре их содержимое ещё прозрачно. Прокручиваем анимацию
    // до конца — превью показывает установившийся вид, а не первый кадр.
    let mut strip = strip;
    for _ in 0..60 {
        strip.panel.animate(50.0);
    }
    let mut prims = Vec::new();
    strip.panel.draw(&mut prims);
    draw_primitives(&mut canvas, &prims);

    let path = std::env::var("RESTICKER_STRIP_PREVIEW").unwrap_or_else(|_| {
        std::env::temp_dir()
            .join("resticker_strip_preview.png")
            .to_string_lossy()
            .into_owned()
    });
    std::fs::write(&path, encode_png(&canvas)).expect("записать превью");
    println!("превью ленты: {path}");
}

/// Превью ленты раскладок для двух окон — чтобы опознать карточку, на
/// которую показал пользователь, а не гадать по описанию таблицы.
#[test]
#[ignore = "инструмент разработки: пишет PNG, не проверяет инвариантов"]
fn preset_strip_preview_png() {
    use crate::preset_strip;

    let (w, h) = (900u32, 200u32);
    let mut canvas = Canvas::new((f64::from(w) * SCALE) as u32, (f64::from(h) * SCALE) as u32);
    backdrop(&mut canvas);

    let presets = rst_core::group_layout::presets_for(2).to_vec();
    println!("раскладок для двух окон: {}", presets.len());
    for (i, p) in presets.iter().enumerate() {
        let slots: Vec<String> = p
            .slots
            .iter()
            .map(|r| format!("({:.2},{:.2} {:.2}x{:.2})", r.x, r.y, r.w, r.h))
            .collect();
        println!("  #{i}: {}", slots.join(" "));
    }
    let screen = DipRect::new(0.0, 0.0, f64::from(w), f64::from(h));
    let mut strip = preset_strip::build(&presets, false, Some(0), &screen, 4, &[]);
    for _ in 0..60 {
        strip.panel.animate(50.0);
    }
    let mut prims = Vec::new();
    strip.panel.draw(&mut prims);
    draw_primitives(&mut canvas, &prims);

    let path = std::env::var("RESTICKER_PRESET_PREVIEW").unwrap_or_else(|_| {
        std::env::temp_dir()
            .join("resticker_preset_preview.png")
            .to_string_lossy()
            .into_owned()
    });
    std::fs::write(&path, encode_png(&canvas)).expect("записать превью");
    println!("превью раскладок: {path}");
}

/// Мультивыделение: общая рамка с ручками, тонкие контуры участников и общий
/// тулбар под ними (запрос пользователя 2026-09-06 — «один квадрат выделения,
/// скейлить и двигать одновременно»). Рисуется теми же строителями, что и
/// живой оверлей: `SelectionBox` для рамок, `build_toolbar` для панели.
#[test]
#[ignore = "инструмент разработки: пишет PNG, не проверяет инвариантов"]
fn group_selection_preview_png() {
    use rst_core::model::{MonitorId, Placement, Transform};
    use rst_render::{SELECTION_COLOR, SelectionBox};

    let (w, h) = (900u32, 460u32);
    let mut canvas = Canvas::new((f64::from(w) * SCALE) as u32, (f64::from(h) * SCALE) as u32);
    backdrop(&mut canvas);

    let place = |cx: f64, cy: f64, w: f64, h: f64| Placement {
        monitor_id: MonitorId("preview".to_string()),
        cx,
        cy,
        w,
        h,
    };
    let members = [
        place(240.0, 150.0, 200.0, 130.0),
        place(560.0, 210.0, 260.0, 160.0),
    ];

    let mut prims = Vec::new();
    // Сами стикеры — просто плашки: превью про рамки, а не про картинки.
    for m in &members {
        prims.push(Primitive::Fill {
            rect: Box2D {
                cx: m.cx,
                cy: m.cy,
                w: m.w,
                h: m.h,
                rotation: 0.0,
            },
            color: [0x20, 0x24, 0x30],
            opacity: 0.92,
        });
    }
    // Контур участника — приглушённый: он отмечает «этот входит в группу»,
    // а главная линия — общая рамка.
    for m in &members {
        for rect in SelectionBox::new(m, &Transform::default()).visuals().outline {
            prims.push(Primitive::Fill {
                rect,
                color: SELECTION_COLOR,
                opacity: 0.45,
            });
        }
    }
    // Общая рамка: union по краям участников, ручки — белые.
    let left = members
        .iter()
        .map(|m| m.cx - m.w / 2.0)
        .fold(f64::INFINITY, f64::min);
    let top = members
        .iter()
        .map(|m| m.cy - m.h / 2.0)
        .fold(f64::INFINITY, f64::min);
    let right = members
        .iter()
        .map(|m| m.cx + m.w / 2.0)
        .fold(f64::NEG_INFINITY, f64::max);
    let bottom = members
        .iter()
        .map(|m| m.cy + m.h / 2.0)
        .fold(f64::NEG_INFINITY, f64::max);
    let frame = place(
        (left + right) / 2.0,
        (top + bottom) / 2.0,
        right - left,
        bottom - top,
    );
    let group = SelectionBox::new(&frame, &Transform::default());
    let visuals = group.visuals();
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
    draw_primitives(&mut canvas, &prims);

    // Тулбар группы: ползунок прозрачности на месте (прежнее правило SPEC
    // «в мульти только кнопки» отменено), видео-виджеты — потому что в
    // выделении есть видео, глаз закрыт — потому что скрыт хотя бы один.
    let mut toolbar = build_toolbar(
        &DipRect::new(left, top, right - left, bottom - top),
        &ToolbarState {
            opacity: 0.6,
            visible: false,
            video: Some(VideoToolbarState {
                paused: true,
                show_timeline: false,
                volume_pct: 65,
                muted: false,
            }),
        },
        f64::from(h),
    );
    if let Some(volume) =
        toolbar.widget_mut::<rst_render::VolumeControl>(crate::toolbar::TB_VOLUME)
    {
        volume.set_hovered(true);
    }
    for _ in 0..40 {
        toolbar.animate(16.0);
    }
    let mut panel_prims = Vec::new();
    toolbar.draw(&mut panel_prims);
    draw_primitives(&mut canvas, &panel_prims);

    let path = std::env::var("RESTICKER_GROUP_PREVIEW").unwrap_or_else(|_| {
        std::env::temp_dir()
            .join("resticker_group_preview.png")
            .to_string_lossy()
            .into_owned()
    });
    std::fs::write(&path, encode_png(&canvas)).expect("записать превью мультивыделения");
    println!("превью мультивыделения: {path}");
}

/// Численный замер читаемости состояний видео-кнопок: яркость плашки
/// переключателя полосы и попарное отличие альфа-покрытия иконок динамика.
#[test]
#[ignore = "инструмент разработки: пишет PNG и печатает замеры различимости"]
fn video_control_states_measurement_png() {
    use rst_render::{Button, Icon};

    let (w, h) = (1200u32, 190u32);
    let mut off_canvas = Canvas::new((f64::from(w) * SCALE) as u32, (f64::from(h) * SCALE) as u32);
    let mut on_canvas = Canvas::new(off_canvas.w, off_canvas.h);
    backdrop(&mut off_canvas);
    on_canvas.px.copy_from_slice(&off_canvas.px);

    let toolbar_bounds = |cx| DipRect::from_center(cx, 80.0, 400.0, 40.0);
    let state = |show_timeline| ToolbarState {
        opacity: 0.8,
        visible: true,
        video: Some(VideoToolbarState {
            paused: false,
            show_timeline,
            volume_pct: 65,
            muted: false,
        }),
    };
    // Рендерим оба состояния в одной и той же точке на одинаковой подложке:
    // иначе горизонтальный градиент backdrop смешал бы яркость стекла с
    // разницей фона.
    let off_panel = build_toolbar(&toolbar_bounds(600.0), &state(false), f64::from(h));
    let on_panel = build_toolbar(&toolbar_bounds(600.0), &state(true), f64::from(h));
    let mut off_prims = Vec::new();
    off_panel.draw(&mut off_prims);
    draw_primitives(&mut off_canvas, &off_prims);
    let mut on_prims = Vec::new();
    on_panel.draw(&mut on_prims);
    draw_primitives(&mut on_canvas, &on_prims);

    let mean_brightness = |canvas: &Canvas, bounds: Box2D| {
        let inset = 2.0 * SCALE;
        let x0 = ((bounds.cx - bounds.w / 2.0) * SCALE + inset)
            .floor()
            .max(0.0) as u32;
        let x1 = ((bounds.cx + bounds.w / 2.0) * SCALE - inset)
            .ceil()
            .min(f64::from(canvas.w)) as u32;
        let y0 = ((bounds.cy - bounds.h / 2.0) * SCALE + inset)
            .floor()
            .max(0.0) as u32;
        let y1 = ((bounds.cy + bounds.h / 2.0) * SCALE - inset)
            .ceil()
            .min(f64::from(canvas.h)) as u32;
        let mut sum = 0.0;
        let mut count = 0u32;
        for y in y0..y1 {
            for x in x0..x1 {
                let i = ((y * canvas.w + x) * 4) as usize;
                // Relative luminance makes the number independent of the
                // equal RGB channels used by the button glass.
                sum += 0.2126 * f64::from(canvas.px[i])
                    + 0.7152 * f64::from(canvas.px[i + 1])
                    + 0.0722 * f64::from(canvas.px[i + 2]);
                count += 1;
            }
        }
        sum / f64::from(count.max(1))
    };
    let percent_delta = |a: f64, b: f64| 100.0 * (a - b).abs() / b.abs().max(f64::EPSILON);

    let button_bounds = |panel: &rst_render::Panel, id| {
        panel
            .widget::<rst_render::Button>(id)
            .expect("кнопка тулбара на месте")
            .bounds()
    };
    let off_timeline = mean_brightness(
        &off_canvas,
        button_bounds(&off_panel, crate::toolbar::TB_TIMELINE),
    );
    let on_timeline = mean_brightness(
        &on_canvas,
        button_bounds(&on_panel, crate::toolbar::TB_TIMELINE),
    );
    let off_play = mean_brightness(
        &off_canvas,
        button_bounds(&off_panel, crate::toolbar::TB_PLAY_PAUSE),
    );
    let on_play = mean_brightness(
        &on_canvas,
        button_bounds(&on_panel, crate::toolbar::TB_PLAY_PAUSE),
    );
    let timeline_delta = (on_timeline - off_timeline).abs();
    println!(
        "яркость плашки (luminance, inset 2 DIP): timeline off {off_timeline:.3}, on {on_timeline:.3}, "
    );
    println!(
        "  timeline delta: {timeline_delta:.3} ({:.2}% от off); play/pause: off {off_play:.3}, on {on_play:.3}; on timeline vs обычная кнопка: {:.3} ({:.2}%), off timeline vs обычная: {:.3} ({:.2}%)",
        percent_delta(on_timeline, off_timeline),
        (on_timeline - on_play).abs(),
        percent_delta(on_timeline, on_play),
        (off_timeline - off_play).abs(),
        percent_delta(off_timeline, off_play),
    );

    let icon_difference = |a: Icon, b: Icon, size: u32| {
        let lhs = icon_rgba(a, size);
        let rhs = icon_rgba(b, size);
        let mut different = 0u32;
        let mut nontransparent = 0u32;
        for (la, rb) in lhs.chunks_exact(4).zip(rhs.chunks_exact(4)) {
            if la[3] > 0 || rb[3] > 0 {
                nontransparent += 1;
                if la[3] != rb[3] {
                    different += 1;
                }
            }
        }
        (different, nontransparent, 100.0 * f64::from(different) / f64::from(nontransparent.max(1)))
    };
    let icon_side_dip = rst_render::theme::BUTTON_SIZE - 2.0 * rst_render::theme::BUTTON_PAD;
    for scale in [1.0, 2.0] {
        let side = (icon_side_dip * scale).round() as u32;
        println!("иконки динамика: N={side} px (сторона {icon_side_dip:.1} DIP × scale {scale:.1})");
        for (name, a, b) in [
            ("high vs low", Icon::VolumeHigh, Icon::VolumeLow),
            ("high vs mute", Icon::VolumeHigh, Icon::VolumeMute),
            ("low vs mute", Icon::VolumeLow, Icon::VolumeMute),
        ] {
            let (different, nontransparent, percent) = icon_difference(a, b, side);
            println!("  {name}: {different}/{nontransparent} пикселей альфы отличаются ({percent:.2}%)");
        }
    }

    // Отдельный кадр держит ровно те же реальные 30-DIP кнопки и 22-DIP
    // области иконок, чтобы метрики можно было сопоставить с картинкой.
    let mut preview = Canvas::new((520.0 * SCALE) as u32, (150.0 * SCALE) as u32);
    backdrop(&mut preview);
    let icons = [
        ("timeline off", Icon::TimelineOff, false),
        ("timeline on", Icon::Timeline, true),
        ("volume high", Icon::VolumeHigh, false),
        ("volume low", Icon::VolumeLow, false),
        ("volume mute", Icon::VolumeMute, false),
    ];
    for (i, (label, icon, toggled)) in icons.into_iter().enumerate() {
        let cx = 52.0 + i as f64 * 104.0;
        let button = Button::icon(i as u32, cx, 65.0, icon).toggled(toggled);
        let mut prims = Vec::new();
        button.draw(&mut prims);
        draw_primitives(&mut preview, &prims);
        let (tw, th) = text_size(label);
        draw_primitives(
            &mut preview,
            &[Primitive::Text {
                rect: Box2D {
                    cx,
                    cy: 112.0,
                    w: tw,
                    h: th,
                    rotation: 0.0,
                },
                text: label.to_string(),
                color: rst_render::theme::TEXT,
                opacity: 1.0,
            }],
        );
    }

    let path = std::env::var("RESTICKER_VIDEO_STATES_PREVIEW").unwrap_or_else(|_| {
        std::env::temp_dir()
            .join("resticker_video_control_states.png")
            .to_string_lossy()
            .into_owned()
    });
    std::fs::write(&path, encode_png(&preview)).expect("записать превью состояний видео-кнопок");
    println!("PNG состояний видео-кнопок: {path}");
}
