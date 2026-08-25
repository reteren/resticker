//! Кодирование и сопоставление жителей раскладки: окна и стикеры
//! (M9, docs/TILING_DESIGN.md §T7).
//!
//! # Почему тайлинг живёт внутри resticker?
//!
//! В отличие от внешних тайлинг-менеджеров (komorebi, GlazeWM), resticker управляет
//! как окнами операционной системы, так и собственными стикерами (заметки, картинки,
//! медиаплееры, справочные карточки). Пользователь может поместить стикер в сетку
//! тайлинга наравне с обычными приложениями (например, окно редактора кода слева,
//! а стикер с макетом или TODO-заметкой — справа в соседнем тайле).
//!
//! # Ключевое архитектурное решение: Прозрачный [`WindowKey`]
//!
//! Дерево тайлинга [`super::tree::Tree`] хранит узлы как [`WindowKey(pub u64)`].
//! Дереву безразличен внутренний смысл этого числа — оно оперирует им как непрозрачным
//! уникальным идентификатором. Это позволяет поселить стикеры в дерево раскладки
//! **без малейшего изменения структуры дерева и алгоритмов разбиения**.
//!
//! # Ответы на архитектурные вопросы
//!
//! ## 1. Как отличить окно от стикера в одном `u64`?
//! - **HWND на Win64**: Согласно документации Microsoft (*"The New Data Types"* и
//!   исследованиям Raymond Chen), дескрипторы `HWND` являются 32-битными индексами
//!   таблицы дескрипторов USER32, расширенными до 64 бит. Даже в 64-битном адресном
//!   пространстве канонические адреса пользовательского режима ограничены младшими 48 битами
//!   (`0x00007FFF_FFFFFFFF`).
//! - **Старший бит 63 (`STICKER_FLAG = 1 << 63`)**: Для валидного `HWND` бит 63
//!   **всегда равен 0**. Установка бита 63 (`1 << 63`) однозначно и безопасно помечает
//!   ключ как стикер, исключая любые коллизии с реальными дескрипторами окон ОС.
//!
//! ## 2. Реестр стикеров [`StickerTiles`] и неповторяемость ID
//! - `Uuid` (128 бит) не помещается в 64 бита напрямую (хэширование дало бы коллизии).
//! - Реестр [`StickerTiles`] раздаёт стикерам монотонно возрастающие 63-битные ID.
//! - **Защита от ABA-проблемы**: При удалении стикера (`unregister`) его ID
//!   **не возвращается в пул немедленно**. Если бы ID переиспользовался сразу, старые
//!   ссылки в дереве тайлинга или очереди фокуса MRU ошибочно адресовали бы новый стикер.
//!
//! ## 3. Устойчивость к мусорным ключам
//! - При декодировании неизвестных ID или мусорных чисел возвращается `None`.
//! - Код свободен от `panic!` и переполнений при любых граничных значениях `u64`.
//!
//! ## 4. Сериализация и переживание перезапуска
//! - В отличие от окон ОС, чьи `HWND` случайны и меняются при каждом запуске,
//!   стикеры resticker имеют постоянные [`Uuid`], сохраняемые в `stickers.json`.
//! - Реестр [`StickerTiles`] сериализуется вместе с раскладкой, гарантируя
//!   восстановление точного соответствия ключей после перезапуска.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

use super::tree::WindowKey;

/// Маска старшего бита (бит 63), помечающая стикер в [`WindowKey`].
///
/// Для окон Windows (HWND) этот бит всегда равен 0.
pub const STICKER_FLAG: u64 = 1 << 63;

/// Житель плитки в раскладке тайлинга.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TileKind {
    /// Обычное окно операционной системы Windows (сырой дескриптор `HWND as u64`).
    Window(u64),
    /// Стикер resticker со стабильным идентификатором [`Uuid`].
    Sticker(Uuid),
}

/// Реестр соответствия между 128-битными [`Uuid`] стикеров и ключами [`WindowKey`].
///
/// Сериализуем: переживает перезапуск приложения, сохраняя привязку постоянных
/// стикеров к их слотам в раскладке.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StickerTiles {
    uuid_to_id: HashMap<Uuid, u64>,
    id_to_uuid: HashMap<u64, Uuid>,
    next_id: u64,
}

