//! Действия тайлинга: что именно происходит по горячей клавише
//! (M9, docs/TILING_DESIGN.md §T3).
//!
//! Отдельный маленький модуль по одной причине: [`Action`] — общий язык трёх
//! слоёв, которые пишутся независимо. Слой биндов (`binds`) превращает в него
//! нажатие клавиши, слой исполнения (`actions`) применяет его к воркспейсам,
//! конфиг хранит его в виде пары строк `action` + `arg`. Если бы enum жил в
//! любом из них, два других зависели бы от чужого модуля ради одного типа.
//!
//! Имена в [`Action::parse`] — те же строки, что лежат в `config.json`
//! (`TilingBinding::action`), и совпадают с названиями диспетчеров Hyprland
//! настолько, насколько это осмысленно: человеку, знающему Hyprland, не
//! придётся заново учить словарь.

use serde::{Deserialize, Serialize};

use super::ops::Direction;

/// Одно действие тайлинга.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Action {
    /// Перевести фокус в направлении.
    FocusDirection(Direction),
    /// Переставить сфокусированное окно в направлении.
    MoveDirection(Direction),
    /// Поменять сфокусированное окно местами с соседом.
    SwapDirection(Direction),
    /// Изменить размер сфокусированной плитки. `delta` — в долях контейнера.
    Resize { dir: Direction, delta: f64 },
    /// Сменить ориентацию контейнера фокуса: горизонталь ↔ вертикаль.
    ToggleSplit,
    /// Превратить контейнер фокуса в группу с табами и обратно.
    ToggleGroup,
    /// Следующий/предыдущий таб внутри группы.
    CycleGroup { forward: bool },
    /// Плавающее окно ↔ плитка.
    ToggleFloating,
    /// Развернуть сфокусированное окно на весь воркспейс и обратно.
    ToggleFullscreen,
    /// Закрыть сфокусированное окно.
    CloseWindow,
    /// Переключиться на воркспейс.
    Workspace(u8),
    /// Отправить сфокусированное окно на воркспейс.
    SendToWorkspace(u8),
    /// Войти в модальный режим биндов (submap Hyprland).
    EnterSubmap(String),
    /// Выйти из модального режима в обычный.
    LeaveSubmap,
}

