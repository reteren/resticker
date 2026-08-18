//! Начальный размер медиа-стикера при добавлении.
//!
//! Огромное видео (крупнее монитора, на который его добавляют — например
//! портретный ролик 1000x2000 на мониторе 1920x1080) раньше вставлялось
//! в натуральном размере и вылезало за края экрана. Теперь: если размер уже
//! помещается в монитор — оставляем как есть; если нет — сначала вписываем
//! (contain-fit, сохраняя пропорции) в разрешение монитора, затем уменьшаем
//! результат ещё на 20% — небольшой запас, чтобы стикер не упирался ровно
//! в кромку экрана.

/// На сколько уменьшается размер после вписывания в монитор (20%).
const SHRINK_FACTOR: f64 = 0.8;

/// Стартовый размер стикера (DIP) из натурального размера медиа и размера
/// монитора (DIP), на который он добавляется.
pub fn initial_media_size(
    natural_w: f64,
    natural_h: f64,
    monitor_w: f64,
    monitor_h: f64,
) -> (f64, f64) {
    if natural_w <= 0.0 || natural_h <= 0.0 || monitor_w <= 0.0 || monitor_h <= 0.0 {
        return (natural_w, natural_h);
    }
    if natural_w <= monitor_w && natural_h <= monitor_h {
        return (natural_w, natural_h);
    }
    let scale = (monitor_w / natural_w).min(monitor_h / natural_h);
    (
        natural_w * scale * SHRINK_FACTOR,
        natural_h * scale * SHRINK_FACTOR,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_already_is_unchanged() {
        assert_eq!(
            initial_media_size(800.0, 600.0, 1920.0, 1080.0),
            (800.0, 600.0)
        );
    }

    #[test]
    fn exactly_monitor_size_is_unchanged() {
        assert_eq!(
            initial_media_size(1920.0, 1080.0, 1920.0, 1080.0),
            (1920.0, 1080.0)
        );
    }

    #[test]
    fn huge_portrait_video_is_contain_fit_then_shrunk_20_percent() {
        // 1000x2000 на мониторе 1920x1080: связывающее измерение — высота
        // (1080/2000 = 0.54 < 1920/1000 = 1.92), масштаб 0.54 даёт 540x1080,
        // затем ещё 20% -> 432x864. Пропорции (1:2) сохранены на каждом шаге.
        let (w, h) = initial_media_size(1000.0, 2000.0, 1920.0, 1080.0);
        assert!((w - 432.0).abs() < 1e-9, "w = {w}");
        assert!((h - 864.0).abs() < 1e-9, "h = {h}");
    }

    #[test]
    fn huge_landscape_video_is_contain_fit_then_shrunk_20_percent() {
        // 3000x1000 на мониторе 1920x1080: связывающее измерение — ширина
        // (1920/3000 = 0.64 < 1080/1000 = 1.08), масштаб 0.64 даёт 1920x640,
        // затем ещё 20% -> 1536x512.
        let (w, h) = initial_media_size(3000.0, 1000.0, 1920.0, 1080.0);
        assert!((w - 1536.0).abs() < 1e-9, "w = {w}");
        assert!((h - 512.0).abs() < 1e-9, "h = {h}");
    }

    #[test]
    fn overflow_in_only_one_dimension_still_shrinks() {
        // Шире монитора, но не выше: 2500x500 на 1920x1080.
        let (w, h) = initial_media_size(2500.0, 500.0, 1920.0, 1080.0);
        assert!(w <= 1920.0 && h <= 1080.0);
        // Пропорции сохранены.
        assert!((w / h - 2500.0 / 500.0).abs() < 1e-9);
    }

    #[test]
    fn degenerate_zero_size_is_returned_unchanged_no_division_by_zero() {
        assert_eq!(initial_media_size(0.0, 0.0, 1920.0, 1080.0), (0.0, 0.0));
    }
}
