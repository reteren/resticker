//! Полоса перемотки видео-стикера (задача «таймлайн видео»): тонкая дорожка
//! внизу видео с ручкой, подписями времени по краям и перемоткой
//! кликом/перетаскиванием. Чистый виджет: GPU здесь нет, перемотку к
//! `rst_video::VideoSource::seek` подключает координатор через
//! [`VideoTimeline::take_seek`].
//!
//! Почему не переиспользован [`crate::widgets::Slider`] (`crate::widgets::Slider`): у него
//! целочисленное значение — проценты прозрачности. Для перемотки целые
//! проценты означали бы шаг 36 секунд на часовом видео; позиция тут —
//! непрерывная (секунды в `f64`), и это единственное, что делает перемотку
//! плавной на любой длительности.
//!
//! Контракт взаимодействия с панелью повторяет [`crate::widgets::Slider`] один-в-один:
//! `Down` по полосе начинает захват и сразу ставит позицию (клик =
//! перемотка); `Move` обновляет позицию только при активном захвате (панель
//! роутит все `Move`/`Up` виджету с захватом, даже когда курсор ушёл за
//! границы — перетаскивание не «отваливается» на краю); `Up` отпускает
//! захват. Новый путь ввода координатору не нужен.
//!
//! Ключевое отличие от [`crate::widgets::Slider`] — источник истины позиции. Пока ручка
//! тащится, позиция живёт в виджете (палец), а внешняя
//! [`VideoTimeline::set_position`] игнорируется: видео продолжает играть,
//! каждый кадр присылает свежую позицию, и без этого правила ручка дралась
//! бы с пальцем, откатываясь на «где реально играет» при каждом движении.
//! После `Up` внешняя позиция снова принимается — полоса плавно догоняет
//! кадр, на который перемотали.
//!
//! Отрисовка рассчитана на чтение поверх ЛЮБОГО кадра: подложка —
//! полупрозрачная тёмная полоса на всю ширину (как у нормальных плееров).
//! Без неё светлая дорожка теряется на белом видео, тёмная — на тёмном;
//! подложка снимает вопрос фона целиком.
//!
//! Сознательное отступление от ТЗ «круглая ручка»: в наборе примитивов
//! [`Primitive`] нет фигуры круга (только `Fill`/`Icon`/`Rgba`/`Text`), и
//! ручка рисуется квадратной — тем же `Fill`, что и у существующего
//! [`crate::widgets::Slider`]. Сменить форму на круглую без нового примитива (или
//! растрового `Rgba`-круга с кэшем текстур на вызывающем слое) нельзя.

use rst_core::hittest::{to_local, to_world};

use crate::{Box2D, HighlightKind, PointerEvent, Primitive, Widget, WidgetId, text_size, theme};

/// Отступ полосы от краёв стикера, DIP.
pub const TIMELINE_MARGIN: f64 = 8.0;
/// Высота полосы, DIP: дорожка с ручкой плюс подписи времени.
pub const TIMELINE_HEIGHT: f64 = 24.0;
/// Минимальная ширина полосы, DIP. Уже некуда: подписи времени и ручка
/// сливаются в кашу, рисовать полосу на таком стикере бессмысленно —
/// [`timeline_bounds`] вернёт `None`.
pub const TIMELINE_MIN_WIDTH: f64 = 120.0;
/// Толщина дорожки в покое, DIP.
pub const TIMELINE_TRACK_H: f64 = 2.0;
/// Толщина дорожки при наведении/перетаскивании, DIP: толще — заметнее, но
/// и в покое дорожка не исчезает (режим редактирования показывает полосу
/// всегда).
pub const TIMELINE_TRACK_H_HOVER: f64 = 3.0;
/// Сторона ручки, DIP.
pub const TIMELINE_KNOB: f64 = 10.0;
/// Непрозрачность подложки в покое: полупрозрачная, чтобы полоса не
/// заслоняла кадр целиком, когда к ней не прикасаются.
pub const TIMELINE_BG_OPACITY: f64 = 0.5;
/// Непрозрачность подложки при наведении/перетаскивании.
pub const TIMELINE_BG_OPACITY_HOVER: f64 = 0.85;
/// Непрозрачность подписей времени в покое.
pub const TIMELINE_TEXT_OPACITY: f64 = 0.75;
/// Непрозрачность подписей при наведении/перетаскивании.
pub const TIMELINE_TEXT_OPACITY_HOVER: f64 = 0.95;