/// Шаг ресайза по умолчанию, если в конфиге не указан.
///
/// 5% контейнера за нажатие: достаточно, чтобы почувствовать, и достаточно
/// мелко, чтобы дожатием попасть в нужную пропорцию.
pub const DEFAULT_RESIZE_DELTA: f64 = 0.05;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ActionParseError {
    #[error("неизвестное действие: {0}")]
    UnknownAction(String),
    #[error("действию {action} нужен аргумент {expected}")]
    MissingArg {
        action: &'static str,
        expected: &'static str,
    },
    #[error("непонятный аргумент {arg} у действия {action}")]
    BadArg { action: &'static str, arg: String },
}

impl Action {
    /// Разобрать пару строк из `config.json`.
    ///
    /// Ошибка, а не паника и не молчаливый пропуск: конфиг правит человек
    /// руками, опечатка в нём — норма, и она должна превращаться в понятную
    /// строчку в журнале, а не ронять разбор всего файла.
    pub fn parse(action: &str, arg: Option<&str>) -> Result<Self, ActionParseError> {
        match action {
            "focus_direction" => Ok(Self::FocusDirection(direction("focus_direction", arg)?)),
            "move_direction" => Ok(Self::MoveDirection(direction("move_direction", arg)?)),
            "swap_direction" => Ok(Self::SwapDirection(direction("swap_direction", arg)?)),
            // Аргумент ресайза — «направление» либо «направление:шаг»
            // (`right:0.1`). Шаг необязателен: без него берётся
            // [`DEFAULT_RESIZE_DELTA`].
            "resize" => {
                let raw = arg.ok_or(ActionParseError::MissingArg {
                    action: "resize",
                    expected: "направление (left/right/up/down)",
                })?;
                let (dir_part, delta) = match raw.split_once(':') {
                    Some((d, step)) => (
                        d,
                        step.parse::<f64>().map_err(|_| ActionParseError::BadArg {
                            action: "resize",
                            arg: raw.to_string(),
                        })?,
                    ),
                    None => (raw, DEFAULT_RESIZE_DELTA),
                };
                if !delta.is_finite() || delta == 0.0 {
                    return Err(ActionParseError::BadArg {
                        action: "resize",
                        arg: raw.to_string(),
                    });
                }
                Ok(Self::Resize {
                    dir: parse_direction("resize", dir_part)?,
                    delta,
                })
            }
            "toggle_split" => Ok(Self::ToggleSplit),
            "toggle_group" => Ok(Self::ToggleGroup),
            "cycle_group" => {
                // Без аргумента — вперёд: это то, чего ждут от одной клавиши.
                let forward = match arg {
                    None | Some("next") | Some("forward") => true,
                    Some("prev") | Some("previous") | Some("backward") => false,
                    Some(other) => {
                        return Err(ActionParseError::BadArg {
                            action: "cycle_group",
                            arg: other.to_string(),
                        });
                    }
                };
                Ok(Self::CycleGroup { forward })
            }
            "toggle_floating" => Ok(Self::ToggleFloating),
            "toggle_fullscreen" => Ok(Self::ToggleFullscreen),
            "close_window" => Ok(Self::CloseWindow),
            "workspace" => Ok(Self::Workspace(workspace_number("workspace", arg)?)),
            "send_to_workspace" => Ok(Self::SendToWorkspace(workspace_number(
                "send_to_workspace",
                arg,
            )?)),
            "enter_submap" => {
                let name = arg.ok_or(ActionParseError::MissingArg {
                    action: "enter_submap",
                    expected: "имя режима",
                })?;
                if name.is_empty() {
                    return Err(ActionParseError::BadArg {
                        action: "enter_submap",
                        arg: String::new(),
                    });
                }
                Ok(Self::EnterSubmap(name.to_string()))
            }
            "leave_submap" => Ok(Self::LeaveSubmap),
            other => Err(ActionParseError::UnknownAction(other.to_string())),
        }
    }

    /// Действие меняет ТОЛЬКО режим биндов, не трогая окна.
    ///
    /// Нужно исполняющему слою: такие действия обрабатывает автомат режимов,
    /// а до воркспейсов они не доходят вовсе.
    pub fn is_submap_control(&self) -> bool {
        matches!(self, Self::EnterSubmap(_) | Self::LeaveSubmap)
    }
}

fn direction(action: &'static str, arg: Option<&str>) -> Result<Direction, ActionParseError> {
    let raw = arg.ok_or(ActionParseError::MissingArg {
        action,
        expected: "направление (left/right/up/down)",
    })?;
    parse_direction(action, raw)
}

fn parse_direction(action: &'static str, raw: &str) -> Result<Direction, ActionParseError> {
    // Буквы hjkl приняты наравне со словами: так пишут бинды и в i3, и в
    // Hyprland, и переучиваться ради нас никто не станет. Именно hjkl, а не
    // «первая буква слова»: `l` в этом словаре — right, и если принять его
    // ещё и за left, бинд будет означать разное у разных людей.
    match raw.trim().to_ascii_lowercase().as_str() {
        "left" | "h" => Ok(Direction::Left),
        "right" | "l" => Ok(Direction::Right),
        "up" | "k" => Ok(Direction::Up),
        "down" | "j" => Ok(Direction::Down),
        other => Err(ActionParseError::BadArg {
            action,
            arg: other.to_string(),
        }),
    }
}

fn workspace_number(action: &'static str, arg: Option<&str>) -> Result<u8, ActionParseError> {
    let raw = arg.ok_or(ActionParseError::MissingArg {
        action,
        expected: "номер воркспейса",
    })?;
    raw.trim()
        .parse::<u8>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or(ActionParseError::BadArg {
            action,
            arg: raw.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directional_actions_parse_words() {
        assert_eq!(
            Action::parse("focus_direction", Some("left")).unwrap(),
            Action::FocusDirection(Direction::Left)
        );
        assert_eq!(
            Action::parse("move_direction", Some("down")).unwrap(),
            Action::MoveDirection(Direction::Down)
        );
        assert_eq!(
            Action::parse("swap_direction", Some("right")).unwrap(),
            Action::SwapDirection(Direction::Right)
        );
    }

    #[test]
    fn hjkl_letters_work_like_words() {
        // Так пишут бинды в i3 и Hyprland; заставлять переучиваться незачем.
        for (letter, expected) in [
            ("h", Direction::Left),
            ("j", Direction::Down),
            ("k", Direction::Up),
            ("l", Direction::Right),
        ] {
            let parsed = Action::parse("focus_direction", Some(letter)).unwrap();
            assert_eq!(parsed, Action::FocusDirection(expected), "буква {letter}");
        }
    }

    #[test]
    fn direction_is_case_insensitive_and_trimmed() {
        assert_eq!(
            Action::parse("focus_direction", Some("  UP ")).unwrap(),
            Action::FocusDirection(Direction::Up)
        );
    }

    #[test]
    fn a_directional_action_without_an_argument_is_an_error() {
        assert!(matches!(
            Action::parse("focus_direction", None),
            Err(ActionParseError::MissingArg { .. })
        ));
    }

    #[test]
    fn resize_without_a_step_uses_the_default() {
        assert_eq!(
            Action::parse("resize", Some("right")).unwrap(),
            Action::Resize {
                dir: Direction::Right,
                delta: DEFAULT_RESIZE_DELTA
            }
        );
    }

    #[test]
    fn resize_accepts_an_explicit_step() {
        assert_eq!(
            Action::parse("resize", Some("left:0.1")).unwrap(),
            Action::Resize {
                dir: Direction::Left,
                delta: 0.1
            }
        );
    }

    #[test]
    fn a_zero_or_broken_resize_step_is_refused() {
        // Нулевой шаг — бинд, который ничего не делает; молча принять его
        // значило бы отдать пользователю неработающую клавишу без объяснений.
        assert!(Action::parse("resize", Some("left:0")).is_err());
        assert!(Action::parse("resize", Some("left:abc")).is_err());
    }

    #[test]
    fn cycle_group_defaults_to_forward() {
        assert_eq!(
            Action::parse("cycle_group", None).unwrap(),
            Action::CycleGroup { forward: true }
        );
        assert_eq!(
            Action::parse("cycle_group", Some("prev")).unwrap(),
            Action::CycleGroup { forward: false }
        );
    }

    #[test]
    fn workspace_numbers_start_at_one() {
        assert_eq!(
            Action::parse("workspace", Some("3")).unwrap(),
            Action::Workspace(3)
        );
        assert!(
            Action::parse("workspace", Some("0")).is_err(),
            "нулевого воркспейса не бывает — пользователи считают с единицы"
        );
        assert!(Action::parse("workspace", Some("нет")).is_err());
    }

    #[test]
    fn send_to_workspace_shares_the_number_rules() {
        assert_eq!(
            Action::parse("send_to_workspace", Some("9")).unwrap(),
            Action::SendToWorkspace(9)
        );
        assert!(Action::parse("send_to_workspace", None).is_err());
    }

    #[test]
    fn argumentless_actions_parse() {
        for (name, expected) in [
            ("toggle_split", Action::ToggleSplit),
            ("toggle_group", Action::ToggleGroup),
            ("toggle_floating", Action::ToggleFloating),
            ("toggle_fullscreen", Action::ToggleFullscreen),
            ("close_window", Action::CloseWindow),
            ("leave_submap", Action::LeaveSubmap),
        ] {
            assert_eq!(Action::parse(name, None).unwrap(), expected, "{name}");
        }
    }

    #[test]
    fn entering_a_submap_needs_a_name() {
        assert_eq!(
            Action::parse("enter_submap", Some("resize")).unwrap(),
            Action::EnterSubmap("resize".to_string())
        );
        assert!(Action::parse("enter_submap", None).is_err());
        assert!(Action::parse("enter_submap", Some("")).is_err());
    }

    #[test]
    fn an_unknown_action_names_itself_in_the_error() {
        let err = Action::parse("do_a_barrel_roll", None).unwrap_err();
        assert_eq!(
            err,
            ActionParseError::UnknownAction("do_a_barrel_roll".to_string())
        );
        // Сообщение должно быть читаемым: его увидит человек в журнале.
        assert!(err.to_string().contains("do_a_barrel_roll"));
    }

    #[test]
    fn only_submap_actions_are_submap_control() {
        assert!(Action::LeaveSubmap.is_submap_control());
        assert!(Action::EnterSubmap("x".into()).is_submap_control());
        assert!(!Action::ToggleSplit.is_submap_control());
        assert!(!Action::Workspace(1).is_submap_control());
    }

    #[test]
    fn actions_survive_a_serde_roundtrip() {
        let all = vec![
            Action::FocusDirection(Direction::Left),
            Action::Resize {
                dir: Direction::Up,
                delta: 0.2,
            },
            Action::EnterSubmap("resize".to_string()),
            Action::Workspace(7),
        ];
        let json = serde_json::to_string(&all).unwrap();
        let back: Vec<Action> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, all);
    }
}
