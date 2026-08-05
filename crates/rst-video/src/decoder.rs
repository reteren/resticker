//! Декодер-поток `rst-video`: владеет `Pipeline` (единственный поток, где
//! трогаются FFmpeg-контексты) и обслуживает очередь кадров/звука для
//! координатора (docs/M5B_VIDEO_DESIGN.md §2).
//!
//! Контракты потока:
//! - **Пауза** — не тянет пакеты вообще (`recv_timeout` вместо чтения файла),
//!   CPU в простое нулевой; любое сообщение из канала при этом ОБРАБАТЫВАЕТСЯ
//!   (не выбрасывается — иначе Play/Seek/Shutdown, пришедшие на паузе,
//!   терялись бы и поток не выходил из паузы никогда).
//! - **Темп декодирования** — декодер не убегает вперёд: кадр кладётся в
//!   очередь не раньше своего PTS относительно `Instant`-якоря (`anchor`);
//!   полная очередь — кадр отбрасывается (координатор медленнее реального
//!   времени: пропуск кадров здесь и есть корректное поведение, очередь
//!   всегда несёт самые свежие кадры).
//! - **Зацикливание** — на EOF всегда `av_seek_frame` в начало (срез M5b:
//!   семантика Once/HoldLastFrame отложена, design §2/§8).
//! - **Живучесть** — все ожидания прерываемы: команды доезжают с задержкой
//!   ≤ 100 мс, поток завершается по Shutdown всегда, блокирующихся навсегда
//!   ожиданий нет (join в `VideoSource::drop` гарантированно завершается).

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::time::{Duration, Instant};

use tracing::{debug, warn};

use crate::error::VideoError;
use crate::pipeline::{AudioChunkOut, Event, Pipeline, VideoFrameOut};

/// Ёмкость очереди видеокадров (design §2: очередь на 2-3 кадра).
pub(crate) const FRAME_QUEUE_CAPACITY: usize = 3;
/// Ёмкость очереди звука (порция ≈ 20-40 мс — это ~1 с буфера).
pub(crate) const AUDIO_QUEUE_CAPACITY: usize = 32;
/// Срез сна пайсинга: команды (пауза/перемотка/завершение) доезжают с
/// задержкой ≤ 100 мс.
const PACE_SLICE: Duration = Duration::from_millis(100);
/// Период проверки команд на паузе.
const PAUSE_POLL: Duration = Duration::from_millis(100);

/// Команда декодер-потоку от [`crate::VideoSource`].
pub(crate) enum Control {
    Play,
    Pause,
    Seek {
        to: Duration,
        /// Подтверждение выполнения перемотки (ждёт `VideoSource::seek`).
        ack: mpsc::Sender<Result<(), VideoError>>,
    },
    Shutdown,
}

/// Состояние, разделяемое между `VideoSource` и декодер-потоком.
pub(crate) struct Shared {
    pub paused: std::sync::atomic::AtomicBool,
    pub volume: std::sync::Mutex<f32>,
}

/// Запрошенная перемотка: применяется к конвейеру до чтения новых пакетов.
struct PendingSeek {
    to: Duration,
    ack: mpsc::Sender<Result<(), VideoError>>,
}

/// Якорь пайсинга: `anchor` — `Instant`, от которого считаются тайминги
/// кадров (`anchor + pts` = момент показа). Сбрасывается (None) после
/// перемотки, перезапуска и возобновления с паузы — следующий кадр заново
/// якорит таймлайн (в сшивке координатора точную A/V-синхронизацию решит
/// аудио-клок, design §2 «Часы»).
#[derive(Default)]
struct Pacing {
    anchor: Option<Instant>,
}

impl Pacing {
    fn reset(&mut self) {
        self.anchor = None;
    }
}