/// Полоса перемотки видео-стикера: значение — позиция воспроизведения в
/// секундах (`f64`), отрисовка — дорожка, залитая часть слева (уже
/// проигранное), ручка и подписи `m:ss`/`h:mm:ss` по краям.
pub struct VideoTimeline {
    id: WidgetId,
    bounds: Box2D,
    /// Длительность видео, секунды; `0` — длительность неизвестна/не задана
    /// (полоса рисует пустую дорожку, перемотка никуда не ведёт).
    duration_secs: f64,
    /// Внешняя позиция воспроизведения, секунды. Источник истины ТОЛЬКО
    /// вне перетаскивания: во время drag её место занимает `drag_secs`.
    position_secs: f64,
    /// Ручка перетаскивается указателем (захват удерживается панелью).
    dragging: bool,
    /// Позиция пальца, секунды; источник истины, пока `dragging`.
    drag_secs: f64,
    /// Запрос перемотки, ещё не забранный координатором
    /// ([`VideoTimeline::take_seek`]).
    seek: Option<f64>,
    /// Курсор над полосой (или координатор включил появление при наведении
    /// на стикер): влияет на прозрачность/толщину.
    hovered: bool,
}

impl VideoTimeline {
    /// Полоса длительности `duration_secs` с текущей позицией
    /// `position_secs` (секунды; позиция поджимается к длительности).
    pub fn new(id: WidgetId, bounds: Box2D, duration_secs: f64, position_secs: f64) -> Self {
        let duration = duration_secs.max(0.0);
        Self {
            id,
            bounds,
            duration_secs: duration,
            position_secs: position_secs.clamp(0.0, duration),
            dragging: false,
            drag_secs: 0.0,
            seek: None,
            hovered: false,
        }
    }

    /// Текущая позиция, секунды: во время перетаскивания — позиция пальца
    /// (полоса показывает, куда дотащили, а не где играет видео), иначе —
    /// внешняя позиция воспроизведения.
    pub fn position(&self) -> f64 {
        if self.dragging {
            self.drag_secs
        } else {
            self.position_secs
        }
    }

    /// Запрос перемотки с последнего опроса (сбрасывается): последняя
    /// позиция, в которую кликнули/дотащили, в секундах. Координатор
    /// забирает один раз и перематывает к ней — как `take_changed` у
    /// [`crate::widgets::Slider`].
    pub fn take_seek(&mut self) -> Option<f64> {
        self.seek.take()
    }

    /// Внешняя позиция воспроизведения, секунды. ИГНОРИРУЕТСЯ во время
    /// перетаскивания (см. доккомент модуля — палец, а не видео, ведёт
    /// ручку); после `Up` снова принимается.
    pub fn set_position(&mut self, secs: f64) {
        if self.dragging {
            return;
        }
        self.position_secs = secs.clamp(0.0, self.duration_secs);
    }

    /// Длительность видео, секунды (`0` — неизвестна). Позиции поджимаются
    /// к новой длительности: укоротили видео — полоса не висит за правым
    /// краем.
    pub fn set_duration(&mut self, secs: f64) {
        self.duration_secs = secs.max(0.0);
        self.position_secs = self.position_secs.clamp(0.0, self.duration_secs);
        self.drag_secs = self.drag_secs.clamp(0.0, self.duration_secs);
    }

    /// Локальный X точки указателя. Полоса может быть повёрнута вместе со
    /// стикером ([`timeline_bounds`] переносит поворот): мониторные
    /// координаты переводятся в локальную систему полосы, иначе drag на
    /// повёрнутом стикере маппировался бы мимо дорожки.
    fn local_x(&self, pos: (f64, f64)) -> f64 {
        let (lx, _) = to_local(
            self.bounds.cx,
            self.bounds.cy,
            self.bounds.rotation,
            pos.0,
            pos.1,
        );
        lx
    }

