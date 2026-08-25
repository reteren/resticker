//! Таблица биндов и конечный автомат модальных режимов (submaps)
//! (docs/TILING_DESIGN.md §T3, docs/research/tiling/R4_KEYBINDS.md §4, §5).
//!
//! # Архитектура модальных режимов
//!
//! Модальные режимы (submaps в терминологии Hyprland и i3) позволяют временно
//! переназначить клавиатуру для специализированных операций (например, режим ресайза
//! или перемещения окон с помощью одиночных клавиш H/J/K/L или стрелок).
//!
//! Данный модуль платформенно-чист: принимает виртуальный код клавиши и состояние
//! модификаторов [`KeyChord`], сверяет их с текущим активным режимом и возвращает
//! решение [`Resolution`]:
//! - [`Resolution::PassThrough`] — комбинация не перехвачена, передать приложению в ОС.
//! - [`Resolution::Fire`] — сработало действие тайлинга, поглотить нажатие в хуке.
//! - [`Resolution::ModeChanged`] — вход или выход из модального режима (глотается, но
//!   не передается в диспетчер окон).
//!
//! # Ключевые правила
//!
//! 1. **Изоляция режимов**: находясь в режиме `Some(X)`, срабатывают ТОЛЬКО бинды
//!    этого режима. Глобальные бинды (`submap == None`) блокируются, чтобы случайное
//!    нажатие (например, `Alt+1`) не привело к неожиданным побочным действиям (переключению
//!    воркспейса) посреди тонкой подгонки размера окна.
//! 2. **Аварийный Escape**: клавиша `Escape` без модификаторов ВСЕГДА осуществляет выход
//!    из любого модального режима в обычный (`None`), даже если в конфигурации отсутствует
//!    явный бинд. Это критическая защита от зависания клавиатуры (Modal Lockout) при ошибках
//!    пользовательской конфигурации.
//! 3. **Приоритет последнего правила**: при наличии дубликатов комбинаций в одном режиме
//!    всегда срабатывает последняя объявленная запись (позволяет переопределять дефолты в
//!    конце `config.json`).
//! 4. **Плоский набор перехвата (`swallow_set`)**: возвращает все возможные комбинации всех
//!    режимов сразу, так как низкоуровневый хук Win32 (`WH_KEYBOARD_LL`, бюджет 1 мс)
//!    устанавливается один раз и не может перестраивать таблицы на каждый вход в submap.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::action::Action;

/// Виртуальный код клавиши Escape в Windows (`VK_ESCAPE = 0x1B`).
pub const VK_ESCAPE: u32 = 0x1B;

/// Нажатая комбинация клавиш в платформенно-чистом виде.
///
/// Координатор заполняет её из Win32-события клавиатуры (`KBDLLHOOKSTRUCT`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct KeyChord {
    /// Виртуальный код клавиши Windows (VK_*).
    pub vk: u32,
    /// Зажат ли Ctrl.
    pub ctrl: bool,
    /// Зажат ли Alt.
    pub alt: bool,
    /// Зажат ли Shift.
    pub shift: bool,
    /// Зажат ли Win (Super / Meta).
    pub win: bool,
}

impl KeyChord {
    /// Создать комбинацию клавиш.
    pub fn new(vk: u32, ctrl: bool, alt: bool, shift: bool, win: bool) -> Self {
        Self {
            vk,
            ctrl,
            alt,
            shift,
            win,
        }
    }
}

/// Привязка комбинации клавиш к действию в определенном режиме.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Binding {
    /// Комбинация клавиш.
    pub chord: KeyChord,
    /// Действие, выполняемое по нажатию.
    pub action: Action,
    /// Имя модального режима. `None` — глобальный режим.
    pub submap: Option<String>,
}

/// Результат разбора нажатия таблицы биндов.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Resolution {
    /// Ничего нашего: пропустить нажатие дальше целевому окну в ОС (`CallNextHookEx`).
    PassThrough,
    /// Сработал бинд тайлинга. Нажатие поглощается (`LRESULT(1)`), действие передается на исполнение.
    Fire(Action),
    /// Сменился модальный режим (вход или выход). Нажатие поглощается, окна не затрагиваются.
    ModeChanged { submap: Option<String> },
}

/// Таблица биндов и автомат модальных режимов.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct BindTable {
    /// Список биндов в порядке объявления.
    bindings: Vec<Binding>,
    /// Текущий активный режим (`None` — обычный глобальный режим).
    current_submap: Option<String>,
}