/// Вход декодер-потока: открывает файл, шлёт результат в `info_tx`, затем
/// крутит цикл демукса/декода до Shutdown.
#[allow(clippy::too_many_arguments)]
pub(crate) fn decoder_thread(
    path: std::path::PathBuf,
    ctl_rx: Receiver<Control>,
    frame_tx: SyncSender<VideoFrameOut>,
    audio_tx: SyncSender<AudioChunkOut>,
    info_tx: mpsc::Sender<Result<VideoInfo, VideoError>>,
    shared: Arc<Shared>,
    audio_target: crate::pipeline::AudioTarget,
) {
    // Открытие и проверка первого кадра (формат пикселя/размеры) — здесь, в
    // потоке: все FFmpeg-вызовы одного файла живут на одной нити.
    let mut pipe = match Pipeline::open(&path, audio_target) {
        Ok(pipe) => pipe,
        Err(e) => {
            let _ = info_tx.send(Err(e));
            return;
        }
    };
    let info = VideoInfo {
        width: pipe.dimensions().0,
        height: pipe.dimensions().1,
        duration: pipe.duration(),
        has_audio: pipe.has_audio(),
    };
    if info_tx.send(Ok(info)).is_err() {
        // `VideoSource::open` нас не ждёт (отменил открытие) — выходим.
        return;
    }
    debug!(?path, "декодер запущен: {info:?}");

    let mut pacing = Pacing::default();
    let mut pending_seek: Option<PendingSeek> = None;
    let mut paused = shared.paused.load(Ordering::Relaxed);

    loop {
        // 1. Команды (и только команды — пакеты не читаются).
        let was_paused = paused;
        if drain_commands(&ctl_rx, &mut paused, &mut pending_seek, &shared) {
            break; // Shutdown
        }
        if was_paused && !paused {
            // Возобновление с паузы: старый якорь датирован до-паузным
            // моментом — без сброса все кадры после паузы длительностью P
            // отдаются мгновенно "вдогонку", видео пропускает P секунд
            // контента. Найдено независимым ревью: докком `Pacing` обещал
            // сброс "и возобновления с паузы", код его не делал.
            pacing.reset();
        }

        // 2. Перемотка — до чтения новых пакетов (безопасная точка: между
        //    обработками пакетов, никакие FFmpeg-объекты не заимствованы).
        if let Some(seek) = pending_seek.take() {
            let result = pipe.seek(seek.to);
            if let Err(e) = &result {
                warn!(?path, "seek({:?}) не удался: {e}", seek.to);
            }
            let _ = seek.ack.send(result);
            pacing.reset();
        }

        // 3. Пауза: не тянем пакеты вообще — ждём команду (нулевой CPU).
        //    Важно: сообщение из recv_timeout ОБРАБАТЫВАЕТСЯ, а не
        //    выбрасывается (иначе Play/Seek, пришедшие на паузе, терялись бы).
        if paused {
            match ctl_rx.recv_timeout(PAUSE_POLL) {
                Ok(cmd) => {
                    let was_paused = paused;
                    if apply_command(cmd, &mut paused, &mut pending_seek, &shared) {
                        break; // Shutdown
                    }
                    if was_paused && !paused {
                        pacing.reset();
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            continue;
        }

        // 4. Один шаг декода.
        match pipe.next() {
            Ok(Event::Video(frame)) => {
                // pace_to прерывается командой из ctl_rx, а не спит напролёт
                // весь остаток (найдено независимым ревью: иначе Shutdown/
                // Pause/Seek задерживались бы на весь PACE_SLICE-остаток —
                // секунды/минуты на файле с аномальным скачком PTS).
                if let Some(cmd) = pace_to(&mut pacing, frame.pts, &ctl_rx) {
                    if apply_command(cmd, &mut paused, &mut pending_seek, &shared) {
                        break; // Shutdown
                    }
                    // Кадр уже декодирован — не выбрасываем проделанную
                    // работу, отправляем как обычно; если команда была
                    // Pause/Seek, она подхватится на следующей итерации
                    // цикла (шаг 2/3 выше).
                }
                // Полная очередь — кадр отбрасывается: координатор медленнее
                // реального времени, пропуск кадров — корректное поведение
                // (очередь несёт самые свежие кадры, см. доку модуля).
                let _ = frame_tx.try_send(frame);
            }
            Ok(Event::Audio(chunk)) => {
                // Звук не блокируем: полная очередь — пропуск порции (редко:
                // ёмкость ~1 с, микшер выгребает каждый колбэк).
                let _ = audio_tx.try_send(chunk);
            }
            Ok(Event::Idle) => {}
            Ok(Event::Eof) => {
                // Всегда зацикливаем (дизайн §2/§8: Loop — единственная
                // семантика в этом срезе).
                if let Err(e) = pipe.loop_restart() {
                    warn!(?path, "перезапуск цикла после EOF не удался: {e}");
                }
                pacing.reset();
            }
            Err(e) => {
                // Битый участок файла: ошибка чтения контейнера — фатальна
                // для позиции; лечимся перезапуском цикла.
                warn!(?path, "ошибка декода: {e} — перезапуск цикла");
                if let Err(seek_err) = pipe.loop_restart() {
                    warn!(?path, "перезапуск цикла не удался: {seek_err}");
                }
                pacing.reset();
            }
        }
    }
    debug!(?path, "декодер остановлен");
}

/// Выбрать все накопленные команды (неблокирующе). Возвращает `true` при
/// Shutdown/разрыве канала.
fn drain_commands(
    ctl_rx: &Receiver<Control>,
    paused: &mut bool,
    pending_seek: &mut Option<PendingSeek>,
    shared: &Shared,
) -> bool {
    loop {
        match ctl_rx.try_recv() {
            Ok(cmd) => {
                if apply_command(cmd, paused, pending_seek, shared) {
                    return true;
                }
            }
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => return true,
        }
    }
}

/// Применить одну команду. Возвращает `true` при Shutdown.
fn apply_command(
    cmd: Control,
    paused: &mut bool,
    pending_seek: &mut Option<PendingSeek>,
    shared: &Shared,
) -> bool {
    match cmd {
        Control::Play => {
            *paused = false;
            shared.paused.store(false, Ordering::Relaxed);
        }
        Control::Pause => {
            *paused = true;
            shared.paused.store(true, Ordering::Relaxed);
        }
        Control::Seek { to, ack } => *pending_seek = Some(PendingSeek { to, ack }),
        Control::Shutdown => return true,
    }
    false
}

/// Задержать выдачу кадра до его тайминга: `anchor + pts` — момент показа.
/// Кадры до первого (после сброса якоря) отдаются сразу, якорь при этом
/// выставляется так, что таймлайн не прыгает: `anchor = now - pts`.
///
/// Прерывается командой из `ctl_rx`: если во время ожидания приходит
/// Play/Pause/Seek/Shutdown, возвращает её НЕМЕДЛЕННО (не потребляя остаток
/// ожидания) — вызывающий код обязан применить её через `apply_command`.
/// Раньше спал `thread::sleep`-срезами вслепую, не проверяя канал команд —
/// независимое ревью нашло, что это давало задержку Shutdown/Pause/Seek на
/// весь оставшийся сон (секунды-минуты на файле с аномальным скачком PTS),
/// хотя докомментарий заявлял «≤ 100 мс». `recv_timeout` вместо
/// `thread::sleep` даёт то же ограничение сверху (`PACE_SLICE`), но
/// просыпается сразу, как только команда реально приходит.
fn pace_to(pacing: &mut Pacing, pts: Duration, ctl_rx: &Receiver<Control>) -> Option<Control> {
    let anchor = match pacing.anchor {
        Some(anchor) => anchor,
        None => {
            let now = Instant::now();
            pacing.anchor = Some(now.checked_sub(pts).unwrap_or(now));
            return None;
        }
    };
    let target = anchor + pts;
    let mut remaining = target.saturating_duration_since(Instant::now());
    while !remaining.is_zero() {
        let slice = remaining.min(PACE_SLICE);
        match ctl_rx.recv_timeout(slice) {
            Ok(cmd) => return Some(cmd),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            // Отправитель (VideoSource) исчез без явного Shutdown — не должно
            // происходить в штатной работе (Drop всегда шлёт Shutdown), но
            // трактуем как Shutdown защитно, а не зависаем в ожидании.
            Err(mpsc::RecvTimeoutError::Disconnected) => return Some(Control::Shutdown),
        }
        remaining = remaining.saturating_sub(slice);
    }
    None
}

/// Статические данные открытого файла (отправляются из потока в `open`).
#[derive(Debug, Clone, Copy)]
pub(crate) struct VideoInfo {
    pub width: u32,
    pub height: u32,
    pub duration: Option<Duration>,
    pub has_audio: bool,
}
