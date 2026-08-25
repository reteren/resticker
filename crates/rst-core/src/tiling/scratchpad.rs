//! Scratchpad — «special workspace» поверх всего (docs/TILING_DESIGN.md, T7).
//!
//! В Hyprland это special workspace: одной клавишей поверх текущего экрана
//! выпадает отложенное окно (терминал, заметки, мессенджер), второй раз —
//! прячется. Здесь — то же самое в чистой модели, без Win32.
//!
//! # Как это сочетается с обычными воркспейсами (workspace.rs)
//!
//! Scratchpad НЕ воркспейс и НЕ дерево: обычный воркспейс владеет деревом
//! плиток ([`crate::tiling::workspace::Workspace`]), а скретчпад — просто
//! список отложенных окон. Показывается он ПОВЕРХ текущего воркспейса: окна
//! воркспейса остаются видимыми под ним, скрывать их не нужно. Единственное
//! отличие от видимых окон воркспейса — позиция: окна скретчпада не
//! раскладываются в плитки, а центрируются на мониторе (см. «Геометрия»).
//!
//! ## Контракт с координатором (кто за что отвечает)
//!
//! Модель хранит только список и флаг видимости. Физику делает координатор
//! (`crates/resticker/src/tiling.rs`), и он обязан соблюдать два инварианта:
//!
//! 1. **Окно не живёт одновременно в дереве воркспейса и в скретчпаде.**
//!    Перед `stash` координатор вызывает `WorkspaceSet::remove_window(key)`
//!    (тот чистит дерево, floating и fullscreen всех воркспейсов разом,
//!    workspace.rs:241), после `restore` — вставляет окно обратно
//!    (`WorkspaceSet::insert_window`). Сам модуль скретчпада гарантирует
//!    только отсутствие дубликатов ВНУТРИ себя; «ровно одно место в системе»
//!    — инвариант уровня координатора, потому что только он держит обе
//!    структуры вместе.
//! 2. **Видимость = cloak.** `visible_windows()` — источник истины для того,
//!    какие окна скретчпада НЕ должны быть скрыты `DWMWA_CLOAKED`: спрятанный
//!    скретчпад возвращает пустой список (координатор cloak-ает его окна),
//!    показанный — полный. Окна воркспейса под скретчпадом при этом НЕ
//!    трогаются (они и так в `WorkspaceSet::visible_windows`), иначе «поверх
//!    всего» превратилось бы в «вместо всего».
//!
//! # Геометрия: плавающее по центру, а не плитка
//!
//! Окно скретчпада — плавающее по центру активного монитора с отступами
//! (координатор считает прямоугольник сам; модель геометрии не знает).
//! Почему не плитка: скретчпад — временный инструмент, а не рабочее
//! пространство; окна в нём (терминал, заметки) не требуют раскладки, а
//! второе дерево с политиками и reconcile'ом не окупается. Несколько окон —
//! простой стек по вертикали в порядке `windows()` (порядок = z-порядок
//! снизу вверх), как в Hyprland.
//!
//! # Смерть окна
//!
//! Модуль не хранит снимков окон и не может узнать, что окно закрылось.
//! Координатор видит смерть по снимку трекера и зовёт `restore(key)`,
//! просто не вставляя окно обратно в раскладку: скретчпад молча перестаёт
//! его числить. Дублировать «живость» окна в модели незачем — это состояние
//! системы, а не модели.
//!
//! # Сериализация и перезапуск
//!
//! `serde`-деривы есть (для тестов и внутрисессионного переноса состояния),
//! но ЧЕСТНО: `WindowKey` — это `HWND as u64` (tree.rs:20), невалидный после
//! перезапуска. Поэтому скретчпад НЕ персистится в config.json (тот же
//! принцип, что у `PinnedWindow`: «нельзя сохранить в пресет»,
//! pinned_window.rs:3) и после рестарта пуст — пользователь отправляет окна
//! в скретчпад заново, как и в Hyprland. Восстановление по правилам
//! exe/класса для скретчпада не делается: это список явного выбора, а не
//! автоприменение правил.

use serde::{Deserialize, Serialize};

use super::tree::WindowKey;