    /// Диапазон X дорожки в локальных координатах, доступный центру ручки:
    /// ручка не вылезает за края полосы (как у [`crate::widgets::Slider`]).
    fn track_range(&self) -> (f64, f64) {
        let half = TIMELINE_KNOB / 2.0;
        (-self.bounds.w / 2.0 + half, self.bounds.w / 2.0 - half)
    }

    /// Локальный X центра ручки для позиции `secs`.
    fn secs_to_x(&self, secs: f64) -> f64 {
        let (x0, x1) = self.track_range();
        let t = if self.duration_secs > 0.0 {
            (secs / self.duration_secs).clamp(0.0, 1.0)
        } else {
            0.0
        };
        x0 + t * (x1 - x0)
    }

    /// Секунды по локальному X: линейная интерполяция по дорожке, отсечение
    /// к диапазону — увод указателя за край полосы не ломает drag и не
    /// уводит позицию за `[0, duration]`.
    fn x_to_secs(&self, lx: f64) -> f64 {
        let (x0, x1) = self.track_range();
        if x1 <= x0 {
            // Дорожка вырождена (полоса уже ручки): позиция только 0 —
            // деления на нулевую длину хода нет.
            return 0.0;
        }
        let t = ((lx - x0) / (x1 - x0)).clamp(0.0, 1.0);
        t * self.duration_secs
    }

    /// Поставить позицию пальца по локальному X и заявить перемотку. Клик и
    /// каждое движение пальца — перемотка: даже «клик в текущую позицию»
    /// безопасен (перемотка к тому же месту — no-op для видео).
    fn set_from_local_x(&mut self, lx: f64) {
        self.drag_secs = self.x_to_secs(lx);
        self.seek = Some(self.drag_secs);
    }

    /// Активна ли полоса (наведение или перетаскивание) — от этого зависит
    /// контраст.
    fn active(&self) -> bool {
        self.hovered || self.dragging
    }

    /// Непрозрачность подложки.
    fn bg_opacity(&self) -> f64 {
        if self.active() {
            TIMELINE_BG_OPACITY_HOVER
        } else {
            TIMELINE_BG_OPACITY
        }
    }

    /// Непрозрачность подписей времени.
    fn text_opacity(&self) -> f64 {
        if self.active() {
            TIMELINE_TEXT_OPACITY_HOVER
        } else {
            TIMELINE_TEXT_OPACITY
        }
    }

    /// Толщина дорожки.
    fn track_h(&self) -> f64 {
        if self.active() {
            TIMELINE_TRACK_H_HOVER
        } else {
            TIMELINE_TRACK_H
        }
    }

    /// Прямоугольник примитива по центру в локальных координатах полосы
    /// (поворот переносится с полосы — см. [`timeline_bounds`]).
    fn local_rect(&self, lx: f64, ly: f64, w: f64, h: f64) -> Box2D {
        let (cx, cy) = to_world(self.bounds.cx, self.bounds.cy, self.bounds.rotation, lx, ly);
        Box2D {
            cx,
            cy,
            w,
            h,
            rotation: self.bounds.rotation,
        }
    }
}

impl Widget for VideoTimeline {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn bounds(&self) -> Box2D {
        self.bounds
    }

    fn set_bounds(&mut self, bounds: Box2D) {
        self.bounds = bounds;
    }

