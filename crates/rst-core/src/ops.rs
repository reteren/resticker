//! Операции над моделью: z-order, дублирование, удаление (и подтверждение
//! удаления), переключение видимости (SPEC.md, раздел 3; ROADMAP.md M2 «Тулбар»).
//!
//! Все функции чистые: без ввода-вывода, только читают и мутируют переданный
//! `Config` по правилам CONFIG.md («order»: больше — выше, кнопки выше/ниже
//! меняют местами значения с соседом).

use chrono::Utc;
use uuid::Uuid;

use crate::model::{Config, StickerSource};

/// Сдвиг копии при дублировании, DIP (по диагонали вправо-вниз), чтобы
/// копия не ложилась строго поверх оригинала.
pub const DUPLICATE_OFFSET: f64 = 16.0;

/// Ошибка операции над конфигом.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OpError {
    /// Стикер с данным id не найден в списке.
    #[error("no sticker with id {0}")]
    StickerNotFound(Uuid),
    /// `relink_file` вызван для стикера, чей источник не `File` (`Pasted`
    /// не поддерживает переуказание пути).
    #[error("sticker {0} has no file to relink")]
    NotFileBacked(Uuid),
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

/// Поднять блок выделения на одну позицию по z-order как единое целое
/// (docs/M2_MULTISELECT_TOOLBAR_NOTES.md, §3.3). По-элементный прогон
/// [`step_up`] по выделению некорректен: соседние выбранные обменялись бы
/// заказами между собой без видимого эффекта. Здесь весь диапазон
/// `min(order выбранных)..=max(order выбранных)` сдвигается вверх на одну
/// позицию; элемент, стоявший сразу над блоком, переезжает под него.
/// No-op, если верхний выбранный уже на самом верху. Для одиночного id
/// совпадает с [`step_up`].
///
/// Тулбаром не вызывается — см. заметку у [`step_down_many`].
pub fn step_up_many(config: &mut Config, ids: &[Uuid]) -> Result<(), OpError> {
    if ids.is_empty() {
        return Ok(());
    }
    let (min, max) = selected_order_range(config, ids)?;
    if max == config.stickers.len() - 1 {
        return Ok(());
    }
    shift_block(config, min, max, 1);
    Ok(())
}

/// Опустить блок выделения на одну позицию по z-order как единое целое
/// (docs/M2_MULTISELECT_TOOLBAR_NOTES.md, §3.3) — зеркально [`step_up_many`]:
///
/// ТУЛБАРОМ НЕ ВЫЗЫВАЕТСЯ с 2026-09-06: пользователь попросил другое
/// поведение — «последний выделенный летит выше (ниже) всех, остальные по
/// очереди» (`overlay_manager::reorder_selection` через
/// [`bring_to_front`]/[`send_to_back`]). Функция и её тесты оставлены: это
/// корректная операция «сдвинуть блок на уровень», и она понадобится, если
/// поведение вернут.
/// весь диапазон `min..=max` сдвигается вниз, элемент сразу под блоком
/// переезжает над ним. No-op, если нижний выбранный уже внизу. Для
/// одиночного id совпадает с [`step_down`].
pub fn step_down_many(config: &mut Config, ids: &[Uuid]) -> Result<(), OpError> {
    if ids.is_empty() {
        return Ok(());
    }
    let (min, max) = selected_order_range(config, ids)?;
    if min == 0 {
        return Ok(());
    }
    shift_block(config, min, max, -1);
    Ok(())
}

/// Заказы нижней и верхней границы блока выделения: `(min, max)` — min и max
/// `order` среди `ids` после нормализации (заказы 0..n-1, относительный
/// порядок сохранён). Все id валидируются до мутации: при неизвестном id —
/// [`OpError::StickerNotFound`], конфиг не тронут.
fn selected_order_range(config: &mut Config, ids: &[Uuid]) -> Result<(usize, usize), OpError> {
    for id in ids {
        index_of(config, *id).ok_or(OpError::StickerNotFound(*id))?;
    }
    normalize_orders(config);
    let mut min = usize::MAX;
    let mut max = 0;
    for id in ids {
        let order = config
            .stickers
            .iter()
            .find(|s| s.id == *id)
            .map(|s| s.order as usize)
            .expect("валидность всех id проверена выше");
        min = min.min(order);
        max = max.max(order);
    }
    Ok((min, max))
}