/// Scratchpad одного (текущего) монитора: список отложенных окон + видимость.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Scratchpad {
    /// Окна скретчпада в порядке отправки (`windows()` = z-порядок снизу вверх).
    windows: Vec<WindowKey>,
    /// Показан ли скретчпад прямо сейчас.
    shown: bool,
}

impl Scratchpad {
    /// Пустой спрятанный скретчпад.
    pub fn new() -> Self {
        Self::default()
    }

    /// Отправить окно в скретчпад (оно исчезает с текущего воркспейса).
    ///
    /// `false` — окно уже в скретчпаде: дубликатов не бывает (см. контракт
    /// модуля). Вызывающий (координатор) обязан ПЕРЕД этим вызовом убрать
    /// окно из воркспейсов (`WorkspaceSet::remove_window`), иначе оно
    /// окажется в двух местах сразу.
    pub fn stash(&mut self, key: WindowKey) -> bool {
        if self.windows.contains(&key) {
            return false;
        }
        self.windows.push(key);
        true
    }

    /// Вернуть окно из скретчпада в обычную раскладку.
    ///
    /// Единственный способ вынуть окно из списка. `false` — окна в скретчпаде
    /// не было. Вызывающий решает, куда деть окно дальше: вставить в воркспейс
    /// (`WorkspaceSet::insert_window`) или забыть — именно так убираются
    /// закрытые окна (координатор зовёт `restore` по смерти окна в снимке,
    /// см. докмодуль).
    ///
    /// Побочный эффект: когда из ПОКАЗАННОГО скретчпада вынуто последнее
    /// окно, скретчпад прячется сам — показывать нечего, а «показан» при
    /// пустом списке был бы состоянием-ловушкой (toggle на нём не
    /// срабатывает, см. там же).
    pub fn restore(&mut self, key: WindowKey) -> bool {
        let Some(pos) = self.windows.iter().position(|k| *k == key) else {
            return false;
        };
        self.windows.remove(pos);
        if self.windows.is_empty() {
            self.shown = false;
        }
        true
    }

    /// Показать/спрятать скретчпад поверх текущего экрана.
    ///
    /// Возвращает НОВОЕ состояние показа (`true` — теперь показан). Пустой
    /// скретчпад не переключается и возвращает `false`: показывать нечего,
    /// а флаг «показан» при пустом списке создал бы мёртвое состояние.
    pub fn toggle(&mut self) -> bool {
        if self.windows.is_empty() {
            return false;
        }
        self.shown = !self.shown;
        self.shown
    }

    /// Скретчпад сейчас показан поверх воркспейса?
    pub fn is_shown(&self) -> bool {
        self.shown
    }

    /// Все окна скретчпада в порядке отправки, независимо от видимости.
    pub fn windows(&self) -> &[WindowKey] {
        &self.windows
    }

    /// Окна, которые должны быть видимы прямо сейчас.
    ///
    /// Пусто, когда скретчпад спрятан: его окна уходят под `DWMWA_CLOAKED`
    /// (см. контракт модуля). Порядок — как у [`Self::windows`].
    pub fn visible_windows(&self) -> Vec<WindowKey> {
        if self.shown {
            self.windows.clone()
        } else {
            Vec::new()
        }
    }