impl Default for StickerTiles {
    fn default() -> Self {
        Self::new()
    }
}

impl StickerTiles {
    /// Создать новый пустой реестр стикеров.
    pub fn new() -> Self {
        Self {
            uuid_to_id: HashMap::new(),
            id_to_uuid: HashMap::new(),
            next_id: 1,
        }
    }

    /// Получить существующий или зарегистрировать новый `WindowKey` для стикера.
    ///
    /// Повторный вызов для одного и того же `Uuid` всегда возвращает тот же `WindowKey`.
    pub fn get_or_register(&mut self, uuid: Uuid) -> WindowKey {
        if let Some(&id) = self.uuid_to_id.get(&uuid) {
            return WindowKey(id | STICKER_FLAG);
        }

        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .expect("переполнение 63-битного счетчика идентификаторов стикеров");
        self.uuid_to_id.insert(uuid, id);
        self.id_to_uuid.insert(id, uuid);

        WindowKey(id | STICKER_FLAG)
    }

    /// Зарегистрировать стикер в реестре (псевдоним для [`Self::get_or_register`]).
    pub fn register(&mut self, uuid: Uuid) -> WindowKey {
        self.get_or_register(uuid)
    }

    /// Найти [`Uuid`] стикера по ключу [`WindowKey`].
    ///
    /// Возвращает `None`, если ключ не является стикером или отсутствует в реестре.
    pub fn get_uuid(&self, key: WindowKey) -> Option<Uuid> {
        if !is_sticker(key) {
            return None;
        }
        let id = key.0 & !STICKER_FLAG;
        self.id_to_uuid.get(&id).copied()
    }

    /// Найти [`WindowKey`] зарегистрированного стикера по [`Uuid`].
    pub fn get_key(&self, uuid: Uuid) -> Option<WindowKey> {
        self.uuid_to_id
            .get(&uuid)
            .map(|&id| WindowKey(id | STICKER_FLAG))
    }

    /// Удалить стикер из реестра по [`Uuid`].
    ///
    /// Идентификатор удалённого стикера не переиспользуется немедленно,
    /// защищая от ABA-проблем при наличии устаревших ссылок.
    pub fn unregister(&mut self, uuid: Uuid) -> bool {
        if let Some(id) = self.uuid_to_id.remove(&uuid) {
            self.id_to_uuid.remove(&id);
            true
        } else {
            false
        }
    }

    /// Удалить стикер из реестра по его [`WindowKey`].
    pub fn remove_by_key(&mut self, key: WindowKey) -> Option<Uuid> {
        if !is_sticker(key) {
            return None;
        }
        let id = key.0 & !STICKER_FLAG;
        if let Some(uuid) = self.id_to_uuid.remove(&id) {
            self.uuid_to_id.remove(&uuid);
            Some(uuid)
        } else {
            None
        }
    }

    /// Проверить, зарегистрирован ли стикер с данным [`Uuid`].
    pub fn contains_uuid(&self, uuid: Uuid) -> bool {
        self.uuid_to_id.contains_key(&uuid)
    }

    /// Проверить, зарегистрирован ли стикер с данным [`WindowKey`].
    pub fn contains_key(&self, key: WindowKey) -> bool {
        self.get_uuid(key).is_some()
    }

    /// Количество зарегистрированных стикеров в реестре.
    pub fn len(&self) -> usize {
        self.uuid_to_id.len()
    }

    /// Проверить, пуст ли реестр стикеров.
    pub fn is_empty(&self) -> bool {
        self.uuid_to_id.is_empty()
    }

    /// Очистить реестр стикеров (счетчик монотонных ID сохраняется).
    pub fn clear(&mut self) {
        self.uuid_to_id.clear();
        self.id_to_uuid.clear();
    }

    /// Упаковать [`TileKind`] в [`WindowKey`], используя реестр для стикеров.
    pub fn encode(&mut self, kind: &TileKind) -> WindowKey {
        match kind {
            TileKind::Window(hwnd) => encode_window(*hwnd),
            TileKind::Sticker(uuid) => self.get_or_register(*uuid),
        }
    }