    fn draw(&self, out: &mut Vec<Primitive>) {
        let (x0, x1) = self.track_range();
        let kx = self.secs_to_x(self.position());
        // Порядок «нижний — первым»: подложка, дорожка, заливка, ручка,
        // подписи (подписи поверх всего — их должно быть видно всегда).
        out.push(Primitive::Fill {
            rect: self.bounds,
            color: theme::LOCK_INDICATOR_BG,
            opacity: self.bg_opacity(),
        });
        out.push(Primitive::Fill {
            rect: self.local_rect((x0 + x1) / 2.0, 0.0, x1 - x0, self.track_h()),
            color: theme::SLIDER_TRACK,
            opacity: 1.0,
        });
        if kx > x0 {
            out.push(Primitive::Fill {
                rect: self.local_rect((x0 + kx) / 2.0, 0.0, kx - x0, self.track_h()),
                color: HighlightKind::Pin.color(),
                opacity: 1.0,
            });
        }
        out.push(Primitive::Fill {
            rect: self.local_rect(kx, 0.0, TIMELINE_KNOB, TIMELINE_KNOB),
            color: HighlightKind::Pin.color(),
            opacity: 1.0,
        });
        // Подписи времени: слева — текущая позиция, справа — длительность
        // (выровнены по краям полосы, вертикально — по центру дорожки).
        let (left_text, right_text) = (
            format_time(self.position()),
            format_time(self.duration_secs),
        );
        let (lw, lh) = text_size(&left_text);
        let (rw, rh) = text_size(&right_text);
        out.push(Primitive::Text {
            rect: self.local_rect(
                -self.bounds.w / 2.0 + TIMELINE_MARGIN + lw / 2.0,
                0.0,
                lw,
                lh,
            ),
            text: left_text,
            color: theme::TEXT,
            opacity: self.text_opacity(),
        });
        out.push(Primitive::Text {
            rect: self.local_rect(
                self.bounds.w / 2.0 - TIMELINE_MARGIN - rw / 2.0,
                0.0,
                rw,
                rh,
            ),
            text: right_text,
            color: theme::TEXT,
            opacity: self.text_opacity(),
        });
    }

    fn set_hovered(&mut self, hovered: bool) -> bool {
        if self.hovered == hovered {
            return false;
        }
        self.hovered = hovered;
        true
    }