    /// Окно числится в скретчпаде?
    pub fn contains(&self, key: WindowKey) -> bool {
        self.windows.contains(&key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(n: u64) -> WindowKey {
        WindowKey(n)
    }

    #[test]
    fn empty_scratchpad_has_no_windows_and_is_hidden() {
        let s = Scratchpad::new();
        assert!(s.windows().is_empty());
        assert!(!s.is_shown());
        assert!(s.visible_windows().is_empty());
        assert!(!s.contains(w(1)));
    }

    #[test]
    fn stash_adds_window_to_the_list() {
        let mut s = Scratchpad::new();
        assert!(s.stash(w(1)));
        assert_eq!(s.windows(), &[w(1)]);
        assert!(s.contains(w(1)));
        // Отправка НЕ показывает скретчпад: окно отложено, экран не тронут.
        assert!(!s.is_shown());
    }

    #[test]
    fn stashing_the_same_window_twice_is_rejected() {
        let mut s = Scratchpad::new();
        assert!(s.stash(w(1)));
        assert!(!s.stash(w(1)), "дубликатов не бывает");
        assert_eq!(s.windows().len(), 1);
    }

    #[test]
    fn stash_preserves_the_order_of_insertion() {
        let mut s = Scratchpad::new();
        s.stash(w(3));
        s.stash(w(1));
        s.stash(w(2));
        assert_eq!(s.windows(), &[w(3), w(1), w(2)], "порядок = z-порядок");
    }

    #[test]
    fn restore_removes_only_the_requested_window() {
        let mut s = Scratchpad::new();
        s.stash(w(1));
        s.stash(w(2));
        assert!(s.restore(w(1)));
        assert_eq!(s.windows(), &[w(2)]);
        assert!(!s.contains(w(1)));
        assert!(s.contains(w(2)));
    }

    #[test]
    fn restoring_a_window_not_in_scratchpad_returns_false() {
        let mut s = Scratchpad::new();
        s.stash(w(1));
        assert!(!s.restore(w(42)));
        assert_eq!(s.windows(), &[w(1)]);
    }

    #[test]
    fn toggle_shows_and_hides_the_scratchpad() {
        let mut s = Scratchpad::new();
        s.stash(w(1));
        assert!(s.toggle(), "первый toggle показывает");
        assert!(s.is_shown());
        assert!(!s.toggle(), "второй toggle прячет");
        assert!(!s.is_shown());
    }

    #[test]
    fn toggling_an_empty_scratchpad_does_nothing() {
        let mut s = Scratchpad::new();
        assert!(!s.toggle());
        assert!(!s.is_shown());
    }

    #[test]
    fn visible_windows_is_empty_while_hidden() {
        let mut s = Scratchpad::new();
        s.stash(w(1));
        s.stash(w(2));
        assert!(
            s.visible_windows().is_empty(),
            "спрятан — нечего показывать"
        );
    }

    #[test]
    fn visible_windows_lists_everything_when_shown() {
        let mut s = Scratchpad::new();
        s.stash(w(1));
        s.stash(w(2));
        s.toggle();
        assert_eq!(s.visible_windows(), vec![w(1), w(2)]);
    }

    #[test]
    fn windows_list_is_visible_even_when_hidden() {
        // Координатору нужен полный список, чтобы решить, что cloak-ать.
        let mut s = Scratchpad::new();
        s.stash(w(1));
        assert_eq!(s.windows(), &[w(1)]);
        assert!(!s.is_shown());
    }

    #[test]
    fn restoring_the_last_window_hides_the_scratchpad() {
        let mut s = Scratchpad::new();
        s.stash(w(1));
        s.toggle();
        assert!(s.restore(w(1)));
        assert!(
            !s.is_shown(),
            "пустой показанный скретчпад — мёртвое состояние"
        );
        assert!(s.visible_windows().is_empty());
    }

    #[test]
    fn closed_window_leaves_the_scratchpad_silently() {
        // Координатор увидел смерть окна по снимку и позвал restore, не
        // вставляя окно обратно в раскладку: скретчпад просто перестаёт
        // числить окно — никаких ошибок и событий.
        let mut s = Scratchpad::new();
        s.stash(w(1));
        s.stash(w(2));
        s.toggle();
        assert!(s.restore(w(1)), "окно мертво — координатор вынимает его");
        assert_eq!(s.windows(), &[w(2)]);
        assert_eq!(s.visible_windows(), vec![w(2)]);
        assert!(!s.contains(w(1)));
    }

    #[test]
    fn scratchpad_survives_a_serde_roundtrip() {
        let mut s = Scratchpad::new();
        s.stash(w(1));
        s.stash(w(2));
        s.toggle();
        let json = serde_json::to_string(&s).unwrap();
        let back: Scratchpad = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
        assert!(back.is_shown());
        assert_eq!(back.visible_windows(), vec![w(1), w(2)]);
    }

    #[test]
    fn scratchpad_defaults_roundtrip_through_serde() {
        // Сериализация пустого скретчпада не ломается и возвращает дефолт.
        let s = Scratchpad::new();
        let json = serde_json::to_string(&s).unwrap();
        let back: Scratchpad = serde_json::from_str(&json).unwrap();
        assert_eq!(back, Scratchpad::new());
    }
}