    /// Распаковать [`WindowKey`] в [`TileKind`], используя реестр для стикеров.
    pub fn decode(&self, key: WindowKey) -> Option<TileKind> {
        if is_sticker(key) {
            self.get_uuid(key).map(TileKind::Sticker)
        } else if is_window(key) {
            Some(TileKind::Window(key.0))
        } else {
            None
        }
    }
}

/// Проверить, является ли ключ дескриптором стикера (установлен бит 63).
pub fn is_sticker(key: WindowKey) -> bool {
    (key.0 & STICKER_FLAG) != 0
}

/// Проверить, является ли ключ дескриптором окна Win32 (бит 63 сброшен и ключ != 0).
pub fn is_window(key: WindowKey) -> bool {
    key.0 != 0 && (key.0 & STICKER_FLAG) == 0
}

/// Закодировать окно Win32 (`HWND`) в [`WindowKey`].
///
/// Старший бит гарантированно сбрасывается.
pub fn encode_window(hwnd: u64) -> WindowKey {
    WindowKey(hwnd & !STICKER_FLAG)
}

/// Распаковать [`WindowKey`] окна Win32 (`HWND`). Возвращает `None`, если это стикер или 0.
pub fn decode_window(key: WindowKey) -> Option<u64> {
    if is_window(key) { Some(key.0) } else { None }
}

/// Упаковать [`TileKind`] в [`WindowKey`] через реестр стикеров.
pub fn encode(kind: &TileKind, registry: &mut StickerTiles) -> WindowKey {
    registry.encode(kind)
}

