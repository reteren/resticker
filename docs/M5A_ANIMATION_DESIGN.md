# M5a — анимация (GIF / animated WebP / APNG): дизайн

Статус: в работе, автономная ночная сессия 2026-08-05/06. Пишется по тому же
шаблону, что `M4_WINDOW_PICKER_DESIGN.md` — контракт для параллельных
воркеров, чтобы не переспрашивать по мелочам ночью.

Скоуп по ROADMAP M5a: декод всех кадров в текстурный атлас, анимация через
смену UV, собственные часы на стикер, планировщик ближайшего дедлайна.
**Потоковый режим для очень длинных анимаций — отдельный пункт ROADMAP, в
эту ночь НЕ делается** (см. §5 «Отложено»), но decode-слой обязан не упасть/
не сожрать память на патологическом файле — это разные требования.

Опорный документ: `ARCHITECTURE.md` §4 «Как не убить процессор» (§4.2 —
частота кадров по требованию, ближайший дедлайн + waitable timer; §4.3 —
не тикать invisible/occluded; §4.4 — атлас, порог стриминга >300 кадров или
>256MB).

## 1. Данные не меняются (важно!)

`Sticker.source: StickerSource::File { path, media_type }` уже несёт
`MediaType::Animation` (`rst-core/src/model.rs:228-233`) — вариант существует,
просто никогда не выставляется (везде хардкод `MediaType::Image`). Часы
анимации (`frame_index`, `started_at`) — **чисто runtime state, не
персистится**, тем же паттерном, что `occluder_cache` в `overlay_manager.rs`
(локальная переменная `run()`, не поле `EditState`/`Config`). Схема конфига
**не меняется**, миграция v1→v2 не нужна для этого среза.

## 2. Слой декодирования — `rst-media` (Задача A)

Новый файл `crates/rst-media/src/animation.rs`.

```rust
pub struct DecodedFrame {
    pub rgba: Vec<u8>,   // straight-alpha RGBA8, len == width*height*4
    pub delay: Duration, // на кадр; 0 или подкадровые значения (GIF centisecond
                          // rounding) клэмпятся к минимуму 20ms (браузерная
                          // конвенция для "нулевой" задержки в GIF)
}

pub struct DecodedAnimation {
    pub width: u32,
    pub height: u32,
    pub frames: Vec<DecodedFrame>,
}

pub enum MediaError {
    // ...существующие варианты...
    TooManyFrames { count: usize, limit: usize },      // > 300
    TooLargeForAtlas { bytes: usize, limit: usize },   // > 256 * 1024 * 1024
    NotAnimated, // 0 или 1 кадр — вызывающий код (rst-media::sniff) обязан
                 // в этом случае трактовать файл как MediaType::Image, а не
                 // как ошибку пользователю
}

pub fn decode_animation(path: &Path) -> Result<DecodedAnimation, MediaError>;

/// Определяет MediaType по содержимому файла (не по расширению!) —
/// пробует decode_animation, на NotAnimated/любую ошибку декода падает
/// обратно на MediaType::Image (координатор должен уметь показать статик
/// даже если это на самом деле битый gif). Animation только когда decode_animation
/// вернул >= 2 кадра.
pub fn sniff_media_type(path: &Path) -> rst_core::model::MediaType;
```

Форматы через `image` crate 0.25 `AnimationDecoder` trait: GIF
(`image::codecs::gif::GifDecoder`), APNG (`image::codecs::png::PngDecoder` +
`.apng()`), animated WebP (`image::codecs::webp::WebPDecoder`). **Cargo.toml
правка обязательна**: `rst-media/Cargo.toml` сейчас включает только
`["png", "bmp"]` — добавить `"gif"`; для APNG отдельный feature-флаг не
нужен (тот же `png` кодек, `.apng()` метод), для animated WebP проверить,
нужен ли отдельный feature у `image` 0.25 (может быть уже включён через
основной `webp`).

