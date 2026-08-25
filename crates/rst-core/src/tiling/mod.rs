//! M9 — движок раскладок тайлинга (docs/TILING_DESIGN.md).
//!
//! Платформенно-чисто: Win32 здесь нет ни строчки (CONTRIBUTING.md, «Правило
//! зависимостей»). Окно представлено непрозрачным ключом [`WindowKey`], а
//! координатор сам отображает его на `HWND`. Всё, что тут есть, — геометрия и
//! операции над деревом, покрытые юнит-тестами на любой ОС.
//!
//! Модель — дерево контейнеров в стиле i3/sway, а не BSP/dwindle
//! (docs/TILING_DESIGN.md §Р4): группа с табами в ней — обычный контейнер с
//! [`ContainerLayout::Tabbed`], то есть первоклассный узел, а не надстройка
//! сбоку. Dwindle и master-stack выражаются поверх той же модели политиками
//! вставки (`policy`), обратное неверно — выбрав BSP, табы пришлось бы
//! прикручивать отдельным механизмом.

pub mod action;
pub mod actions;
pub mod animation;
pub mod binds;
pub mod layout;
pub mod ops;
pub mod policy;
pub mod rebind;
pub mod reconcile;
pub mod rules;
pub mod scratchpad;
pub mod switcher;
pub mod tile_id;
pub mod tree;
pub mod workspace;

pub use action::{Action, ActionParseError};
pub use layout::{LayoutParams, Placement};
pub use ops::Direction;
pub use policy::{InsertPlan, InsertPolicy};
pub use rules::{RuleAction, RuleMatch, RuleOutcome, WindowFacts, WindowRule};
pub use tree::{ContainerLayout, InsertAt, Node, NodeId, NodeKind, Tree, TreeError, WindowKey};

// Функции модулей намеренно НЕ реэкспортируются: `tiling::layout::layout` и
// `tiling::ops::focus_direction` читаются на месте вызова лучше, чем голые
// `layout()` и `focus_direction()`, а имя `layout` иначе означало бы и модуль,
// и функцию сразу.
