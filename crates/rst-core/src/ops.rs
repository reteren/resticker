//! Операции над моделью: z-order, дублирование, удаление, переключение
//! видимости (SPEC.md, раздел 3; ROADMAP.md M2 «Тулбар»).
//!
//! Все функции чистые: без ввода-вывода, только читают и мутируют переданный
//! `Config` по правилам CONFIG.md («order»: больше — выше, кнопки выше/ниже
//! меняют местами значения с соседом).

use chrono::Utc;
use uuid::Uuid;

use crate::model::Config;

/// Сдвиг копии при дублировании, DIP (по диагонали вправо-вниз), чтобы
/// копия не ложилась строго поверх оригинала.
pub const DUPLICATE_OFFSET: f64 = 16.0;

/// Ошибка операции над конфигом.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OpError {
    /// Стикер с данным id не найден в списке.
    #[error("стикер с id {0} не найден")]
    StickerNotFound(Uuid),
}

/// Индекс стикера по id.
fn index_of(config: &Config, id: Uuid) -> Option<usize> {
    config.stickers.iter().position(|s| s.id == id)
}

/// Нормализация порядка к последовательности 0..N при сохранении
/// относительного порядка при равных значениях (CONFIG.md, «order»).
/// Локальная копия `config::normalize_orders`: порядок стикеров в зей-операциях
/// должен быть строго уникальным, чтобы «сосед» определялся однозначно.
fn normalize_orders(config: &mut Config) {
    let mut order: Vec<usize> = (0..config.stickers.len()).collect();
    order.sort_by_key(|&i| config.stickers[i].order);
    for (new_order, idx) in order.into_iter().enumerate() {
        config.stickers[idx].order = new_order as i64;
    }
}

/// Поднять стикер на передний план (CONFIG.md: больше — выше).
/// Если стикер уже сверху — no-op.
pub fn bring_to_front(config: &mut Config, id: Uuid) -> Result<(), OpError> {
    let i = index_of(config, id).ok_or(OpError::StickerNotFound(id))?;
    normalize_orders(config);
    let max = config.stickers.iter().map(|s| s.order).max().unwrap_or(0);
    if config.stickers[i].order == max {
        return Ok(());
    }
    config.stickers[i].order = max + 1;
    Ok(())
}

/// Опустить стикер на задний план. Если стикер уже снизу — no-op.
pub fn send_to_back(config: &mut Config, id: Uuid) -> Result<(), OpError> {
    let i = index_of(config, id).ok_or(OpError::StickerNotFound(id))?;
    normalize_orders(config);
    let min = config.stickers.iter().map(|s| s.order).min().unwrap_or(0);
    if config.stickers[i].order == min {
        return Ok(());
    }
    config.stickers[i].order = min - 1;
    Ok(())
}

/// Поднять стикер на одну позицию (обмен значениями с соседом сверху,
/// CONFIG.md). Если стикер уже наверху — no-op.
pub fn step_up(config: &mut Config, id: Uuid) -> Result<(), OpError> {
    let i = index_of(config, id).ok_or(OpError::StickerNotFound(id))?;
    normalize_orders(config);
    let n = config.stickers.len();
    if n < 2 {
        return Ok(());
    }
    let me = config.stickers[i].order as usize;
    if me == n - 1 {
        return Ok(());
    }
    let neighbor = config
        .stickers
        .iter()
        .position(|s| s.order == (me + 1) as i64)
        .expect("после нормализации сосед сверху существует");
    config.stickers[i].order = (me + 1) as i64;
    config.stickers[neighbor].order = me as i64;
    Ok(())
}

/// Опустить стикер на одну позицию (обмен значениями с соседом снизу,
/// CONFIG.md). Если стикер уже внизу — no-op.
pub fn step_down(config: &mut Config, id: Uuid) -> Result<(), OpError> {
    let i = index_of(config, id).ok_or(OpError::StickerNotFound(id))?;
    normalize_orders(config);
    let n = config.stickers.len();
    if n < 2 {
        return Ok(());
    }
    let me = config.stickers[i].order as usize;
    if me == 0 {
        return Ok(());
    }
    let neighbor = config
        .stickers
        .iter()
        .position(|s| s.order == (me - 1) as i64)
        .expect("после нормализации сосед снизу существует");
    config.stickers[i].order = (me - 1) as i64;
    config.stickers[neighbor].order = me as i64;
    Ok(())
}