Порог остановки — проверять **инкрементально по мере итерации**
`Frames`-итератора (`AnimationDecoder::into_frames()`), не после полной
материализации: считать накопленные кадры/байты на каждой итерации и
вернуть `Err` сразу, как только превышен любой из порогов, вместо того
чтобы держать в памяти сотни мегабайт ради последующей проверки. Вынести
чистую функцию `fn check_thresholds(frame_count: usize, total_bytes: usize)
-> Result<(), MediaError>` — так порог тестируется без реального
огромного файла-фикстуры.

Тесты (по образцу `rst-media/src/paste.rs` — синтетические фикстуры
`image`-крейтом в памяти, без файлов на диске): 2-кадровый GIF декодируется
верно (кадры, delay), 1-кадровый GIF/PNG даёт `NotAnimated`, delay=0
клэмпится к 20ms, `check_thresholds` реджектит по count и по bytes отдельно
(юнит-тест на чистую функцию, без реального 256MB файла).

## 3. Текстурный атлас и UV — `rst-render` (Задача B)

Новый файл `crates/rst-render/src/atlas.rs` (или расширение `texture.rs`).

```rust
pub struct AtlasFrame {
    pub uv_offset: [f32; 2],
    pub uv_scale: [f32; 2],
    pub delay: Duration,
}

pub struct TextureAtlas {
    pub texture: Texture,
    pub frames: Vec<AtlasFrame>,
}

impl Device {
    pub fn create_texture_atlas(
        &self,
        frames: &[(Vec<u8>, Duration)], // straight-alpha RGBA8, все одного w×h
        frame_w: u32,
        frame_h: u32,
    ) -> Result<TextureAtlas, RenderError>;
}
```

**Раскладка — грид, не горизонтальная полоса.** `columns =
ceil(sqrt(frame_count))`, `rows = ceil(frame_count / columns)`, кадр `i` в
ячейке `(col = i % columns, row = i / columns)`, `atlas_w = columns *
frame_w`, `atlas_h = rows * frame_h`, `uv_offset = (col/columns,
row/rows)`, `uv_scale = (1/columns, 1/rows)`. Причина не брать горизонтальную
полосу: она быстро упирается в лимит текстуры D3D11 feature level 11
(16384px) даже при скромном числе кадров у стикера покрупнее. Явно
проверить `atlas_w <= 16384 && atlas_h <= 16384` и вернуть чистую ошибку
(не отдавать драйверу непроверенный размер).

`Sprite` (`sprite.rs`) получает 2 новых поля с дефолтом «вся текстура»:

```rust
pub struct Sprite {
    pub texture: Texture,
    pub placement: Placement,
    pub transform: Transform,
    pub uv_offset: [f32; 2], // default [0.0, 0.0]
    pub uv_scale: [f32; 2],  // default [1.0, 1.0]
}
```

`Sprite::new(...)` — **сигнатура не меняется**, инициализирует
`uv_offset=[0,0]`/`uv_scale=[1,1]` (чтобы все существующие M1-M4 call sites
не трогать). Добавить `Sprite::with_uv(self, offset: [f32;2], scale:
[f32;2]) -> Self` builder-методом для анимированного случая.

`SpriteParams` constant buffer (`device.rs`, сейчас 48 байт, `repr(C)`,
поля на офсетах 0/16/32) — добавить `uv_offset`/`uv_scale` (16 байт,
выравнивание HLSL cbuffer не нарушается, 48+16=64). Шейдер (`shader.rs`
/ HLSL источник `mainVS`/`mainPS`/`mainMaskPS`): `finalUV = uv_offset +
rawUV * uv_scale` перед семплированием текстуры. `Device::draw`/
`draw_masked` — при заполнении `Map`/`Unmap` CB на каждой итерации писать
эти 2 поля из `sprite.uv_offset`/`uv_scale`.

