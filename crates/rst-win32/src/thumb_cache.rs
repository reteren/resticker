//! Кэш растровых превью окон для переключателя Alt+Tab (docs/TILING_DESIGN.md §Р3).
//!
//! Снятие скриншота чужого окна через `PrintWindow` ([`crate::window_thumb::capture`])
//! занимает от единиц до десятков миллисекунд и является синхронным блокирующим вызовом.
//! Вызывать его на каждый кадр оверлея или на каждое нажатие клавиши в переключателе
//! недопустимо — это приведёт к заметным задержкам интерфейса.
//!
//! Модуль предоставляет [`ThumbCache`] — кэш снимков с контролем времени жизни (TTL),
//! LRU-вытеснением при переполнении и кэшированием неудачных попыток захвата.
//!
//! # Ответы на архитектурные вопросы
//!
//! ### 1. Время жизни записей (TTL)
//! Окна в десктопной ОС динамичны (воспроизведение видео, чаты, терминалы, веб-страницы).
//! Превью, снятое 2–4 секунды назад (`DEFAULT_TTL_MS = 3000`), воспринимается глазом
//! как абсолютно актуальное состояние окна, но позволяет полностью исключить повторные
//! тяжелые вызовы `PrintWindow` во время активного сеанса Alt+Tab. При этом короткий TTL
//! гарантирует, что превью не устареет до нерелевантного состояния при повторном вызове
//! переключателя через минуту.
//!
//! ### 2. Кэширование неудачи (Negative Caching)
//! `capture` возвращает `None` для окон с защищённым DRM-контентом, чисто чёрных кадров,
//! свёрнутых окон или защищённых системных процессов. Если не кэшировать `None`, переключатель
//! будет на каждом шаге инициировать дорогостоящий и заведомо безуспешный опрос окна.
//! Неудачные попытки кэшируются с тем же `ttl_ms`: если окно развернётся или станет доступным,
//! после истечения TTL будет произведена повторная попытка захвата.
//!
//! ### 3. Политика вытеснения (LRU)
//! При достижении лимита `max_entries` кэш вытесняет запись, к которой дольше всего не
//! обращались (`last_access_ms`). Это гарантирует, что окна, находящиеся в верхушке MRU-списка
//! переключателя (к которым пользователь обращается чаще всего), остаются в памяти, а фоновые
//! окна освобождаются.
//!
//! ### 4. Оценка потребления памяти (Memory Footprint)
//! - Одно превью размером 144×80 px в формате RGBA (4 байта на пиксель) занимает:
//!   `144 * 80 * 4 = 46 080 байт ≈ 45 КБ`.
//! - При стандартном `max_entries = 32`: `32 * 45 КБ ≈ 1.44 МБ`.
//! - При расширенном `max_entries = 64`: `64 * 45 КБ ≈ 2.88 МБ`.
//!
//! Это пренебрежимо малый объём для десктопного приложения, а взамен
//! переключатель окон отвечает мгновенно.
//!
//! ### 5. Переиспользование HWND в Windows
//! Windows может повторно использовать численные значения `HWND` после закрытия старого окна
//! и создания нового. Если окно закрылось, вызывающий слой вызывает [`ThumbCache::forget`].
//! Даже если событие закрытия было потеряно, короткий TTL (3 секунды) ограничивает окно
//! риска: старый снимок протухнет и будет заменён актуальным превью нового владельца `HWND`.

use std::collections::HashMap;

use crate::window_thumb::{WindowThumb, capture};

/// Время жизни успешного и отрицательного снимка по умолчанию, мс (3 секунды).
pub const DEFAULT_TTL_MS: u64 = 3000;
/// Максимальное количество снимков в кэше по умолчанию.
pub const DEFAULT_MAX_ENTRIES: usize = 32;

/// Тип абстрактной функции захвата превью (для тестирования без Win32).
pub type CaptureFn = Box<dyn FnMut(usize, u32) -> Option<WindowThumb>>;

/// Запись в кэше превью.
#[derive(Debug, Clone)]
struct CacheEntry {
    thumb: Option<WindowThumb>,
    expires_at_ms: u64,
    last_access_ms: u64,
}

/// Кэш снимков окон переключателя Alt+Tab.
pub struct ThumbCache {
    entries: HashMap<usize, CacheEntry>,
    max_entries: usize,
    ttl_ms: u64,
    capturer: Option<CaptureFn>,
}

impl ThumbCache {
    /// Создать новый кэш превью с реальным Win32-захватом [`crate::window_thumb::capture`].
    ///
    /// - `max_entries`: максимальное число записей в памяти.
    /// - `ttl_ms`: время жизни записи в миллисекундах.
    pub fn new(max_entries: usize, ttl_ms: u64) -> Self {
        Self {
            entries: HashMap::new(),
            max_entries,
            ttl_ms,
            capturer: None,
        }
    }

