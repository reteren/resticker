//! Кэш GPU-текстур UI (docs/M2_WIRING_PLAN.md, §2–3): 1×1 заливка цвета под
//! `Primitive::Fill` и растризованный текст под `Primitive::Text`. Оба кэша —
//! «создай один раз на ключ»: повторный запрос возвращает закэшированную
//! текстуру, фабрика (GPU) не вызывается.
//!
//! Текстуры создаются через [`TextureFactory`], чтобы модуль не зависел от
//! конкретного устройства: реальная фабрика — [`crate::Device`], в тестах —
//! мок без D3D11-устройства.

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use crate::text;
use crate::{Device, RenderError, Texture};

/// Фабрика GPU-текстур для [`UiTextures`]: единственная операция — залить
/// RGBA-пиксели (straight alpha, premultiply делает загрузчик текстуры)
/// в текстуру. Реальная реализация — [`crate::Device`]; в тестах
/// подставляется мок, считающий вызовы.
pub trait TextureFactory {
    /// Тип текстуры фабрики (у `Device` — [`Texture`]).
    type Texture: Clone;

    /// Залить `data` (RGBA `width`×`height`, straight alpha) в текстуру.
    fn create_texture_from_rgba(
        &self,
        data: &[u8],
        width: u32,
        height: u32,
    ) -> Result<Self::Texture, RenderError>;
}

impl TextureFactory for Device {
    type Texture = Texture;

    fn create_texture_from_rgba(
        &self,
        data: &[u8],
        width: u32,
        height: u32,
    ) -> Result<Self::Texture, RenderError> {
        Device::create_texture_from_rgba(self, data, width, height)
    }
}

/// Кэш текстур UI редактора (docs/M2_WIRING_PLAN.md, §2–3): 1×1 пиксель цвета
/// под `Primitive::Fill` и растр строки под `Primitive::Text`.
///
/// `T` — тип текстуры фабрики (у боевого устройства — [`Texture`], в тестах —
/// фейк). Параметризация по типу нужна, чтобы юнит-тесты обходились без GPU.
pub struct UiTextures<T> {
    /// Целочисленный масштаб растеризации текста (1 для 100% DPI, 2 для 200%;
    /// дробный DPI округляет вверх вызывающий слой). Масштаб фиксируется на
    /// весь кэш — ключ кэша текста (строка, цвет), а M1 задаёт DPI один раз.
    scale: u32,
    /// 1×1 текстуры цвета под `Primitive::Fill`.
    fills: HashMap<[u8; 3], T>,
    /// Растры под `Primitive::Text`, ключ — (строка, цвет).
    texts: HashMap<(String, [u8; 3]), T>,
}

impl<T> UiTextures<T> {
    /// Пустой кэш с масштабом растеризации текста `scale` (0 трактуется как 1).
    pub fn new(scale: u32) -> Self {
        Self {
            scale,
            fills: HashMap::new(),
            texts: HashMap::new(),
        }
    }

    /// Число закэшированных цветных заливок.
    pub fn fill_len(&self) -> usize {
        self.fills.len()
    }

    /// Число закэшированных растров текста.
    pub fn text_len(&self) -> usize {
        self.texts.len()
    }
}

impl<T> Default for UiTextures<T> {
    fn default() -> Self {
        Self::new(1)
    }
}

impl<T: Clone> UiTextures<T> {
    /// Текстура цвета `color` (1×1, непрозрачная; прозрачность задаёт спрайт
    /// при отрисовке). Повторный запрос того же цвета возвращает кэш —
    /// фабрика не вызывается.
    pub fn fill_texture<F: TextureFactory<Texture = T>>(
        &mut self,
        color: [u8; 3],
        factory: &F,
    ) -> Result<T, RenderError> {
        match self.fills.entry(color) {
            Entry::Occupied(e) => Ok(e.get().clone()),
            Entry::Vacant(e) => {
                let texture = factory.create_texture_from_rgba(
                    &[color[0], color[1], color[2], 0xff],
                    1,
                    1,
                )?;
                Ok(e.insert(texture).clone())
            }
        }
    }