/// Дублировать стикер: копия со сдвигом позиции и новым id, помещается на
/// передний план (CONFIG.md: новый стикер получает `max(order) + 1`).
/// Возвращает id копии.
pub fn duplicate(config: &mut Config, id: Uuid) -> Result<Uuid, OpError> {
    let i = index_of(config, id).ok_or(OpError::StickerNotFound(id))?;
    let mut clone = config.stickers[i].clone();
    clone.id = Uuid::new_v4();
    clone.created_at = Utc::now();
    clone.placement.cx += DUPLICATE_OFFSET;
    clone.placement.cy += DUPLICATE_OFFSET;
    let max = config.stickers.iter().map(|s| s.order).max().unwrap_or(0);
    clone.order = max + 1;
    let new_id = clone.id;
    config.stickers.push(clone);
    Ok(new_id)
}

/// Удалить стикер из списка.
pub fn delete(config: &mut Config, id: Uuid) -> Result<(), OpError> {
    let i = index_of(config, id).ok_or(OpError::StickerNotFound(id))?;
    config.stickers.remove(i);
    Ok(())
}

/// Переключить видимость стикера (кнопка «глаз», SPEC.md раздел 3.2).
pub fn toggle_visibility(config: &mut Config, id: Uuid) -> Result<(), OpError> {
    let i = index_of(config, id).ok_or(OpError::StickerNotFound(id))?;
    config.stickers[i].visible = !config.stickers[i].visible;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MediaType, Sticker, StickerSource};

    fn sticker(order: i64) -> Sticker {
        Sticker {
            id: Uuid::new_v4(),
            order,
            ..Default::default()
        }
    }

    fn cfg(orders: &[i64]) -> Config {
        Config {
            stickers: orders.iter().map(|&o| sticker(o)).collect(),
            ..Default::default()
        }
    }

    fn ids(config: &Config) -> Vec<Uuid> {
        config.stickers.iter().map(|s| s.id).collect()
    }

    fn get(config: &Config, id: Uuid) -> &Sticker {
        config
            .stickers
            .iter()
            .find(|s| s.id == id)
            .expect("стикер существует")
    }

    fn top_id(config: &Config) -> Uuid {
        config
            .stickers
            .iter()
            .max_by_key(|s| s.order)
            .expect("непустой список")
            .id
    }

    fn bottom_id(config: &Config) -> Uuid {
        config
            .stickers
            .iter()
            .min_by_key(|s| s.order)
            .expect("непустой список")
            .id
    }

    fn assert_stickers_equal(a: &Config, b: &Config) {
        assert_eq!(a.stickers, b.stickers);
    }

    #[test]
    fn bring_to_front_moves_bottom_sticker_to_top() {
        let mut c = cfg(&[0, 1, 2]);
        let bottom = bottom_id(&c);
        bring_to_front(&mut c, bottom).unwrap();
        assert_eq!(top_id(&c), bottom);
        assert_eq!(get(&c, bottom).order, 3);
    }

    #[test]
    fn bring_to_front_already_on_top_is_noop() {
        let mut c = cfg(&[0, 1, 2]);
        let top = top_id(&c);
        let before = c.clone();
        bring_to_front(&mut c, top).unwrap();
        assert_stickers_equal(&c, &before);
    }

    #[test]
    fn bring_to_front_single_sticker_is_noop() {
        let mut c = cfg(&[0]);
        let id = top_id(&c);
        let before = c.clone();
        bring_to_front(&mut c, id).unwrap();
        assert_stickers_equal(&c, &before);
    }

    #[test]
    fn send_to_back_moves_top_sticker_to_bottom() {
        let mut c = cfg(&[0, 1, 2]);
        let top = top_id(&c);
        send_to_back(&mut c, top).unwrap();
        assert_eq!(bottom_id(&c), top);
        assert_eq!(get(&c, top).order, -1);
    }

    #[test]
    fn send_to_back_already_on_bottom_is_noop() {
        let mut c = cfg(&[0, 1, 2]);
        let bottom = bottom_id(&c);
        let before = c.clone();
        send_to_back(&mut c, bottom).unwrap();
        assert_stickers_equal(&c, &before);
    }

    #[test]
    fn send_to_back_single_sticker_is_noop() {
        let mut c = cfg(&[0]);
        let id = top_id(&c);
        let before = c.clone();
        send_to_back(&mut c, id).unwrap();
        assert_stickers_equal(&c, &before);
    }

    #[test]
    fn step_up_moves_sticker_one_position_up() {
        let mut c = cfg(&[0, 1, 2]);
        let ids = ids(&c);
        step_up(&mut c, ids[1]).unwrap();
        assert_eq!(get(&c, ids[1]).order, 2);
        assert_eq!(get(&c, ids[2]).order, 1);
        assert_eq!(get(&c, ids[0]).order, 0);
    }

    #[test]
    fn step_up_bottom_sticker_moves_to_middle() {
        let mut c = cfg(&[0, 1, 2]);
        let ids = ids(&c);
        step_up(&mut c, ids[0]).unwrap();
        assert_eq!(get(&c, ids[0]).order, 1);
        assert_eq!(get(&c, ids[1]).order, 0);
    }

    #[test]
    fn step_up_already_on_top_is_noop() {
        let mut c = cfg(&[0, 1, 2]);
        let top = top_id(&c);
        let before = c.clone();
        step_up(&mut c, top).unwrap();
        assert_stickers_equal(&c, &before);
    }

    #[test]
    fn step_up_single_sticker_is_noop() {
        let mut c = cfg(&[0]);
        let id = top_id(&c);
        let before = c.clone();
        step_up(&mut c, id).unwrap();
        assert_stickers_equal(&c, &before);
    }

    #[test]
    fn step_down_moves_sticker_one_position_down() {
        let mut c = cfg(&[0, 1, 2]);
        let ids = ids(&c);
        step_down(&mut c, ids[1]).unwrap();
        assert_eq!(get(&c, ids[1]).order, 0);
        assert_eq!(get(&c, ids[0]).order, 1);
        assert_eq!(get(&c, ids[2]).order, 2);
    }

    #[test]
    fn step_down_top_sticker_moves_to_middle() {
        let mut c = cfg(&[0, 1, 2]);
        let ids = ids(&c);
        step_down(&mut c, ids[2]).unwrap();
        assert_eq!(get(&c, ids[2]).order, 1);
        assert_eq!(get(&c, ids[1]).order, 2);
    }

    #[test]
    fn step_down_already_on_bottom_is_noop() {
        let mut c = cfg(&[0, 1, 2]);
        let bottom = bottom_id(&c);
        let before = c.clone();
        step_down(&mut c, bottom).unwrap();
        assert_stickers_equal(&c, &before);
    }

    #[test]
    fn step_down_single_sticker_is_noop() {
        let mut c = cfg(&[0]);
        let id = top_id(&c);
        let before = c.clone();
        step_down(&mut c, id).unwrap();
        assert_stickers_equal(&c, &before);
    }

    #[test]
    fn z_ops_handle_tied_orders() {
        let mut c = Config {
            stickers: vec![sticker(5), sticker(5), sticker(5)],
            ..Default::default()
        };
        let sticker_ids = ids(&c);
        bring_to_front(&mut c, sticker_ids[0]).unwrap();
        assert_eq!(top_id(&c), sticker_ids[0]);
        step_up(&mut c, sticker_ids[0]).unwrap();
        assert_eq!(top_id(&c), sticker_ids[0], "уже сверху — no-op");

        let mut c = Config {
            stickers: vec![sticker(5), sticker(5), sticker(5)],
            ..Default::default()
        };
        let sticker_ids = ids(&c);
        step_up(&mut c, sticker_ids[1]).unwrap();
        assert_eq!(get(&c, sticker_ids[1]).order, 2);
        assert_eq!(get(&c, sticker_ids[2]).order, 1);
    }

    #[test]
    fn duplicate_creates_offset_clone_on_top() {
        let mut c = cfg(&[0, 1, 2]);
        let src = top_id(&c);
        let original = get(&c, src).clone();
        let new_id = duplicate(&mut c, src).unwrap();
        assert_ne!(new_id, src);
        let clone = get(&c, new_id);
        assert_eq!(clone.placement.cx, original.placement.cx + DUPLICATE_OFFSET);
        assert_eq!(clone.placement.cy, original.placement.cy + DUPLICATE_OFFSET);
        assert_eq!(clone.order, 3);
        assert_eq!(top_id(&c), new_id);
        assert_eq!(clone.visible, original.visible);
        assert_eq!(clone.enabled, original.enabled);
        assert_eq!(c.stickers.len(), 4);
    }

    #[test]
    fn duplicate_preserves_rest_of_fields() {
        let mut original = sticker(0);
        original.source = StickerSource::File {
            path: "C:\\pics\\cat.png".into(),
            media_type: MediaType::Image,
        };
        original.transform.opacity = 0.5;
        original.transform.flip_h = true;
        original.playback.volume = 0.25;
        original.visible = false;
        let mut c = Config {
            stickers: vec![original.clone()],
            ..Default::default()
        };
        let src = original.id;
        let new_id = duplicate(&mut c, src).unwrap();
        let clone = get(&c, new_id);
        assert_eq!(clone.source, original.source);
        assert_eq!(clone.transform, original.transform);
        assert_eq!(clone.playback, original.playback);
        assert_eq!(clone.visible, original.visible);
        assert_ne!(clone.id, original.id);
    }

    #[test]
    fn delete_removes_sticker() {
        let mut c = cfg(&[0, 1, 2]);
        let mid = ids(&c)[1];
        delete(&mut c, mid).unwrap();
        assert_eq!(c.stickers.len(), 2);
        assert!(index_of(&c, mid).is_none());
    }

    #[test]
    fn delete_last_sticker_leaves_empty() {
        let mut c = cfg(&[7]);
        let id = top_id(&c);
        delete(&mut c, id).unwrap();
        assert!(c.stickers.is_empty());
    }

    #[test]
    fn delete_unknown_id_errors() {
        let mut c = cfg(&[0]);
        let unknown = Uuid::new_v4();
        assert_eq!(
            delete(&mut c, unknown),
            Err(OpError::StickerNotFound(unknown))
        );
    }

    #[test]
    fn toggle_visibility_flips_flag() {
        let mut c = cfg(&[0]);
        let id = top_id(&c);
        assert!(get(&c, id).visible);
        toggle_visibility(&mut c, id).unwrap();
        assert!(!get(&c, id).visible);
        toggle_visibility(&mut c, id).unwrap();
        assert!(get(&c, id).visible);
    }

    #[test]
    fn toggle_visibility_unknown_id_errors() {
        let mut c = cfg(&[0]);
        let unknown = Uuid::new_v4();
        assert_eq!(
            toggle_visibility(&mut c, unknown),
            Err(OpError::StickerNotFound(unknown))
        );
    }

    #[test]
    fn ops_on_empty_list_error() {
        let mut c = cfg(&[]);
        let id = Uuid::new_v4();
        let not_found = Err(OpError::StickerNotFound(id));
        assert_eq!(bring_to_front(&mut c, id), not_found);
        assert_eq!(send_to_back(&mut c, id), not_found);
        assert_eq!(step_up(&mut c, id), not_found);
        assert_eq!(step_down(&mut c, id), not_found);
        assert_eq!(delete(&mut c, id), not_found);
        assert_eq!(toggle_visibility(&mut c, id), not_found);
        assert_eq!(duplicate(&mut c, id), Err(OpError::StickerNotFound(id)));
        assert!(c.stickers.is_empty());
    }
}