    /// Создать кэш с пользовательской функцией захвата (для модульных тестов без Win32).
    pub fn with_capturer(
        max_entries: usize,
        ttl_ms: u64,
        capturer: impl FnMut(usize, u32) -> Option<WindowThumb> + 'static,
    ) -> Self {
        Self {
            entries: HashMap::new(),
            max_entries,
            ttl_ms,
            capturer: Some(Box::new(capturer)),
        }
    }

    /// Получить снимок окна: возвращает ссылку из кэша, либо производит захват и сохраняет.
    ///
    /// - `hwnd`: хэндл окна.
    /// - `max_side`: максимальная длина стороны превью при захвате.
    /// - `now_ms`: текущее монотонное время в миллисекундах.
    pub fn get(&mut self, hwnd: usize, max_side: u32, now_ms: u64) -> Option<&WindowThumb> {
        // 1. Проверяем наличие свежей записи в кэше
        let is_fresh = match self.entries.get_mut(&hwnd) {
            Some(entry) if now_ms < entry.expires_at_ms => {
                entry.last_access_ms = now_ms;
                true
            }
            _ => false,
        };

        if is_fresh {
            return self.entries.get(&hwnd).and_then(|e| e.thumb.as_ref());
        }

        // 2. Если кэш отключен (max_entries == 0), выполняем захват без сохранения
        if self.max_entries == 0 {
            return None;
        }

        // 3. Выполняем захват превью через Win32 capture либо через тестовый мок
        let thumb = if let Some(ref mut capturer) = self.capturer {
            capturer(hwnd, max_side)
        } else {
            capture(hwnd, max_side)
        };

        // 4. Проверяем переполнение и при необходимости вытесняем самую старую запись (LRU)
        if self.entries.len() >= self.max_entries && !self.entries.contains_key(&hwnd) {
            self.evict_lru();
        }

        // 5. Сохраняем результат (включая None для отрицательного кэширования)
        let entry = CacheEntry {
            thumb,
            expires_at_ms: now_ms.saturating_add(self.ttl_ms),
            last_access_ms: now_ms,
        };

        self.entries.insert(hwnd, entry);
        self.entries.get(&hwnd).and_then(|e| e.thumb.as_ref())
    }

    /// Забыть окно (вызывается при уничтожении окна `EVENT_OBJECT_DESTROY` или удалении из раскладки).
    pub fn forget(&mut self, hwnd: usize) {
        self.entries.remove(&hwnd);
    }

    /// Выбросить все протухшие записи по таймеру.
    pub fn prune(&mut self, now_ms: u64) {
        self.entries.retain(|_, entry| now_ms < entry.expires_at_ms);
    }

