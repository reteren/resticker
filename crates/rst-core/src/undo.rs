//! Стек команд отмены/повтора (ROADMAP.md M2 «Undo/Redo, command stack»).
//!
//! Чистая логика без привязки к типам состояния: команда инкапсулирует
//! изменение произвольного состояния (через захваченные ссылки/указатели),
//! стек хранит ограниченную историю. Семантика как у QUndoStack: [`UndoStack::push`]
//! сам выполняет команду, поэтому команда попадает в историю всегда
//! уже применённой.

use std::collections::VecDeque;

/// Глубина истории по умолчанию (в команду входит одно действие
/// редактирования: перемещение, ресайз, поворот и т.п.).
pub const DEFAULT_CAPACITY: usize = 100;

/// Обратимое изменение состояния.
///
/// Инвариант: [`Command::undo`] обязан вернуть состояние ровно в то,
/// что было до [`Command::execute`], — иначе пары undo/redo расходятся
/// (ARCHITECTURE.md, раздел 11: property-тест «N отмен, N повторов ->
/// исходное состояние»).
///
/// Метод называется `execute`, а не `do`: `do` — зарезервированное слово Rust.
pub trait Command {
    /// Выполнить изменение. Вызывается стеком при `push` и при каждом redo.
    fn execute(&mut self);
    /// Отменить изменение.
    fn undo(&mut self);
    /// Имя для UI («Отменить перемещение»); пустая строка — безымянная команда.
    fn name(&self) -> &str {
        ""
    }
}

/// Ограниченная история команд: LIFO-отмена, повтор в обратном порядке.
/// При переполнении вытесняется самая старая команда.
///
/// Не требует от команд `Send`: стек живёт и используется на одном потоке
/// (ядро — единственный владелец состояния, ADR-013).
pub struct UndoStack {
    capacity: usize,
    /// Выполненные команды в порядке выполнения; последняя — сверху.
    done: VecDeque<Box<dyn Command>>,
    /// Отменённые команды; последняя отменённая — сверху.
    undone: Vec<Box<dyn Command>>,
}

impl UndoStack {
    /// Пустой стек с лимитом `capacity` (0 — история отключена:
    /// команды выполняются, но не запоминаются).
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            done: VecDeque::new(),
            undone: Vec::new(),
        }
    }

    /// Выполнить команду и положить её в историю. Стек redo при этом
    /// очищается (новая ветка истории).
    pub fn push(&mut self, mut cmd: Box<dyn Command>) {
        cmd.execute();
        if self.capacity == 0 {
            return;
        }
        self.undone.clear();
        if self.done.len() == self.capacity {
            // Вытесняем самую старую команду — она потеряна для отмены.
            self.done.pop_front();
        }
        self.done.push_back(cmd);
    }

    /// Отменить последнюю команду. `false` — отменять нечего.
    pub fn undo(&mut self) -> bool {
        let Some(mut cmd) = self.done.pop_back() else {
            return false;
        };
        cmd.undo();
        self.undone.push(cmd);
        true
    }

    /// Повторить последнюю отменённую команду. `false` — повторять нечего.
    pub fn redo(&mut self) -> bool {
        let Some(mut cmd) = self.undone.pop() else {
            return false;
        };
        cmd.execute();
        if self.done.len() == self.capacity {
            self.done.pop_front();
        }
        self.done.push_back(cmd);
        true
    }

    /// Есть ли что отменять.
    pub fn can_undo(&self) -> bool {
        !self.done.is_empty()
    }

    /// Есть ли что повторять.
    pub fn can_redo(&self) -> bool {
        !self.undone.is_empty()
    }

    /// Имя команды, которая будет отменена следующей (для пункта меню).
    pub fn undo_name(&self) -> Option<&str> {
        self.done.back().map(|c| c.name())
    }

    /// Имя команды, которая будет повторена следующей.
    pub fn redo_name(&self) -> Option<&str> {
        self.undone.last().map(|c| c.name())
    }

    /// Число команд в истории (доступных для отмены).
    pub fn len(&self) -> usize {
        self.done.len()
    }

    /// История пуста?
    pub fn is_empty(&self) -> bool {
        self.done.is_empty()
    }

    /// Лимит глубины истории.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Сбросить всю историю (загрузка пресета, выход из режима редактирования).
    pub fn clear(&mut self) {
        self.done.clear();
        self.undone.clear();
    }
}

impl Default for UndoStack {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    /// Тестовая команда: прибавляет `delta` к общему счётчику.
    struct Add {
        cell: Rc<Cell<i64>>,
        delta: i64,
        name: &'static str,
    }

    impl Command for Add {
        fn execute(&mut self) {
            self.cell.set(self.cell.get() + self.delta);
        }
        fn undo(&mut self) {
            self.cell.set(self.cell.get() - self.delta);
        }
        fn name(&self) -> &str {
            self.name
        }
    }

    fn add(cell: &Rc<Cell<i64>>, delta: i64) -> Box<dyn Command> {
        Box::new(Add {
            cell: Rc::clone(cell),
            delta,
            name: "add",
        })
    }