/// Распаковать [`WindowKey`] в [`TileKind`] через реестр стикеров.
pub fn decode(key: WindowKey, registry: &StickerTiles) -> Option<TileKind> {
    registry.decode(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_encodes_and_decodes_back() {
        let mut reg = StickerTiles::new();
        let hwnd = 0x00000000_00123456u64;
        let kind = TileKind::Window(hwnd);

        let key = encode(&kind, &mut reg);
        assert!(!is_sticker(key));
        assert!(is_window(key));
        assert_eq!(decode(key, &reg), Some(TileKind::Window(hwnd)));
        assert_eq!(decode_window(key), Some(hwnd));
    }

    #[test]
    fn sticker_encodes_and_decodes_back() {
        let mut reg = StickerTiles::new();
        let uuid = Uuid::new_v4();
        let kind = TileKind::Sticker(uuid);

        let key = encode(&kind, &mut reg);
        assert!(is_sticker(key));
        assert!(!is_window(key));
        assert_eq!(decode(key, &reg), Some(TileKind::Sticker(uuid)));
        assert_eq!(reg.get_uuid(key), Some(uuid));
    }

    #[test]
    fn window_key_is_not_recognized_as_sticker() {
        let reg = StickerTiles::new();
        let win_key = WindowKey(0x00000000_AABBCCDD);
        assert!(!is_sticker(win_key));
        assert_eq!(reg.get_uuid(win_key), None);
    }

    #[test]
    fn sticker_key_is_not_recognized_as_window() {
        let mut reg = StickerTiles::new();
        let uuid = Uuid::new_v4();
        let sticker_key = reg.register(uuid);

        assert!(is_sticker(sticker_key));
        assert!(!is_window(sticker_key));
        assert_eq!(decode_window(sticker_key), None);
    }

    #[test]
    fn garbage_sticker_key_returns_none_without_panic() {
        let reg = StickerTiles::new();
        // Ключ с битом стикера, но несуществующим ID 9999
        let garbage_sticker = WindowKey(STICKER_FLAG | 9999);
        assert_eq!(decode(garbage_sticker, &reg), None);
        assert_eq!(reg.get_uuid(garbage_sticker), None);
    }

    #[test]
    fn zero_window_key_returns_none_without_panic() {
        let reg = StickerTiles::new();
        let zero_key = WindowKey(0);
        assert_eq!(decode(zero_key, &reg), None);
        assert!(!is_window(zero_key));
        assert!(!is_sticker(zero_key));
    }

    #[test]
    fn registry_assigns_distinct_keys_to_distinct_stickers() {
        let mut reg = StickerTiles::new();
        let u1 = Uuid::new_v4();
        let u2 = Uuid::new_v4();

        let k1 = reg.register(u1);
        let k2 = reg.register(u2);
        assert_ne!(k1, k2);
        assert_eq!(reg.len(), 2);
    }

    #[test]
    fn registry_returns_same_key_for_same_sticker_repeatedly() {
        let mut reg = StickerTiles::new();
        let u1 = Uuid::new_v4();

        let k1 = reg.register(u1);
        let k2 = reg.register(u1);
        assert_eq!(k1, k2);
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn unregistered_sticker_is_not_found_in_registry() {
        let mut reg = StickerTiles::new();
        let u1 = Uuid::new_v4();
        let key = reg.register(u1);

        assert!(reg.unregister(u1));
        assert_eq!(reg.get_uuid(key), None);
        assert_eq!(reg.get_key(u1), None);
        assert_eq!(decode(key, &reg), None);
        assert_eq!(reg.len(), 0);
    }

    #[test]
    fn sticker_id_is_not_reused_immediately_after_unregistration() {
        let mut reg = StickerTiles::new();
        let u1 = Uuid::new_v4();
        let u2 = Uuid::new_v4();

        let k1 = reg.register(u1);
        reg.unregister(u1);

        let k2 = reg.register(u2);
        // k2 не должен совпасть с k1
        assert_ne!(k1, k2);
    }

    #[test]
    fn remove_by_key_unregisters_sticker_correctly() {
        let mut reg = StickerTiles::new();
        let u1 = Uuid::new_v4();
        let key = reg.register(u1);

        assert_eq!(reg.remove_by_key(key), Some(u1));
        assert_eq!(reg.get_uuid(key), None);
        assert_eq!(reg.remove_by_key(key), None);
    }

    #[test]
    fn registry_serde_roundtrip_preserves_mappings_and_next_id() {
        let mut reg = StickerTiles::new();
        let u1 = Uuid::new_v4();
        let u2 = Uuid::new_v4();
        let k1 = reg.register(u1);
        let k2 = reg.register(u2);

        let json = serde_json::to_string(&reg).expect("сериализация реестра");
        let mut restored: StickerTiles = serde_json::from_str(&json).expect("десериализация");

        assert_eq!(restored.get_uuid(k1), Some(u1));
        assert_eq!(restored.get_uuid(k2), Some(u2));
        assert_eq!(restored.len(), 2);

        // Проверяем, что счетчик next_id сохранился и новый стикер получит k3 > k2
        let u3 = Uuid::new_v4();
        let k3 = restored.register(u3);
        assert_ne!(k3, k1);
        assert_ne!(k3, k2);
    }

    #[test]
    fn boundary_u64_max_value_handled_safely() {
        let reg = StickerTiles::new();
        let max_key = WindowKey(u64::MAX);
        // Бит 63 выставлен, но ID u64::MAX & !STICKER_FLAG не зарегистрирован -> None
        assert!(is_sticker(max_key));
        assert_eq!(decode(max_key, &reg), None);
    }

    #[test]
    fn is_sticker_and_is_window_predicates_partition_keys_correctly() {
        let win = WindowKey(0x1234);
        assert!(is_window(win));
        assert!(!is_sticker(win));

        let stk = WindowKey(STICKER_FLAG | 0x1234);
        assert!(!is_window(stk));
        assert!(is_sticker(stk));

        let zero = WindowKey(0);
        assert!(!is_window(zero));
        assert!(!is_sticker(zero));
    }

    #[test]
    fn clear_empties_registry_while_retaining_monotonic_id_counter() {
        let mut reg = StickerTiles::new();
        let u1 = Uuid::new_v4();
        let k1 = reg.register(u1);

        reg.clear();
        assert!(reg.is_empty());
        assert_eq!(reg.get_uuid(k1), None);

        let u2 = Uuid::new_v4();
        let k2 = reg.register(u2);
        assert_ne!(k1, k2);
    }

    #[test]
    fn tile_kind_serde_roundtrip() {
        let win = TileKind::Window(0x42);
        let u = Uuid::new_v4();
        let stk = TileKind::Sticker(u);

        let json_win = serde_json::to_string(&win).unwrap();
        let json_stk = serde_json::to_string(&stk).unwrap();

        assert_eq!(serde_json::from_str::<TileKind>(&json_win).unwrap(), win);
        assert_eq!(serde_json::from_str::<TileKind>(&json_stk).unwrap(), stk);
    }
}