Тест (обязательно GPU, по образцу `draw_mask_produces_expected_coverage` в
`device.rs::gpu_tests`): собрать 2-кадровый атлас с чётко различимыми
сплошными цветами (кадр 0 — красный, кадр 1 — синий), нарисовать спрайт с
`uv_offset`/`uv_scale`, указывающими на кадр 1, прочитать пиксели через
staging-текстуру и убедиться, что нарисован синий, а не красный —
доказывает, что UV remap реально работает через шейдер, а не только в
юнит-тесте математики.

## 4. Часы анимации — `rst-core` (Задача C)

Новый файл `crates/rst-core/src/animation_clock.rs`, чистые функции/структуры,
без GPU/окно-зависимостей (тестируется как арифметика `Instant`/`Duration`).

```rust
pub struct AnimationClock {
    pub frame_index: usize,
    pub frame_started_at: Instant,
}

impl AnimationClock {
    pub fn new(now: Instant) -> Self;

    /// Продвигает frame_index вперёд по мере необходимости, учитывая
    /// per-frame delays. ВАЖНО: если процесс был заблокирован/усыплён надолго
    /// (сон системы, тяжёлая пауза), НЕ крутить наивный while-цикл по каждому
    /// прошедшему кадру (может быть тысячи итераций) — считать позицию через
    /// модульную арифметику по суммарной длительности цикла анимации.
    /// Возвращает true, если frame_index реально изменился (нужен redraw).
    pub fn advance(&mut self, now: Instant, frame_delays: &[Duration]) -> bool;

    /// Ближайший момент, когда часы должны быть продвинуты снова.
    pub fn next_deadline(&self, frame_delays: &[Duration]) -> Instant;
}
```

Контракт для патологических случаев (обязательно покрыть тестами):
- `frame_delays.len() == 1` — анимация не тикает никогда (`advance` всегда
  `false`, `next_deadline` — что-то очень далёкое, например `now +
  Duration::from_secs(3600)`, не `Instant::MAX`, чтобы не переполнить
  арифметику где-то выше по стеку).
- `frame_delays.is_empty()` — программная ошибка вызывающего кода;
  задокументировать явно (паника с понятным сообщением — это **зона
  ответственности часов, не защита на каждом уровне выше**, вызывающая
  сторона (coordinator) обязана гарантировать непустой список кадров прежде
  чем вообще создавать `AnimationClock` для стикера).
- Долгая пауза (например, 10 секунд elapsed против кадров по 100ms) должна
  корректно приземлиться на правильный кадр по модулю суммарной
  длительности цикла, без цикла из ~100 итераций.

Юнит-тесты: обычное продвижение на 1 кадр, "догонялки" после длинной паузы
(модульная арифметика, не итеративный while), одно-кадровый список никогда
не тикает, `next_deadline` монотонно корректен при вызовах подряд без
`advance` между ними.

## 5. Сшивка в координатор (делаю сам, `overlay_manager.rs`)

Не для воркеров — самый рискованный интеграционный код, тот же паттерн,
что маска перекрытия (M4) и панель выбора окон.

- Точка добавления стикера (`overlay_manager.rs:~4568`, `handle add
  sticker`): заменить хардкод `MediaType::Image` на
  `rst_media::animation::sniff_media_type(&path)`. Если `Animation` —
  вместо `device.load_image` вызвать `decode_animation` +
  `device.create_texture_atlas`, завести `AnimationClock::new(now)` в новой
  локальной мапе координатора `animations: HashMap<Uuid, (TextureAtlas,
  AnimationClock)>` (тот же паттерн локальной runtime-структуры, что
  `occluder_cache`/`window_snapshot` — НЕ поле `EditState`).
- Новый вариант сообщения `OverlayMessage::AnimationTick` + планировщик:
  выделенный поток (тот же паттерн, что `Tick`-поток для потери монитора,
  `overlay_manager.rs:957-965`), но с **переменным** интервалом вместо
  фиксированного 1с. Канал `mpsc::Sender<Option<Instant>>` от координатора
  к потоку-планировщику для обновления ближайшего дедлайна; поток блокируется
  на `recv_timeout(deadline - now)`, при таймауте шлёт
  `OverlayMessage::AnimationTick` в основной канал и блокируется на `recv()`
  (ждёт следующий дедлайн от координатора — тот пересчитает и пришлёт снова
  после каждого redraw). Координатор пересчитывает
  `min(animations.values().filter(visible_and_unoccluded).map(next_deadline))`
  и шлёт в канал планировщика, только если дедлайн реально изменился.