/// Сдвинуть блок заказов `[min..=max]` на `delta` (±1) как единое целое:
/// каждый стикер блока получает `order + delta`, элемент сразу за границей
/// блока занимает освободившуюся позицию. Заказы предполагаются
/// нормализованными (`0..n-1`); результат остаётся нормализованным, поэтому
/// повторная нормализация не нужна.
fn shift_block(config: &mut Config, min: usize, max: usize, delta: i64) {
    let neighbour_order = if delta > 0 {
        max as i64 + 1
    } else {
        min as i64 - 1
    };
    let neighbour = config
        .stickers
        .iter()
        .position(|s| s.order == neighbour_order)
        .expect("после нормализации сосед блока существует");
    let block = min as i64..=max as i64;
    for s in &mut config.stickers {
        if block.contains(&s.order) {
            s.order += delta;
        }
    }
    config.stickers[neighbour].order = if delta > 0 { min as i64 } else { max as i64 };
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

/// Показывать ли диалог подтверждения удаления (SPEC.md, «Больше не
/// спрашивать»): да, пока пользователь не отключил подтверждение.
pub fn should_confirm_delete(config: &Config) -> bool {
    !config.settings.skip_delete_confirmation
}

/// Отключить подтверждение удаления («Больше не спрашивать», SPEC.md).
/// Сброс — прямым присваиванием `settings.skip_delete_confirmation = false`
/// в настройках.
pub fn suppress_delete_confirmation(config: &mut Config) {
    config.settings.skip_delete_confirmation = true;
}

/// Переключить видимость стикера (кнопка «глаз», SPEC.md раздел 3.2).
pub fn toggle_visibility(config: &mut Config, id: Uuid) -> Result<(), OpError> {
    let i = index_of(config, id).ok_or(OpError::StickerNotFound(id))?;
    config.stickers[i].visible = !config.stickers[i].visible;
    Ok(())
}

/// Выставить видимость группе стикеров ровно в `visible` (не тоггл) — батч
/// для кнопки «глаз» при мультивыделении (docs/M2_MULTISELECT_TOOLBAR_NOTES.md,
/// §3.2). Целевое состояние считает координатор
/// (`target = !(все выбранные видимы)`) — здесь только применение решения
/// батчем. Несуществующие id молча пропускаются.
pub fn set_visible_many(config: &mut Config, ids: &[Uuid], visible: bool) {
    for id in ids {
        if let Some(sticker) = config.stickers.iter_mut().find(|s| s.id == *id) {
            sticker.visible = visible;
        }
    }
}

/// «Сбросить позицию» (окно настроек, вкладка «Стикеры», SPEC.md раздел
/// 10): вернуть центр стикера в переданную точку (обычно центр его
/// монитора — координатор решает это по геометрии, здесь только
/// присваивание), размер/поворот/прозрачность не трогаются.
pub fn reset_position(
    config: &mut Config,
    id: Uuid,
    center_x: f64,
    center_y: f64,
) -> Result<(), OpError> {
    let i = index_of(config, id).ok_or(OpError::StickerNotFound(id))?;
    config.stickers[i].placement.cx = center_x;
    config.stickers[i].placement.cy = center_y;
    Ok(())
}

/// «Сбросить размер и поворот» (окно настроек, вкладка «Стикеры», SPEC.md
/// раздел 10): размер стикера возвращается к натуральному размеру
/// декодированного изображения/кадра (координатор передаёт его — здесь
/// нет доступа к GPU-текстуре/декодеру), поворот и отражения — к дефолту.
/// Позиция (`cx`/`cy`) и прозрачность НЕ трогаются — это отдельное
/// действие «сбросить позицию» и часть внешнего вида, не размера/поворота.
pub fn reset_transform_and_size(
    config: &mut Config,
    id: Uuid,
    natural_w: f64,
    natural_h: f64,
) -> Result<(), OpError> {
    let i = index_of(config, id).ok_or(OpError::StickerNotFound(id))?;
    let sticker = &mut config.stickers[i];
    sticker.placement.w = natural_w;
    sticker.placement.h = natural_h;
    sticker.transform.rotation = crate::model::Transform::default().rotation;
    sticker.transform.flip_h = crate::model::Transform::default().flip_h;
    sticker.transform.flip_v = crate::model::Transform::default().flip_v;
    Ok(())
}

/// «Переуказать файл» (окно настроек, вкладка «Стикеры», SPEC.md раздел
/// 10): заменить путь и тип медиа источника у существующего стикера, не
/// создавая новый (сохраняет id/placement/transform/playback) — размеры
/// координатор пересчитывает сам после подмены, перезагрузив спрайт той же
/// логикой, что обычное добавление. `media_type` координатор определяет по
/// новому файлу заранее (та же проверка расширения, что в `add_sticker`) и
/// передаёт готовым — здесь нет доступа к `VIDEO_EXTENSIONS`/декодеру.
/// `Pasted`-источник этим действием не переуказывается (SPEC не описывает
/// такой сценарий для него).
pub fn relink_file(
    config: &mut Config,
    id: Uuid,
    new_path: std::path::PathBuf,
    media_type: crate::model::MediaType,
) -> Result<(), OpError> {
    let i = index_of(config, id).ok_or(OpError::StickerNotFound(id))?;
    match &mut config.stickers[i].source {
        StickerSource::File {
            path,
            media_type: mt,
        } => {
            *path = new_path;
            *mt = media_type;
            Ok(())
        }
        // Кусок чужого окна файлом не подпирается: «переуказать» его значит
        // выделить заново над другим окном, а не подменить путь.
        StickerSource::Pasted { .. } | StickerSource::WindowCrop { .. } => {
            Err(OpError::NotFileBacked(id))
        }
    }
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
    fn step_up_many_moves_adjacent_block_up_as_a_whole() {
        let mut c = cfg(&[0, 1, 2, 3]);
        let ids = ids(&c);
        // Выделены соседние B (1) и C (2).
        step_up_many(&mut c, &[ids[1], ids[2]]).unwrap();
        // Блок [1..=2] сдвинут вверх: B→2, C→3; D (стоял над блоком) → 1.
        assert_eq!(get(&c, ids[1]).order, 2);
        assert_eq!(get(&c, ids[2]).order, 3);
        assert_eq!(get(&c, ids[3]).order, 1);
        assert_eq!(get(&c, ids[0]).order, 0);
    }

    #[test]
    fn step_up_many_moves_disconnected_block_up_as_a_whole() {
        let mut c = cfg(&[0, 1, 2, 3, 4]);
        let ids = ids(&c);
        // Выделены A (0) и D (3); между ними невыбранные B, C. Порядок id
        // в списке не должен влиять на результат.
        step_up_many(&mut c, &[ids[3], ids[0]]).unwrap();
        // Блок [0..=3] сдвинут вверх: A→1, B→2, C→3, D→4; E (над блоком) → 0.
        assert_eq!(get(&c, ids[0]).order, 1);
        assert_eq!(get(&c, ids[1]).order, 2);
        assert_eq!(get(&c, ids[2]).order, 3);
        assert_eq!(get(&c, ids[3]).order, 4);
        assert_eq!(get(&c, ids[4]).order, 0);
    }

    #[test]
    fn step_up_many_noop_when_top_selected_on_top() {
        let mut c = cfg(&[0, 1, 2]);
        let ids = ids(&c);
        // Выделены A и C — верхний выбранный уже наверху.
        let before = c.clone();
        step_up_many(&mut c, &[ids[0], ids[2]]).unwrap();
        assert_stickers_equal(&c, &before);
    }

    #[test]
    fn step_up_many_single_id_matches_step_up() {
        for orders in [vec![0, 1, 2], vec![0, 1, 2, 3, 4]] {
            let mut a = cfg(&orders);
            let ids_a = ids(&a);
            step_up(&mut a, ids_a[1]).unwrap();

            let mut b = cfg(&orders);
            let ids_b = ids(&b);
            step_up_many(&mut b, &[ids_b[1]]).unwrap();

            // id в a и b разные — сравниваем только порядок по позициям.
            let a_orders: Vec<i64> = a.stickers.iter().map(|s| s.order).collect();
            let b_orders: Vec<i64> = b.stickers.iter().map(|s| s.order).collect();
            assert_eq!(a_orders, b_orders);
        }
    }

    #[test]
    fn step_down_many_moves_adjacent_block_down_as_a_whole() {
        let mut c = cfg(&[0, 1, 2, 3]);
        let ids = ids(&c);
        // Выделены соседние B (1) и C (2).
        step_down_many(&mut c, &[ids[1], ids[2]]).unwrap();
        // Блок [1..=2] сдвинут вниз: B→0, C→1; A (под блоком) → 2.
        assert_eq!(get(&c, ids[1]).order, 0);
        assert_eq!(get(&c, ids[2]).order, 1);
        assert_eq!(get(&c, ids[0]).order, 2);
        assert_eq!(get(&c, ids[3]).order, 3);
    }

    #[test]
    fn step_down_many_moves_disconnected_block_down_as_a_whole() {
        let mut c = cfg(&[0, 1, 2, 3, 4]);
        let ids = ids(&c);
        // Выделены B (1) и E (4); между ними невыбранные C, D.
        step_down_many(&mut c, &[ids[4], ids[1]]).unwrap();
        // Блок [1..=4] сдвинут вниз: B→0, C→1, D→2, E→3; A (под блоком) → 4.
        assert_eq!(get(&c, ids[0]).order, 4);
        assert_eq!(get(&c, ids[1]).order, 0);
        assert_eq!(get(&c, ids[2]).order, 1);
        assert_eq!(get(&c, ids[3]).order, 2);
        assert_eq!(get(&c, ids[4]).order, 3);
    }

    #[test]
    fn step_down_many_noop_when_bottom_selected_on_bottom() {
        let mut c = cfg(&[0, 1, 2]);
        let ids = ids(&c);
        // Выделены A и C — нижний выбранный уже внизу.
        let before = c.clone();
        step_down_many(&mut c, &[ids[0], ids[2]]).unwrap();
        assert_stickers_equal(&c, &before);
    }

    #[test]
    fn step_down_many_single_id_matches_step_down() {
        for orders in [vec![0, 1, 2], vec![0, 1, 2, 3, 4]] {
            let mut a = cfg(&orders);
            let ids_a = ids(&a);
            step_down(&mut a, ids_a[1]).unwrap();

            let mut b = cfg(&orders);
            let ids_b = ids(&b);
            step_down_many(&mut b, &[ids_b[1]]).unwrap();

            // id в a и b разные — сравниваем только порядок по позициям.
            let a_orders: Vec<i64> = a.stickers.iter().map(|s| s.order).collect();
            let b_orders: Vec<i64> = b.stickers.iter().map(|s| s.order).collect();
            assert_eq!(a_orders, b_orders);
        }
    }

    #[test]
    fn step_many_empty_selection_is_noop() {
        let mut c = cfg(&[0, 1, 2]);
        let before = c.clone();
        step_up_many(&mut c, &[]).unwrap();
        step_down_many(&mut c, &[]).unwrap();
        assert_stickers_equal(&c, &before);
    }

    #[test]
    fn step_many_unknown_id_errors_without_mutation() {
        let mut c = cfg(&[0, 1]);
        let ids = ids(&c);
        let unknown = Uuid::new_v4();
        assert_eq!(
            step_up_many(&mut c, &[ids[0], unknown]),
            Err(OpError::StickerNotFound(unknown))
        );
        assert_eq!(get(&c, ids[0]).order, 0);
        assert_eq!(get(&c, ids[1]).order, 1);
    }

    #[test]
    fn step_many_handles_tied_orders() {
        let mut c = Config {
            stickers: vec![sticker(5), sticker(5), sticker(5), sticker(5)],
            ..Default::default()
        };
        let sticker_ids = ids(&c);
        // После нормализации — [0, 1, 2, 3]; выделены второй и третий.
        step_up_many(&mut c, &[sticker_ids[1], sticker_ids[2]]).unwrap();
        assert_eq!(get(&c, sticker_ids[1]).order, 2);
        assert_eq!(get(&c, sticker_ids[2]).order, 3);
        assert_eq!(get(&c, sticker_ids[3]).order, 1);
        assert_eq!(get(&c, sticker_ids[0]).order, 0);
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
    fn delete_confirmation_on_by_default() {
        let c = Config::default();
        assert!(should_confirm_delete(&c));
        assert!(!c.settings.skip_delete_confirmation);
    }

    #[test]
    fn suppress_delete_confirmation_flips_flag() {
        let mut c = Config::default();
        suppress_delete_confirmation(&mut c);
        assert!(!should_confirm_delete(&c));
        assert!(c.settings.skip_delete_confirmation);
    }

    #[test]
    fn reset_restores_confirmation() {
        let mut c = Config::default();
        suppress_delete_confirmation(&mut c);
        c.settings.skip_delete_confirmation = false;
        assert!(should_confirm_delete(&c));
    }

    #[test]
    fn suppress_is_idempotent() {
        let mut c = Config::default();
        suppress_delete_confirmation(&mut c);
        suppress_delete_confirmation(&mut c);
        assert!(!should_confirm_delete(&c));
    }

    #[test]
    fn should_confirm_delete_does_not_mutate() {
        let c = Config::default();
        let before = c.clone();
        let _ = should_confirm_delete(&c);
        assert_eq!(c, before);
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
    fn set_visible_many_sets_exact_value_on_mixed_group() {
        let mut c = cfg(&[0, 1, 2, 3]);
        let ids = ids(&c);
        // Смешанная группа: два видимы, два скрыты.
        toggle_visibility(&mut c, ids[0]).unwrap();
        toggle_visibility(&mut c, ids[2]).unwrap();
        assert!(!get(&c, ids[0]).visible && !get(&c, ids[2]).visible);
        assert!(get(&c, ids[1]).visible && get(&c, ids[3]).visible);

        set_visible_many(&mut c, &ids, true);
        assert!(ids.iter().all(|id| get(&c, *id).visible));

        set_visible_many(&mut c, &ids, false);
        assert!(ids.iter().all(|id| !get(&c, *id).visible));
    }

    #[test]
    fn set_visible_many_does_not_touch_unlisted_stickers() {
        let mut c = cfg(&[0, 1, 2]);
        let ids = ids(&c);
        toggle_visibility(&mut c, ids[2]).unwrap();
        set_visible_many(&mut c, &ids[0..1], false);
        assert!(!get(&c, ids[0]).visible);
        assert!(get(&c, ids[1]).visible, "не в списке — не тронут");
        assert!(!get(&c, ids[2]).visible, "не в списке — не тронут");
    }

    #[test]
    fn set_visible_many_ignores_unknown_ids() {
        let mut c = cfg(&[0, 1]);
        let ids = ids(&c);
        let unknown = Uuid::new_v4();
        set_visible_many(&mut c, &[ids[0], unknown], false);
        assert!(!get(&c, ids[0]).visible);
        assert!(get(&c, ids[1]).visible);
    }

    #[test]
    fn set_visible_many_on_empty_selection_is_noop() {
        let mut c = cfg(&[0, 1]);
        let before = c.clone();
        set_visible_many(&mut c, &[], false);
        assert_stickers_equal(&c, &before);
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

    #[test]
    fn reset_position_moves_center_keeps_size_and_rotation() {
        let mut c = cfg(&[0]);
        let ids = ids(&c);
        {
            let s = c.stickers.iter_mut().find(|s| s.id == ids[0]).unwrap();
            s.placement.cx = 500.0;
            s.placement.cy = 500.0;
            s.placement.w = 200.0;
            s.placement.h = 150.0;
            s.transform.rotation = 45.0;
        }
        reset_position(&mut c, ids[0], 100.0, 80.0).unwrap();
        let s = get(&c, ids[0]);
        assert_eq!((s.placement.cx, s.placement.cy), (100.0, 80.0));
        assert_eq!((s.placement.w, s.placement.h), (200.0, 150.0));
        assert_eq!(s.transform.rotation, 45.0);
    }

    #[test]
    fn reset_position_unknown_id_errors() {
        let mut c = cfg(&[0]);
        let id = Uuid::new_v4();
        assert_eq!(
            reset_position(&mut c, id, 0.0, 0.0),
            Err(OpError::StickerNotFound(id))
        );
    }

    #[test]
    fn reset_transform_and_size_restores_natural_size_and_default_rotation_flips() {
        let mut c = cfg(&[0]);
        let ids = ids(&c);
        {
            let s = c.stickers.iter_mut().find(|s| s.id == ids[0]).unwrap();
            s.placement.cx = 500.0;
            s.placement.cy = 500.0;
            s.placement.w = 999.0;
            s.placement.h = 999.0;
            s.transform.rotation = 45.0;
            s.transform.flip_h = true;
            s.transform.flip_v = true;
            s.transform.opacity = 0.3;
        }
        reset_transform_and_size(&mut c, ids[0], 64.0, 48.0).unwrap();
        let s = get(&c, ids[0]);
        assert_eq!((s.placement.w, s.placement.h), (64.0, 48.0));
        assert_eq!(
            (s.placement.cx, s.placement.cy),
            (500.0, 500.0),
            "позиция не трогается"
        );
        assert_eq!(s.transform.rotation, 0.0);
        assert!(!s.transform.flip_h);
        assert!(!s.transform.flip_v);
        assert_eq!(
            s.transform.opacity, 0.3,
            "прозрачность — не часть этого сброса"
        );
    }

    #[test]
    fn reset_transform_and_size_unknown_id_errors() {
        let mut c = cfg(&[0]);
        let id = Uuid::new_v4();
        assert_eq!(
            reset_transform_and_size(&mut c, id, 1.0, 1.0),
            Err(OpError::StickerNotFound(id))
        );
    }

    #[test]
    fn relink_file_replaces_path_and_media_type_for_file_source() {
        let mut c = cfg(&[0]);
        let ids = ids(&c);
        let new_path = std::path::PathBuf::from("W:/videos/clip.mp4");
        relink_file(&mut c, ids[0], new_path.clone(), MediaType::Video).unwrap();
        match &get(&c, ids[0]).source {
            StickerSource::File { path, media_type } => {
                assert_eq!(path, &new_path);
                assert_eq!(*media_type, MediaType::Video);
            }
            other => panic!("ожидался StickerSource::File, получено {other:?}"),
        }
    }

    #[test]
    fn relink_file_rejects_pasted_source() {
        let mut c = cfg(&[0]);
        let ids = ids(&c);
        {
            let s = c.stickers.iter_mut().find(|s| s.id == ids[0]).unwrap();
            s.source = StickerSource::Pasted {
                path: std::path::PathBuf::from("W:/pasted/x.png"),
            };
        }
        assert_eq!(
            relink_file(
                &mut c,
                ids[0],
                std::path::PathBuf::from("W:/x.mp4"),
                MediaType::Video
            ),
            Err(OpError::NotFileBacked(ids[0]))
        );
    }

    #[test]
    fn relink_file_unknown_id_errors() {
        let mut c = cfg(&[0]);
        let id = Uuid::new_v4();
        assert_eq!(
            relink_file(
                &mut c,
                id,
                std::path::PathBuf::from("W:/x.png"),
                MediaType::Image
            ),
            Err(OpError::StickerNotFound(id))
        );
    }
}