    #[test]
    fn push_executes_and_undo_reverts() {
        let cell = Rc::new(Cell::new(0));
        let mut stack = UndoStack::new(10);
        stack.push(add(&cell, 5));
        assert_eq!(cell.get(), 5, "push выполняет команду");
        assert!(stack.undo());
        assert_eq!(cell.get(), 0);
    }

    #[test]
    fn redo_reexecutes() {
        let cell = Rc::new(Cell::new(0));
        let mut stack = UndoStack::new(10);
        stack.push(add(&cell, 5));
        stack.undo();
        assert!(stack.can_redo());
        assert!(stack.redo());
        assert_eq!(cell.get(), 5);
        assert!(!stack.can_redo());
    }

    #[test]
    fn push_clears_redo_branch() {
        let cell = Rc::new(Cell::new(0));
        let mut stack = UndoStack::new(10);
        stack.push(add(&cell, 1));
        stack.undo();
        stack.push(add(&cell, 10));
        assert!(!stack.redo(), "новая команда срезает redo-ветку");
        assert_eq!(cell.get(), 10);
    }

    #[test]
    fn empty_stack_undo_redo_return_false() {
        let mut stack = UndoStack::new(10);
        assert!(!stack.undo());
        assert!(!stack.redo());
        assert!(stack.is_empty());
        assert_eq!(stack.undo_name(), None);
        assert_eq!(stack.redo_name(), None);
    }

    #[test]
    fn capacity_evicts_oldest() {
        let cell = Rc::new(Cell::new(0));
        let mut stack = UndoStack::new(2);
        stack.push(add(&cell, 1));
        stack.push(add(&cell, 2));
        stack.push(add(&cell, 3));
        assert_eq!(stack.len(), 2, "лимит истории");
        assert!(stack.undo());
        assert!(stack.undo());
        assert!(!stack.undo(), "самая старая команда вытеснена");
        assert_eq!(cell.get(), 1, "+1 потерян для отмены");
    }

    #[test]
    fn zero_capacity_keeps_nothing() {
        let cell = Rc::new(Cell::new(0));
        let mut stack = UndoStack::new(0);
        stack.push(add(&cell, 5));
        assert_eq!(cell.get(), 5, "команда всё равно выполняется");
        assert!(!stack.can_undo());
        assert!(!stack.undo());
    }

    #[test]
    fn names_exposed_for_ui() {
        let cell = Rc::new(Cell::new(0));
        let mut stack = UndoStack::new(10);
        stack.push(add(&cell, 1));
        assert_eq!(stack.undo_name(), Some("add"));
        stack.undo();
        assert_eq!(stack.redo_name(), Some("add"));
        assert_eq!(stack.undo_name(), None);
    }

    #[test]
    fn clear_resets_history() {
        let cell = Rc::new(Cell::new(0));
        let mut stack = UndoStack::new(10);
        stack.push(add(&cell, 1));
        stack.undo();
        stack.clear();
        assert!(stack.is_empty());
        assert!(!stack.can_redo());
    }

    /// splitmix64: детерминированный ГПСЧ, чтобы property-тест не зависел
    /// от внешних крейтов и был воспроизводим.
    struct Splitmix64(u64);

    impl Splitmix64 {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }
    }

    /// Property-тест из ARCHITECTURE.md, раздел 11: случайная
    /// последовательность push/undo/redo; эталонная модель на VecDeque
    /// должна сходиться с состоянием счётчика на каждом шаге, а полная
    /// отмена + полный повтор возвращают состояние.
    #[test]
    fn random_ops_match_reference_model() {
        const CAP: usize = 512;
        let cell = Rc::new(Cell::new(0i64));
        let mut stack = UndoStack::new(CAP);
        let mut rng = Splitmix64(0xC0FF_EE42);
        // Эталон: дельты в истории и сумма вытесненных лимитом команд.
        let mut applied: VecDeque<i64> = VecDeque::new();
        let mut redone: Vec<i64> = Vec::new();
        let mut evicted_sum = 0i64;

        for _ in 0..2000 {
            match rng.next() % 3 {
                0 => {
                    let delta = (rng.next() % 2001) as i64 - 1000;
                    stack.push(add(&cell, delta));
                    if applied.len() == CAP {
                        evicted_sum += applied.pop_front().unwrap();
                    }
                    applied.push_back(delta);
                    redone.clear();
                }
                1 => {
                    let expected = applied.pop_back();
                    assert_eq!(stack.undo(), expected.is_some());
                    if let Some(d) = expected {
                        redone.push(d);
                    }
                }
                _ => {
                    let expected = redone.pop();
                    assert_eq!(stack.redo(), expected.is_some());
                    if let Some(d) = expected {
                        applied.push_back(d);
                    }
                }
            }
            assert_eq!(cell.get(), evicted_sum + applied.iter().sum::<i64>());
            assert_eq!(stack.len(), applied.len());
            assert_eq!(stack.can_redo(), !redone.is_empty());
        }

        let target = cell.get();
        while stack.undo() {}
        assert_eq!(cell.get(), evicted_sum, "отменили всё, что в истории");
        while stack.redo() {}
        assert_eq!(cell.get(), target, "повтор вернул состояние");
    }
}