impl BindTable {
    /// Создать таблицу биндов с начальным глобальным режимом.
    pub fn new(bindings: Vec<Binding>) -> Self {
        Self {
            bindings,
            current_submap: None,
        }
    }

    /// Текущий модальный режим. `None` — обычный глобальный режим.
    pub fn submap(&self) -> Option<&str> {
        self.current_submap.as_deref()
    }

    /// Разобрать нажатие комбинации клавиш с учётом текущего режима.
    ///
    /// Единственная точка входа для клавиатурного хука.
    pub fn resolve(&mut self, chord: KeyChord) -> Resolution {
        let current = self.current_submap.as_deref();

        // Ищем совпадение с конца списка: последнее объявленное правило перебивает предыдущие
        let matching_binding = self
            .bindings
            .iter()
            .rev()
            .find(|b| b.submap.as_deref() == current && b.chord == chord);

        if let Some(b) = matching_binding {
            match &b.action {
                Action::EnterSubmap(target_submap) => {
                    self.current_submap = Some(target_submap.clone());
                    Resolution::ModeChanged {
                        submap: self.current_submap.clone(),
                    }
                }
                Action::LeaveSubmap => {
                    self.current_submap = None;
                    Resolution::ModeChanged { submap: None }
                }
                action => Resolution::Fire(action.clone()),
            }
        } else {
            // Если в модальном режиме нажата чистая клавиша Escape без модификаторов,
            // и для нее не было найдено явного переопределения, срабатывает аварийный выход.
            if self.current_submap.is_some()
                && chord.vk == VK_ESCAPE
                && !chord.ctrl
                && !chord.alt
                && !chord.shift
                && !chord.win
            {
                self.current_submap = None;
                Resolution::ModeChanged { submap: None }
            } else {
                Resolution::PassThrough
            }
        }
    }

    /// Принудительно сбросить модальный режим в глобальный (`None`).
    ///
    /// Вызывается координатором при потере фокуса, клике мимо или истечении таймаута бездействия.
    pub fn reset(&mut self) {
        self.current_submap = None;
    }

    /// Плоский набор всех комбинаций клавиш, которые таблица способна поглотить.
    ///
    /// Возвращает комбинации для всех режимов сразу (включая аварийный `Escape`),
    /// что позволяет Win32-хуку мгновенно фильтровать события без блокировок.
    /// Комбинации, которые надо перехватывать ПРЯМО СЕЙЧАС, в текущем режиме.
    ///
    /// Именно это отдаётся клавиатурному хуку, а не [`Self::swallow_set`].
    /// Разница критична: в модальном режиме бинды - одиночные клавиши
    /// (hjkl), и если хук глотает их всегда, пользователь теряет эти буквы
    /// во ВСЕХ приложениях, пока тайлинг включён. Найдено ревью
    /// (docs/research/tiling/REVIEW_WIN32_T2_T3.md, находка 1).
    ///
    /// Переставлять сам хук при этом не нужно: меняется только таблица за
    /// ним (`keyboard_guard::set_swallow_set`), а это короткая запись.
    pub fn active_swallow_set(&self) -> Vec<KeyChord> {
        let current = self.current_submap.as_deref();
        let mut set: HashSet<KeyChord> = self
            .bindings
            .iter()
            .filter(|b| b.submap.as_deref() == current)
            .map(|b| b.chord)
            .collect();
        // Аварийный выход перехватываем, только когда есть откуда выходить:
        // в обычном режиме Escape обязан доставаться приложениям.
        if current.is_some() {
            set.insert(KeyChord {
                vk: VK_ESCAPE,
                ctrl: false,
                alt: false,
                shift: false,
                win: false,
            });
        }
        let mut result: Vec<KeyChord> = set.into_iter().collect();
        result.sort_by_key(|c| (c.vk, c.ctrl, c.alt, c.shift, c.win));
        result
    }