    /// Текущее количество записей в кэше (включая отрицательные).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Пуст ли кэш.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Вытеснить наименее востребованную запись (наименьший `last_access_ms`).
    fn evict_lru(&mut self) {
        if let Some((&lru_hwnd, _)) = self
            .entries
            .iter()
            .min_by_key(|(_, entry)| entry.last_access_ms)
        {
            self.entries.remove(&lru_hwnd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn dummy_thumb(id: u8) -> WindowThumb {
        WindowThumb {
            width: 144,
            height: 80,
            rgba: vec![id; 144 * 80 * 4],
        }
    }

    #[test]
    fn cache_stores_and_returns_captured_thumbnail() {
        let mut cache =
            ThumbCache::with_capturer(10, 3000, |hwnd, _| Some(dummy_thumb(hwnd as u8)));

        let thumb = cache.get(1, 100, 1000);
        assert!(thumb.is_some());
        assert_eq!(thumb.unwrap().rgba[0], 1);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn cache_hits_do_not_invoke_capturer_again() {
        let call_count = std::sync::Arc::new(AtomicUsize::new(0));
        let cc = call_count.clone();
        let mut cache = ThumbCache::with_capturer(10, 3000, move |hwnd, _| {
            cc.fetch_add(1, Ordering::SeqCst);
            Some(dummy_thumb(hwnd as u8))
        });

        assert!(cache.get(42, 100, 1000).is_some());
        assert_eq!(call_count.load(Ordering::SeqCst), 1);

        assert!(cache.get(42, 100, 2000).is_some());
        assert_eq!(call_count.load(Ordering::SeqCst), 1);

        assert!(cache.get(42, 100, 3999).is_some());
        assert_eq!(call_count.load(Ordering::SeqCst), 1);

        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn cache_evicts_expired_entries_on_get_after_ttl() {
        let mut call_count = 0;
        let mut cache = ThumbCache::with_capturer(10, 3000, move |_, _| {
            call_count += 1;
            Some(dummy_thumb(call_count))
        });

        // Первый запрос в t = 1000, годен до 4000
        let thumb1 = cache.get(10, 100, 1000).unwrap();
        assert_eq!(thumb1.rgba[0], 1);

        // Второй запрос в t = 4001 (протух) -> повторный захват
        let thumb2 = cache.get(10, 100, 4001).unwrap();
        assert_eq!(thumb2.rgba[0], 2);
    }

    #[test]
    fn cache_prune_removes_expired_entries() {
        let mut cache =
            ThumbCache::with_capturer(10, 2000, |hwnd, _| Some(dummy_thumb(hwnd as u8)));

        cache.get(1, 100, 1000); // годен до 3000
        cache.get(2, 100, 2000); // годен до 4000
        assert_eq!(cache.len(), 2);

        // В t = 3500 окно 1 протухло, окно 2 ещё живо
        cache.prune(3500);
        assert_eq!(cache.len(), 1);
        assert!(cache.entries.contains_key(&2));
        assert!(!cache.entries.contains_key(&1));
    }

    #[test]
    fn cache_forget_removes_entry_immediately() {
        let mut cache =
            ThumbCache::with_capturer(10, 5000, |hwnd, _| Some(dummy_thumb(hwnd as u8)));

        cache.get(100, 100, 1000);
        assert_eq!(cache.len(), 1);

        cache.forget(100);
        assert_eq!(cache.len(), 0);
        assert!(cache.is_empty());
    }

    #[test]
    fn cache_negative_result_is_cached_and_retried_after_ttl() {
        let mut attempts = 0;
        let mut cache = ThumbCache::with_capturer(10, 3000, move |_, _| {
            attempts += 1;
            if attempts == 1 {
                None // первая попытка неудачна
            } else {
                Some(dummy_thumb(99)) // вторая успешна
            }
        });

        // 1. Первая попытка -> None, запоминается как неудача
        assert!(cache.get(50, 100, 1000).is_none());
        assert_eq!(cache.len(), 1);

        // 2. Повторный опрос до истечения TTL не вызывает захват повторно
        assert!(cache.get(50, 100, 2000).is_none());

        // 3. После истечения TTL (t = 4001) происходит повторная попытка -> Some
        let res = cache.get(50, 100, 4001);
        assert!(res.is_some());
        assert_eq!(res.unwrap().rgba[0], 99);
    }

    #[test]
    fn cache_evicts_lru_entry_when_full() {
        let mut cache =
            ThumbCache::with_capturer(3, 10000, |hwnd, _| Some(dummy_thumb(hwnd as u8)));

        cache.get(1, 100, 1000);
        cache.get(2, 100, 1100);
        cache.get(3, 100, 1200);
        assert_eq!(cache.len(), 3);

        // Обращаемся к окну 1, чтобы обновить last_access_ms
        cache.get(1, 100, 1300);

        // Добавляем 4-е окно -> должно вытесниться окно 2 (наименьший last_access_ms = 1100)
        cache.get(4, 100, 1400);
        assert_eq!(cache.len(), 3);
        assert!(cache.entries.contains_key(&1));
        assert!(cache.entries.contains_key(&3));
        assert!(cache.entries.contains_key(&4));
        assert!(!cache.entries.contains_key(&2));
    }

    #[test]
    fn cache_zero_max_entries_does_not_cache_and_returns_none() {
        let mut cache = ThumbCache::with_capturer(0, 3000, |hwnd, _| Some(dummy_thumb(hwnd as u8)));

        assert!(cache.get(1, 100, 1000).is_none());
        assert_eq!(cache.len(), 0);
        assert!(cache.is_empty());
    }

    #[test]
    fn cache_prune_on_empty_cache_is_noop() {
        let mut cache = ThumbCache::new(10, 3000);
        assert_eq!(cache.len(), 0);
        cache.prune(10000);
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn cache_len_and_is_empty_reflect_actual_state() {
        let mut cache = ThumbCache::with_capturer(5, 5000, |hwnd, _| Some(dummy_thumb(hwnd as u8)));

        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);

        cache.get(10, 100, 1000);
        assert!(!cache.is_empty());
        assert_eq!(cache.len(), 1);

        cache.forget(10);
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn cache_access_refreshes_lru_priority() {
        let mut cache =
            ThumbCache::with_capturer(2, 10000, |hwnd, _| Some(dummy_thumb(hwnd as u8)));

        cache.get(1, 100, 100);
        cache.get(2, 100, 200);

        // Обновляем доступ к 1 в t = 300
        cache.get(1, 100, 300);

        // Вставка 3 должна вытеснить 2
        cache.get(3, 100, 400);
        assert!(cache.entries.contains_key(&1));
        assert!(cache.entries.contains_key(&3));
        assert!(!cache.entries.contains_key(&2));
    }

    #[test]
    fn cache_expiration_boundary_conditions() {
        let mut cache = ThumbCache::with_capturer(5, 1000, |hwnd, _| Some(dummy_thumb(hwnd as u8)));

        // Создан в t = 1000, годен до 2000
        cache.get(1, 100, 1000);

        // В t = 1999 запись ещё валидна
        assert!(cache.entries.get(&1).unwrap().expires_at_ms > 1999);

        // В t = 2000 запись уже считается истекшей
        cache.prune(2000);
        assert_eq!(cache.len(), 0);
    }
}