    fn pointer_event(&mut self, ev: PointerEvent) -> bool {
        match ev {
            // Клик в любую точку полосы ставит позицию и начинает drag —
            // тот же контракт, что у Slider.
            PointerEvent::Down { pos } => {
                self.dragging = true;
                self.set_from_local_x(self.local_x(pos));
                true
            }
            PointerEvent::Move { pos } => {
                if self.dragging {
                    self.set_from_local_x(self.local_x(pos));
                    true
                } else {
                    false
                }
            }
            PointerEvent::Up { .. } => std::mem::replace(&mut self.dragging, false),
            PointerEvent::Wheel { .. } => false,
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// Прямоугольник полосы перемотки внизу прямоугольника стикера (DIP):
/// отступы от краёв, фиксированная высота. `None` — стикер слишком мал:
/// подписи и ручка на такой полосе нечитаемы, рисовать её бессмысленно.
///
/// Поворот стикера переносится на полосу: низ повёрнутого видео — это низ
/// его собственной рамки, а не оси-выровненной оболочки (AABB) — полоса на
/// AABB повёрнутого стикера съехала бы под угол и наполовину вылезла наружу.
pub fn timeline_bounds(sticker_rect: Box2D) -> Option<Box2D> {
    if sticker_rect.w - 2.0 * TIMELINE_MARGIN < TIMELINE_MIN_WIDTH
        || sticker_rect.h < TIMELINE_HEIGHT + 2.0 * TIMELINE_MARGIN
    {
        return None;
    }
    // Центр полосы в локальных координатах стикера: по X — его центр, по
    // Y — низ (нижняя кромка минус отступ минус половина высоты полосы).
    let ly = sticker_rect.h / 2.0 - TIMELINE_MARGIN - TIMELINE_HEIGHT / 2.0;
    let (cx, cy) = to_world(
        sticker_rect.cx,
        sticker_rect.cy,
        sticker_rect.rotation,
        0.0,
        ly,
    );
    Some(Box2D {
        cx,
        cy,
        w: sticker_rect.w - 2.0 * TIMELINE_MARGIN,
        h: TIMELINE_HEIGHT,
        rotation: sticker_rect.rotation,
    })
}

/// Подпись времени `m:ss`, часовые — `h:mm:ss`. Секунды округляются вниз:
/// полоса показывает «сколько уже прошло», а не «до половины следующей
/// секунды» (поведение обычных плееров). Отрицательное время не бывает,
/// но подпись устойчива и к нему.
fn format_time(secs: f64) -> String {
    let total = secs.max(0.0).floor() as u64;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    /// Полоса 200 DIP по центру (100, 100), длительность 120 с, позиция 30.
    fn tl() -> VideoTimeline {
        VideoTimeline::new(
            1,
            Box2D::from_center(100.0, 100.0, 200.0, TIMELINE_HEIGHT),
            120.0,
            30.0,
        )
    }

    /// Допуск для плавающей маппинга x ↔ секунды: линейная интерполяция на
    /// широкой дорожке даёт ошибку порядка 1e-13, но тестам хватает 1e-9.
    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} != {b}");
    }

    fn down_at(w: &mut VideoTimeline, x: f64, y: f64) {
        w.pointer_event(PointerEvent::Down { pos: (x, y) });
    }

    fn move_at(w: &mut VideoTimeline, x: f64, y: f64) {
        w.pointer_event(PointerEvent::Move { pos: (x, y) });
    }

    fn up_at(w: &mut VideoTimeline, x: f64, y: f64) {
        w.pointer_event(PointerEvent::Up { pos: (x, y) });
    }

    fn mid(w: &VideoTimeline) -> f64 {
        w.bounds.cx
    }

    fn left_edge(w: &VideoTimeline) -> f64 {
        w.bounds.cx - w.bounds.w / 2.0
    }

    fn right_edge(w: &VideoTimeline) -> f64 {
        w.bounds.cx + w.bounds.w / 2.0
    }

    #[test]
    fn x_maps_to_seconds_and_back_at_different_widths() {
        // Маппинг линейный и взаимно-обратный на любой ширине: от узкой
        // полосы до широкого монитора.
        for w in [200.0, 640.0, 1200.0] {
            let t = VideoTimeline::new(
                1,
                Box2D::from_center(0.0, 0.0, w, TIMELINE_HEIGHT),
                120.0,
                60.0,
            );
            let (x0, x1) = t.track_range();
            close(t.secs_to_x(0.0), x0);
            close(t.secs_to_x(120.0), x1);
            close(t.x_to_secs(x0), 0.0);
            close(t.x_to_secs(x1), 120.0);
            for secs in [0.0, 1.0, 59.5, 60.0, 119.0, 120.0] {
                close(t.x_to_secs(t.secs_to_x(secs)), secs);
            }
            // Середина дорожки — половина длительности.
            close(t.x_to_secs((x0 + x1) / 2.0), 60.0);
        }
    }

    #[test]
    fn click_at_edges_seeks_zero_and_duration() {
        let mut t = tl();
        let (lx, y) = (left_edge(&t), t.bounds.cy);
        down_at(&mut t, lx, y);
        assert_eq!(t.take_seek(), Some(0.0));
        assert!(t.dragging, "клик начинает drag");
        let (rx, y) = (right_edge(&t), t.bounds.cy);
        up_at(&mut t, rx, y);

        let mut t = tl();
        let (rx, y) = (right_edge(&t), t.bounds.cy);
        down_at(&mut t, rx, y);
        assert_eq!(t.take_seek(), Some(120.0));
        up_at(&mut t, rx, y);
    }

    #[test]
    fn pointer_beyond_edges_clamps() {
        let mut t = tl();
        let (lx, y) = (left_edge(&t) - 50.0, t.bounds.cy);
        down_at(&mut t, lx, y);
        assert_eq!(t.take_seek(), Some(0.0));
        let (rx, y) = (right_edge(&t) + 500.0, t.bounds.cy);
        move_at(&mut t, rx, y);
        assert_eq!(t.take_seek(), Some(120.0));
        up_at(&mut t, rx, y);
        assert!(!t.dragging, "Up снимает захват");
    }

    #[test]
    fn take_seek_delivers_once() {
        let mut t = tl();
        let (mx, y) = (mid(&t), t.bounds.cy);
        down_at(&mut t, mx, y);
        assert_eq!(t.take_seek(), Some(60.0));
        assert_eq!(t.take_seek(), None, "второй опрос — пусто");
        up_at(&mut t, mx, y);
        // Move без захвата перемотку не заявляет и не двигает позицию.
        let (lx, y) = (left_edge(&t), t.bounds.cy);
        move_at(&mut t, lx, y);
        assert_eq!(t.take_seek(), None);
        assert_eq!(t.position(), 30.0);
    }

    #[test]
    fn drag_emits_latest_position_and_release_stops() {
        let mut t = tl();
        let (lx, y) = (left_edge(&t), t.bounds.cy);
        down_at(&mut t, lx, y);
        let (rx, y) = (right_edge(&t), t.bounds.cy);
        move_at(&mut t, rx, y);
        assert_eq!(t.take_seek(), Some(120.0), "последняя позиция пальца");
        up_at(&mut t, rx, y);
        let (lx, y) = (left_edge(&t), t.bounds.cy);
        move_at(&mut t, lx, y);
        assert_eq!(t.take_seek(), None, "после Up Move не перематывает");
    }

    #[test]
    fn set_position_ignored_while_dragging_and_accepted_after() {
        let mut t = tl();
        let (lx, y) = (left_edge(&t), t.bounds.cy);
        down_at(&mut t, lx, y);
        let _ = t.take_seek();
        // Видео продолжает играть и шлёт позиции — ручка должна остаться
        // на пальце, иначе полоса дралась бы с кадром.
        t.set_position(90.0);
        assert_eq!(t.position(), 0.0, "drag ведёт палец, а не внешняя позиция");
        up_at(&mut t, lx, y);
        t.set_position(42.0);
        assert_eq!(
            t.position(),
            42.0,
            "после отпускания внешняя позиция снова принимается"
        );
        // Внешняя позиция клампится к длительности.
        t.set_position(999.0);
        assert_eq!(t.position(), 120.0);
        t.set_position(-5.0);
        assert_eq!(t.position(), 0.0);
    }

    #[test]
    fn zero_duration_never_uses_division() {
        let mut t = VideoTimeline::new(
            1,
            Box2D::from_center(0.0, 0.0, 200.0, TIMELINE_HEIGHT),
            0.0,
            5.0,
        );
        assert_eq!(t.position(), 0.0, "позиция поджата к нулевой длительности");
        let (mx, y) = (mid(&t), 0.0);
        down_at(&mut t, mx, y);
        assert_eq!(
            t.take_seek(),
            Some(0.0),
            "клик по пустой дорожке — позиция 0"
        );
        up_at(&mut t, mx, y);
        // Рисование при нулевой длительности не паникует и даёт ровно
        // подложку, дорожку, ручку и две подписи.
        let mut out = Vec::new();
        t.draw(&mut out);
        assert_eq!(out.len(), 5);
        t.set_duration(60.0);
        t.set_position(30.0);
        assert_eq!(t.position(), 30.0);
    }

    #[test]
    fn format_time_minutes_seconds_and_hours() {
        assert_eq!(format_time(0.0), "0:00");
        assert_eq!(format_time(0.9), "0:00", "секунды округляются вниз");
        assert_eq!(format_time(65.0), "1:05");
        assert_eq!(format_time(3599.0), "59:59");
        assert_eq!(format_time(3600.0), "1:00:00");
        assert_eq!(format_time(3661.0), "1:01:01");
        assert_eq!(format_time(2.0 * 3600.0 + 1.0), "2:00:01");
        assert_eq!(
            format_time(-1.0),
            "0:00",
            "отрицательное время не бывает, но подпись устойчива"
        );
    }

    #[test]
    fn timeline_bounds_sits_at_bottom_of_sticker() {
        // Неповёрнутый стикер: полоса внизу с отступом, шириной минус поля.
        let sticker = Box2D::from_center(300.0, 200.0, 400.0, 300.0);
        let Some(b) = timeline_bounds(sticker) else {
            panic!("полоса должна поместиться");
        };
        assert_eq!(b.w, 400.0 - 2.0 * TIMELINE_MARGIN);
        assert_eq!(b.h, TIMELINE_HEIGHT);
        assert_eq!(b.cx, 300.0);
        assert_eq!(
            b.cy,
            200.0 + 150.0 - TIMELINE_MARGIN - TIMELINE_HEIGHT / 2.0
        );
        assert_eq!(b.rotation, 0.0);
    }

    #[test]
    fn timeline_bounds_rotated_sticker_keeps_bottom_of_its_frame() {
        // Поворот на 90°: «низ» стикера — это боковая грань; центр полосы
        // уезжает из оси-выровненного AABB по оси X (см. доккомент
        // timeline_bounds — полоса следует за рамкой, а не за AABB).
        let sticker = Box2D::from_center(300.0, 200.0, 400.0, 300.0);
        let Some(b) = timeline_bounds(Box2D {
            rotation: std::f64::consts::FRAC_PI_2,
            ..sticker
        }) else {
            panic!("полоса должна поместиться");
        };
        let ly = 150.0 - TIMELINE_MARGIN - TIMELINE_HEIGHT / 2.0;
        assert!(b.cx - (300.0 - ly) < 1e-9, "cx = {}", b.cx);
        assert!(b.cy - 200.0 < 1e-9, "cy = {}", b.cy);
        assert_eq!(b.rotation, std::f64::consts::FRAC_PI_2);
    }

    #[test]
    fn timeline_bounds_rejects_tiny_stickers() {
        let tiny = Box2D::from_center(0.0, 0.0, 100.0, 100.0);
        assert!(timeline_bounds(tiny).is_none(), "слишком узкий стикер");
        let flat = Box2D::from_center(0.0, 0.0, 500.0, 30.0);
        assert!(timeline_bounds(flat).is_none(), "слишком низкий стикер");
        let ok = Box2D::from_center(0.0, 0.0, TIMELINE_MIN_WIDTH + 2.0 * TIMELINE_MARGIN, 200.0);
        assert!(timeline_bounds(ok).is_some(), "минимальный размер проходит");
    }

    #[test]
    fn rotated_timeline_maps_pointer_in_local_frame() {
        // Повёрнутая полоса: локальная ось X смотрит вниз по миру. Клик в
        // «правый край» повёрнутой полосы (вниз от центра по миру) должен
        // дать duration, а не 0 — маппинг идёт в локальных координатах.
        let mut t = VideoTimeline::new(
            1,
            Box2D {
                cx: 100.0,
                cy: 100.0,
                w: 200.0,
                h: TIMELINE_HEIGHT,
                rotation: std::f64::consts::FRAC_PI_2,
            },
            120.0,
            0.0,
        );
        down_at(&mut t, 100.0, 100.0 + 100.0 - TIMELINE_KNOB / 2.0);
        assert_eq!(t.take_seek(), Some(120.0));
    }

    #[test]
    fn hover_changes_visuals_and_returns_redraw_flag() {
        let mut t = tl();
        assert!(!t.hovered);
        let mut out = Vec::new();
        t.draw(&mut out);
        let bg_idle = match &out[0] {
            Primitive::Fill { opacity, .. } => *opacity,
            other => panic!("первый примитив — подложка: {other:?}"),
        };
        assert_eq!(bg_idle, TIMELINE_BG_OPACITY);
        assert!(t.set_hovered(true), "смена наведения требует перерисовки");
        assert!(t.hovered);
        assert!(
            !t.set_hovered(true),
            "повторная установка — без перерисовки"
        );
        let mut out = Vec::new();
        t.draw(&mut out);
        let bg_hover = match &out[0] {
            Primitive::Fill { opacity, .. } => *opacity,
            other => panic!("первый примитив — подложка: {other:?}"),
        };
        assert_eq!(
            bg_hover, TIMELINE_BG_OPACITY_HOVER,
            "при наведении контраст выше"
        );
        let track_h_idle = TIMELINE_TRACK_H;
        let track_h_hover = match &out[1] {
            Primitive::Fill { rect, .. } => rect.h,
            other => panic!("второй примитив — дорожка: {other:?}"),
        };
        assert_eq!(track_h_hover, TIMELINE_TRACK_H_HOVER);
        assert_ne!(track_h_hover, track_h_idle, "при наведении дорожка толще");
        assert!(t.set_hovered(false));
    }
}