    /// Комбинации ВСЕХ режимов сразу.
    ///
    /// Хуку это отдавать нельзя (см. [`Self::active_swallow_set`]) - метод
    /// нужен настройкам и диагностике: какие комбинации вообще заняты
    /// тайлингом.
    pub fn swallow_set(&self) -> Vec<KeyChord> {
        let mut set = HashSet::new();
        let mut has_submaps = false;

        for b in &self.bindings {
            set.insert(b.chord);
            if b.submap.is_some() {
                has_submaps = true;
            }
        }

        // Если в таблице объявлены модальные режимы, аварийный Escape должен перехватываться хуком
        if has_submaps {
            set.insert(KeyChord {
                vk: VK_ESCAPE,
                ctrl: false,
                alt: false,
                shift: false,
                win: false,
            });
        }

        let mut result: Vec<KeyChord> = set.into_iter().collect();
        result.sort_by_key(|c| (c.vk, c.ctrl, c.alt, c.shift, c.win));
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiling::ops::Direction;

    // Вспомогательные коды клавиш
    const VK_H: u32 = 0x48;
    const VK_L: u32 = 0x4C;
    const VK_R: u32 = 0x52;
    const VK_1: u32 = 0x31;

    fn chord(vk: u32, ctrl: bool, alt: bool, shift: bool, win: bool) -> KeyChord {
        KeyChord::new(vk, ctrl, alt, shift, win)
    }

    fn alt(vk: u32) -> KeyChord {
        chord(vk, false, true, false, false)
    }

    fn alt_shift(vk: u32) -> KeyChord {
        chord(vk, false, true, true, false)
    }

    fn bare(vk: u32) -> KeyChord {
        chord(vk, false, false, false, false)
    }

    #[test]
    fn global_binding_fires_in_normal_mode() {
        let mut table = BindTable::new(vec![Binding {
            chord: alt(VK_H),
            action: Action::FocusDirection(Direction::Left),
            submap: None,
        }]);

        let res = table.resolve(alt(VK_H));
        assert_eq!(
            res,
            Resolution::Fire(Action::FocusDirection(Direction::Left))
        );
        assert_eq!(table.submap(), None);
    }

    #[test]
    fn global_bindings_do_not_fire_in_submap_mode() {
        let mut table = BindTable::new(vec![
            Binding {
                chord: alt(VK_R),
                action: Action::EnterSubmap("resize".into()),
                submap: None,
            },
            Binding {
                chord: alt(VK_1),
                action: Action::Workspace(1),
                submap: None,
            },
            Binding {
                chord: bare(VK_H),
                action: Action::Resize {
                    dir: Direction::Left,
                    delta: 0.05,
                },
                submap: Some("resize".into()),
            },
        ]);

        // Входим в режим resize
        let res = table.resolve(alt(VK_R));
        assert_eq!(
            res,
            Resolution::ModeChanged {
                submap: Some("resize".into())
            }
        );
        assert_eq!(table.submap(), Some("resize"));

        // Глобальный бинд Alt+1 в режиме resize обязан игнорироваться (PassThrough)
        let res = table.resolve(alt(VK_1));
        assert_eq!(res, Resolution::PassThrough);
    }

    #[test]
    fn submap_binding_fires_only_in_its_submap() {
        let mut table = BindTable::new(vec![
            Binding {
                chord: alt(VK_R),
                action: Action::EnterSubmap("resize".into()),
                submap: None,
            },
            Binding {
                chord: bare(VK_H),
                action: Action::Resize {
                    dir: Direction::Left,
                    delta: 0.05,
                },
                submap: Some("resize".into()),
            },
        ]);

        // Одиночная клавиша 'H' в обычном режиме не перехватывается
        assert_eq!(table.resolve(bare(VK_H)), Resolution::PassThrough);

        // Входим в режим resize
        table.resolve(alt(VK_R));

        // Теперь 'H' срабатывает как Resize
        assert_eq!(
            table.resolve(bare(VK_H)),
            Resolution::Fire(Action::Resize {
                dir: Direction::Left,
                delta: 0.05,
            })
        );
    }

    #[test]
    fn entering_and_leaving_submap_via_actions_updates_mode() {
        let mut table = BindTable::new(vec![
            Binding {
                chord: alt(VK_R),
                action: Action::EnterSubmap("resize".into()),
                submap: None,
            },
            Binding {
                chord: bare(VK_ESCAPE),
                action: Action::LeaveSubmap,
                submap: Some("resize".into()),
            },
        ]);

        assert_eq!(table.submap(), None);

        // Вход
        assert_eq!(
            table.resolve(alt(VK_R)),
            Resolution::ModeChanged {
                submap: Some("resize".into())
            }
        );
        assert_eq!(table.submap(), Some("resize"));

        // Выход
        assert_eq!(
            table.resolve(bare(VK_ESCAPE)),
            Resolution::ModeChanged { submap: None }
        );
        assert_eq!(table.submap(), None);
    }

    #[test]
    fn escape_leaves_submap_even_without_explicit_binding() {
        // Таблица без бинда на Escape внутри submap
        let mut table = BindTable::new(vec![
            Binding {
                chord: alt(VK_R),
                action: Action::EnterSubmap("resize".into()),
                submap: None,
            },
            Binding {
                chord: bare(VK_H),
                action: Action::Resize {
                    dir: Direction::Left,
                    delta: 0.05,
                },
                submap: Some("resize".into()),
            },
        ]);

        table.resolve(alt(VK_R));
        assert_eq!(table.submap(), Some("resize"));

        // Аварийный Escape возвращает в обычный режим
        assert_eq!(
            table.resolve(bare(VK_ESCAPE)),
            Resolution::ModeChanged { submap: None }
        );
        assert_eq!(table.submap(), None);
    }

    #[test]
    fn escape_in_normal_mode_is_passed_through() {
        let mut table = BindTable::new(vec![Binding {
            chord: alt(VK_H),
            action: Action::FocusDirection(Direction::Left),
            submap: None,
        }]);

        // Escape в обычном режиме без явного бинда не должен глотаться
        assert_eq!(table.resolve(bare(VK_ESCAPE)), Resolution::PassThrough);
        assert_eq!(table.submap(), None);
    }

    #[test]
    fn unknown_chord_in_normal_mode_is_passed_through() {
        let mut table = BindTable::new(vec![Binding {
            chord: alt(VK_H),
            action: Action::FocusDirection(Direction::Left),
            submap: None,
        }]);

        assert_eq!(table.resolve(alt(VK_L)), Resolution::PassThrough);
    }

    #[test]
    fn unknown_chord_in_submap_mode_is_passed_through() {
        let mut table = BindTable::new(vec![
            Binding {
                chord: alt(VK_R),
                action: Action::EnterSubmap("resize".into()),
                submap: None,
            },
            Binding {
                chord: bare(VK_H),
                action: Action::Resize {
                    dir: Direction::Left,
                    delta: 0.05,
                },
                submap: Some("resize".into()),
            },
        ]);

        table.resolve(alt(VK_R));

        // 'L' не объявлена в submap resize -> PassThrough
        assert_eq!(table.resolve(bare(VK_L)), Resolution::PassThrough);
        // Режим при этом не сбрасывается
        assert_eq!(table.submap(), Some("resize"));
    }

    #[test]
    fn duplicate_bindings_in_same_submap_last_one_wins() {
        let mut table = BindTable::new(vec![
            Binding {
                chord: alt(VK_H),
                action: Action::FocusDirection(Direction::Left),
                submap: None,
            },
            // Пользователь переопределил Alt+H в конец конфига на MoveDirection
            Binding {
                chord: alt(VK_H),
                action: Action::MoveDirection(Direction::Left),
                submap: None,
            },
        ]);

        assert_eq!(
            table.resolve(alt(VK_H)),
            Resolution::Fire(Action::MoveDirection(Direction::Left))
        );
    }

    #[test]
    fn reset_forces_return_to_normal_mode() {
        let mut table = BindTable::new(vec![Binding {
            chord: alt(VK_R),
            action: Action::EnterSubmap("resize".into()),
            submap: None,
        }]);

        table.resolve(alt(VK_R));
        assert_eq!(table.submap(), Some("resize"));

        table.reset();
        assert_eq!(table.submap(), None);
    }

    #[test]
    fn swallow_set_contains_chords_from_all_modes_and_emergency_escape() {
        let table = BindTable::new(vec![
            Binding {
                chord: alt(VK_H),
                action: Action::FocusDirection(Direction::Left),
                submap: None,
            },
            Binding {
                chord: bare(VK_L),
                action: Action::Resize {
                    dir: Direction::Right,
                    delta: 0.05,
                },
                submap: Some("resize".into()),
            },
        ]);

        let set = table.swallow_set();
        assert!(set.contains(&alt(VK_H)));
        assert!(set.contains(&bare(VK_L)));
        // Аварийный Escape должен присутствовать из-за наличия submap
        assert!(set.contains(&bare(VK_ESCAPE)));
        assert_eq!(set.len(), 3);
    }

    #[test]
    fn empty_table_swallows_nothing_and_passes_through() {
        let mut table = BindTable::new(vec![]);
        assert!(table.swallow_set().is_empty());
        assert_eq!(table.resolve(alt(VK_H)), Resolution::PassThrough);
        assert_eq!(table.resolve(bare(VK_ESCAPE)), Resolution::PassThrough);
    }

    #[test]
    fn modifier_combinations_are_distinguished_strictly() {
        let mut table = BindTable::new(vec![
            Binding {
                chord: alt(VK_H),
                action: Action::FocusDirection(Direction::Left),
                submap: None,
            },
            Binding {
                chord: alt_shift(VK_H),
                action: Action::MoveDirection(Direction::Left),
                submap: None,
            },
        ]);

        assert_eq!(
            table.resolve(alt(VK_H)),
            Resolution::Fire(Action::FocusDirection(Direction::Left))
        );
        assert_eq!(
            table.resolve(alt_shift(VK_H)),
            Resolution::Fire(Action::MoveDirection(Direction::Left))
        );
    }

    #[test]
    fn entering_empty_submap_switches_mode_and_allows_emergency_escape() {
        let mut table = BindTable::new(vec![Binding {
            chord: alt(VK_R),
            action: Action::EnterSubmap("empty_mode".into()),
            submap: None,
        }]);

        assert_eq!(
            table.resolve(alt(VK_R)),
            Resolution::ModeChanged {
                submap: Some("empty_mode".into())
            }
        );
        assert_eq!(table.submap(), Some("empty_mode"));

        // Любая клавиша пропускается
        assert_eq!(table.resolve(bare(VK_H)), Resolution::PassThrough);

        // Escape успешно выводит из пустого режима
        assert_eq!(
            table.resolve(bare(VK_ESCAPE)),
            Resolution::ModeChanged { submap: None }
        );
        assert_eq!(table.submap(), None);
    }

    #[test]
    fn explicit_escape_action_leaves_submap_cleanly() {
        let mut table = BindTable::new(vec![
            Binding {
                chord: alt(VK_R),
                action: Action::EnterSubmap("resize".into()),
                submap: None,
            },
            Binding {
                chord: bare(VK_ESCAPE),
                action: Action::LeaveSubmap,
                submap: Some("resize".into()),
            },
        ]);

        table.resolve(alt(VK_R));
        assert_eq!(table.submap(), Some("resize"));

        assert_eq!(
            table.resolve(bare(VK_ESCAPE)),
            Resolution::ModeChanged { submap: None }
        );
        assert_eq!(table.submap(), None);
    }

    #[test]
    fn submap_getter_reflects_current_state() {
        let mut table = BindTable::new(vec![
            Binding {
                chord: alt(VK_R),
                action: Action::EnterSubmap("mode_a".into()),
                submap: None,
            },
            Binding {
                chord: alt(VK_L),
                action: Action::EnterSubmap("mode_b".into()),
                submap: Some("mode_a".into()),
            },
        ]);

        assert_eq!(table.submap(), None);
        table.resolve(alt(VK_R));
        assert_eq!(table.submap(), Some("mode_a"));
        table.resolve(alt(VK_L));
        assert_eq!(table.submap(), Some("mode_b"));
    }

    #[test]
    fn serde_roundtrip_for_bind_table_and_types() {
        let chord = KeyChord::new(VK_H, true, true, false, true);
        let binding = Binding {
            chord,
            action: Action::ToggleFullscreen,
            submap: Some("special".into()),
        };
        let table = BindTable::new(vec![binding]);

        let json = serde_json::to_string(&table).unwrap();
        let back: BindTable = serde_json::from_str(&json).unwrap();
        assert_eq!(back, table);

        let res = Resolution::ModeChanged {
            submap: Some("test".into()),
        };
        let json_res = serde_json::to_string(&res).unwrap();
        let back_res: Resolution = serde_json::from_str(&json_res).unwrap();
        assert_eq!(back_res, res);
    }

    #[test]
    fn action_resolution_preserves_submap_control_isolation() {
        // Проверяем, что действия смены режима не порождают Fire(...)
        let mut table = BindTable::new(vec![
            Binding {
                chord: alt(VK_R),
                action: Action::EnterSubmap("custom".into()),
                submap: None,
            },
            Binding {
                chord: alt(VK_R),
                action: Action::LeaveSubmap,
                submap: Some("custom".into()),
            },
        ]);

        let res1 = table.resolve(alt(VK_R));
        assert!(matches!(res1, Resolution::ModeChanged { submap: Some(_) }));

        let res2 = table.resolve(alt(VK_R));
        assert!(matches!(res2, Resolution::ModeChanged { submap: None }));
    }
}