- Пауза тика для invisible/occluded стикеров (ARCHITECTURE §4.3): часы не
  продвигаются и не участвуют в min-дедлайне, если стикер скрыт (`!visible`)
  или полностью перекрыт (тот же предикат, что уже отсекает полностью
  перекрытые стикеры из кадра в маске M4).
- Пауза на `SessionLocked` (SPEC §9, уже есть TODO-комментарий на месте
  обработчика `overlay_manager.rs:~1505`): не продвигать часы, пока сессия
  заблокирована — не слать координатору новый дедлайн, пока не придёт
  `SessionUnlocked`.
- `redraw()`: для стикера с `MediaType::Animation` брать `Sprite::with_uv(...)`
  из текущего `frame_index` часов вместо статичного `Sprite::new`.
- Удаление стикера: чистить `animations` мапу по `id` (симметрично тому,
  как `sprites`/`occluder_cache` уже чистятся при delete).

## 6. Порядок работы этой ночи

Задачи A/B/C — независимы друг от друга и от координатора, диспатчатся
параллельно трём из четырёх воркеров, каждая с независимым ревью после
готовности (тот же цикл, что вся сессия M4: build+test+clippy+fmt →
коммит → ревью → фикс при находках → коммит). Четвёртый воркер — на
подхвате для ревью первой готовой задачи. Сшивка (§5) — после того как A/B/C
все прошли ревью и слиты, делаю сам, отдельными коммитами, с финальным
независимым ревью сшивки (тот же паттерн, что `9a0c565`→`db73817`).

## 7. Отложено явно (не в этот срез)

- Потоковый режим для анимаций >300 кадров/>256MB — decode-слой честно
  отказывает (`TooManyFrames`/`TooLargeForAtlas`), UI-сообщение
  пользователю про "анимация слишком большая" — отдельный срез
  (нужно решение по UX ошибки, не техническое).
- `PlaybackSettings.speed`/`loop_mode` — поля уже есть в модели, но
  фактическое применение (ускорение/замедление тика, once vs loop) — если
  время ночью останется, добавить как часть §5 (`speed` — множитель на
  `delay` в `next_deadline`; `loop_mode` — при `frame_index` дошедшем до
  конца списка либо wrap на 0 (loop), либо остановиться на последнем кадре
  и перестать тикать (once)). Если не успею — зафиксировать как известный
  пробел в CURRENT.md, не выдавать за готово.
- M5a UI (добавление анимации через диалог/буфер уже работает — тот же
  путь, что PNG, разницы для пользователя в UI нет; сшивка использует
  `decode_animation` напрямую, не `sniff_media_type`, чтобы не декодировать
  дважды — см. ниже).
- Найдено независимым ревью (низкая серьёзность, не исправлено этой ночью):
  `decode_animation` проверяет порог 256MB только ПОСЛЕ материализации
  очередного кадра в память — файл с честным магическим заголовком, но
  ложно заявленным огромным разрешением кадра (например, 30000×30000 в
  100-байтном GIF), вынуждает один транзитный аллок ~3.6GB перед чистым
  отказом `TooLargeForAtlas` (паники нет, память освобождается). Требует
  разбора заголовка контейнера ДО декода первого кадра (формато-специфично
  для каждого из трёх форматов) — не сделано, память ограничена (не
  бесконечный рост), риск принят для этого среза.
- Найдено тем же ревью: тестовое покрытие animated-WebP пути в
  `decode_animation` — 0 (GIF/APNG покрыты, animated WebP — нет).
  Функционально путь не отличается от GIF/APNG (общий `AnimationDecoder`
  trait), но заслуживает отдельного теста с реальной WebP-анимацией.