    /// Текстура растризованного `text` цвета `color`; кэш по (строка, цвет),
    /// масштаб растеризации — из конструктора. Повторный запрос той же пары
    /// возвращает кэш.
    pub fn text_texture<F: TextureFactory<Texture = T>>(
        &mut self,
        text: &str,
        color: [u8; 3],
        factory: &F,
    ) -> Result<T, RenderError> {
        match self.texts.entry((text.to_string(), color)) {
            Entry::Occupied(e) => Ok(e.get().clone()),
            Entry::Vacant(e) => {
                let (data, w, h) = text::rasterize(text, color, self.scale);
                let texture = factory.create_texture_from_rgba(&data, w, h)?;
                Ok(e.insert(texture).clone())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    /// Фейковая текстура: порядковый номер из счётчика фабрики и размеры.
    /// `seq` позволяет проверять тождество закэшированных объектов.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct FakeTexture {
        seq: u32,
        width: u32,
        height: u32,
    }

    /// Мок-фабрика: считает вызовы, помнит последние переданные пиксели и
    /// умеет «отказывать» — чтобы проверить, что ошибка не кэшируется.
    #[derive(Default)]
    struct MockFactory {
        calls: Cell<u32>,
        fail: Cell<bool>,
        last: RefCell<Option<(Vec<u8>, u32, u32)>>,
    }

    impl MockFactory {
        fn calls(&self) -> u32 {
            self.calls.get()
        }

        fn set_fail(&self, fail: bool) {
            self.fail.set(fail);
        }

        fn last(&self) -> (Vec<u8>, u32, u32) {
            self.last
                .borrow()
                .clone()
                .expect("фабрика ещё не вызывалась")
        }
    }

    impl TextureFactory for MockFactory {
        type Texture = FakeTexture;

        fn create_texture_from_rgba(
            &self,
            data: &[u8],
            width: u32,
            height: u32,
        ) -> Result<Self::Texture, RenderError> {
            self.calls.set(self.calls.get() + 1);
            *self.last.borrow_mut() = Some((data.to_vec(), width, height));
            if self.fail.get() {
                return Err(RenderError::InvalidTextureData(
                    "мок-фабрика отказала".to_string(),
                ));
            }
            Ok(FakeTexture {
                seq: self.calls.get(),
                width,
                height,
            })
        }
    }

    #[test]
    fn fill_same_color_hits_cache_once() {
        let mut cache = UiTextures::new(1);
        let factory = MockFactory::default();
        let a = cache.fill_texture([0x4f, 0x9c, 0xff], &factory).unwrap();
        let b = cache.fill_texture([0x4f, 0x9c, 0xff], &factory).unwrap();
        assert_eq!(a, b);
        assert_eq!(factory.calls(), 1);
        assert_eq!(cache.fill_len(), 1);
    }

    #[test]
    fn fill_different_colors_are_distinct() {
        let mut cache = UiTextures::new(1);
        let factory = MockFactory::default();
        let a = cache.fill_texture([0xff, 0, 0], &factory).unwrap();
        let b = cache.fill_texture([0, 0xff, 0], &factory).unwrap();
        assert_ne!(a, b);
        assert_eq!(factory.calls(), 2);
        assert_eq!(cache.fill_len(), 2);
    }

    #[test]
    fn fill_cache_does_not_grow_on_hits() {
        let mut cache = UiTextures::new(1);
        let factory = MockFactory::default();
        for _ in 0..100 {
            let _ = cache.fill_texture([0, 0, 0], &factory).unwrap();
        }
        assert_eq!(cache.fill_len(), 1);
        assert_eq!(factory.calls(), 1);
    }

    #[test]
    fn fill_passes_opaque_pixel_to_factory() {
        let mut cache = UiTextures::new(1);
        let factory = MockFactory::default();
        let _ = cache.fill_texture([10, 20, 30], &factory).unwrap();
        assert_eq!(factory.last(), (vec![10, 20, 30, 0xff], 1, 1));
    }

    #[test]
    fn fill_error_is_not_cached() {
        let mut cache = UiTextures::new(1);
        let factory = MockFactory::default();
        factory.set_fail(true);
        assert!(cache.fill_texture([1, 2, 3], &factory).is_err());
        assert_eq!(cache.fill_len(), 0);
        factory.set_fail(false);
        let _ = cache.fill_texture([1, 2, 3], &factory).unwrap();
        assert_eq!(factory.calls(), 2, "повтор после ошибки создаёт заново");
        assert_eq!(cache.fill_len(), 1);
    }

    #[test]
    fn text_same_pair_hits_cache_once() {
        let mut cache = UiTextures::new(2);
        let factory = MockFactory::default();
        let a = cache
            .text_texture("100", [0xf0, 0xf0, 0xf0], &factory)
            .unwrap();
        let b = cache
            .text_texture("100", [0xf0, 0xf0, 0xf0], &factory)
            .unwrap();
        assert_eq!(a, b);
        assert_eq!(factory.calls(), 1);
        assert_eq!(cache.text_len(), 1);
        assert_eq!(cache.fill_len(), 0, "кэши заливок и текста независимы");
    }

    #[test]
    fn text_same_string_different_color_is_distinct() {
        let mut cache = UiTextures::new(1);
        let factory = MockFactory::default();
        let a = cache
            .text_texture("100", [0xff, 0xff, 0xff], &factory)
            .unwrap();
        let b = cache
            .text_texture("100", [0x4f, 0x9c, 0xff], &factory)
            .unwrap();
        assert_ne!(a, b);
        assert_eq!(cache.text_len(), 2);
    }

    #[test]
    fn text_different_strings_are_distinct() {
        let mut cache = UiTextures::new(1);
        let factory = MockFactory::default();
        let a = cache
            .text_texture("100", [0xff, 0xff, 0xff], &factory)
            .unwrap();
        let b = cache
            .text_texture("10", [0xff, 0xff, 0xff], &factory)
            .unwrap();
        assert_ne!(a, b);
        assert_eq!(cache.text_len(), 2);
    }

    #[test]
    fn text_cache_does_not_grow_on_hits() {
        let mut cache = UiTextures::new(1);
        let factory = MockFactory::default();
        for _ in 0..50 {
            let _ = cache.text_texture("8%", [1, 2, 3], &factory).unwrap();
        }
        assert_eq!(cache.text_len(), 1);
        assert_eq!(factory.calls(), 1);
    }

    #[test]
    fn text_rasterizes_with_scale_before_factory_call() {
        let mut cache = UiTextures::new(2);
        let factory = MockFactory::default();
        let color = [10, 20, 30];
        let _ = cache.text_texture("100", color, &factory).unwrap();
        let expected = text::rasterize("100", color, 2);
        assert_eq!(factory.last(), (expected.0, expected.1, expected.2));
    }

    #[test]
    fn text_error_is_not_cached() {
        let mut cache = UiTextures::new(1);
        let factory = MockFactory::default();
        factory.set_fail(true);
        assert!(cache.text_texture("100", [1, 2, 3], &factory).is_err());
        assert_eq!(cache.text_len(), 0);
        factory.set_fail(false);
        let _ = cache.text_texture("100", [1, 2, 3], &factory).unwrap();
        assert_eq!(factory.calls(), 2);
        assert_eq!(cache.text_len(), 1);
    }
}
