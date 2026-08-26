//! Видимость группы окон: чистая машина состояний (запрос пользователя
//! 2026-08-26, docs/research/hotkeys — та же тема «хоткей молча не доходит»).
//!
//! Приём тот же, что у [`crate::pinned_window::host_action`]: на вход —
//! событие и факты о мире, на выход — решения по каждому окну. Win32 здесь
//! нет вообще: окна — числовые идентификаторы, «поднять поверх всего» —
//! просто значение в enum, а чем его выполнять, решает координатор.
//!
//! Ключевая идея машины — состояние группы `shown`/`hidden` отдельно от
//! фактической видимости окон. Без него нельзя отличить «группа показана»
//! от «группа спрятана, но одно окно пользователь открыл сам» — а хоткей
//! обязан вести себя в этих двух случаях по-разному (правило 6).

/// Числовой идентификатор окна. `HWND` в Win32 — просто число, и в чистом
/// крейте ему делать нечего (CONTRIBUTING.md, «Правило зависимостей»):
/// конвертация на границе `rst-win32`.
pub type WindowId = u64;

/// Событие, на которое машина отвечает одним списком решений.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupVisibilityEvent {
    /// Нажат хоткей группы — переключатель «показать/спрятать» (правило 2).
    HotkeyPressed,
    /// Нажат переключатель закрепления всей группы поверх всех окон
    /// (Ctrl+Alt+Shift+T): закрепить всех членов / снять закрепление.
    /// Закрепление и показ — разные вещи: `shown` этим событием не
    /// меняется, окна не прячутся и не поднимаются.
    PinTogglePressed,
    /// Сменилось окно переднего плана (Alt+Tab, клик по другому окну,
    /// рабочий стол). Кто теперь в фокусе, машина узнаёт из
    /// [`MemberFacts::is_foreground`] членов.
    ForegroundChanged,
    /// Окно группы закрылось. Решения по закрытому окну не выдаются вовсе —
    /// его больше нет; остальных это событие не касается.
    WindowClosed(WindowId),
}

/// Факты о члене группы на момент события.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemberFacts {
    /// Числовой идентификатор окна — им же подписываются решения.
    pub id: WindowId,
    /// Закреплено пользователем как «поверх всех» (правило 4): его прямое
    /// назначение — постоянная видимость, и группа его не прячет.
    pub pinned: bool,
    /// Есть правила соседства — окно закреплено «между окнами» и живёт по
    /// своей машине соседства ([`crate::pinned_window::host_action`],
    /// правило 7). В `pinned_window` эти два режима взаимоисключающие
    /// ([`crate::pinned_window::HostFilter`]); если факты противоречат,
    /// приоритет у правил соседства — это более сильное ограничение.
    pub has_host_rules: bool,
    /// Окно сейчас на экране (не свёрнуто).
    pub visible: bool,
    /// Окно сейчас в фокусе — на него пришёлся Alt+Tab или клик
    /// (правило 5). У нескольких членов одновременно не бывает.
    pub is_foreground: bool,
}

/// Решение по одному окну.
///
/// «Не трогать» — отдельное значение, а не «спрятать уже спрятанное»:
/// молча трогать чужое окно, когда этого не требуется, — худшее, что может
/// сделать эта программа (окно может быть свёрнуто самим пользователем —
/// его выбор не оспаривается, тот же принцип, что в `host_action`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowAction {
    /// Показать поверх всего — поднять над полноэкранными окнами
    /// (правило 2: группа показывается именно так).
    ShowTopmost,
    /// Показать обычным окном — без topmost и без остальной группы
    /// (правило 5: Alt+Tab на окно группы поднимает именно его).
    ShowNormal,
    /// Вернуть на место между окнами: снять с topmost и передать обратно
    /// машине соседства (правило 7: окно с правилами не исчезает совсем).
    RestoreBetweenWindows,
    /// Закрепить окно поверх всех — установить постоянный topmost-стиль
    /// окна. Отличие от [`WindowAction::ShowTopmost`]: тот — разовый подъём
    /// наверх, который группа сама же отыгрывает при сокрытии и уходе на
    /// постороннее окно; закрепление — постоянное свойство окна, которое
    /// переживает переключение на другое приложение и снимается только
    /// явной командой (переключатель группы или пользователь).
    PinTopmost,
    /// Снять закрепление поверх всех: окно остаётся на экране, но
    /// возвращается в обычный z-порядок. Отличие от [`WindowAction::Hide`]:
    /// окно НЕ прячется — снимается только стиль «поверх всего».
    UnpinTopmost,
    /// Спрятать (свернуть) окно.
    Hide,
    /// Не трогать: состояние окна уже соответствует решению, либо судьбой
    /// окна заведует что-то другое (пользователь, машина соседства).
    None,
}

/// Решение машины по конкретному окну группы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowDecision {
    pub window: WindowId,
    pub action: WindowAction,
}

/// Состояние группы: показана или спрятана (правило 1).
///
/// Состояние НЕ равно «все окна видимы/скрыты»: пока группа спрятана,
/// пользователь может сам открыть одно её окно (правило 6), и тогда часть
/// окон видима при спрятанной группе. Именно поэтому состояние ведётся
/// отдельно и переживает расхождение с фактами.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupVisibilityState {
    /// Группа показана: её окна подняты группой (а не открыты пользователем
    /// вручную) и будут спрятаны при уходе на постороннее окно.
    pub shown: bool,
    /// Группа закреплена целиком поверх всех: её окнам по команде группы
    /// выставлен постоянный topmost-стиль (переключатель Ctrl+Alt+Shift+T).
    /// Независимо от `shown` — закреплённая группа может быть спрятана:
    /// окна свёрнуты, но при следующем показе останутся поверх всего.
    /// Факт `MemberFacts::pinned`, который координатор передаёт на каждое
    /// событие, покрывает и личное закрепление окна пользователем, и
    /// закрепление группой — машина их не различает: для всех её правил
    /// важен только итоговый стиль окна.
    pub pinned: bool,
}

impl Default for GroupVisibilityState {
    /// Свежая группа (после перезапуска программы) — спрятана и не
    /// закреплена: никто её не показывал и не закреплял, и притворяться
    /// нельзя.
    fn default() -> Self {
        Self {
            shown: false,
            pinned: false,
        }
    }
}

/// Ответ машины на событие: новое состояние группы и решения по окнам.
///
/// Вызывается координатором на каждое событие с актуальными фактами.
/// Вход вырожденный (пустая группа, все окна закрыты) переживается без
/// паники: решения пусты, состояние меняется по тем же правилам.
pub fn decide_visibility(
    event: GroupVisibilityEvent,
    state: GroupVisibilityState,
    members: &[MemberFacts],
) -> (GroupVisibilityState, Vec<WindowDecision>) {
    match event {
        GroupVisibilityEvent::HotkeyPressed => {
            // Переключатель (правило 2): показана → спрятать, спрятана →
            // показать. Состояние переключается всегда, даже если окон
            // больше нет, — переключатель честный и предсказуемый.
            let next = GroupVisibilityState {
                shown: !state.shown,
                ..state
            };
            let decisions = hotkey_decisions(members, state);
            (next, decisions)
        }
        GroupVisibilityEvent::PinTogglePressed => {
            // Переключатель закрепления: не закреплена → закрепить всех
            // членов, закреплена → снять со всех. Показ/сокрытие этим
            // событием не трогается — закрепление и видимость разные вещи.
            let next = GroupVisibilityState {
                pinned: !state.pinned,
                ..state
            };
            let decisions = pin_toggle_decisions(members, state);
            (next, decisions)
        }
        GroupVisibilityEvent::ForegroundChanged => {
            if members.iter().any(|m| m.is_foreground) {
                // Alt+Tab пришёлся НА окно группы (правило 5): поднял его
                // сам переключатель, группу целиком не показываем и
                // состояние не трогаем.
                let decisions = member_foreground_decisions(members);
                (state, decisions)
            } else {
                // Фокус на окне ВНЕ группы (правило 3): группа прячется.
                let next = GroupVisibilityState {
                    shown: false,
                    ..state
                };
                let decisions = foreign_foreground_decisions(members, state);
                (next, decisions)
            }
        }
        GroupVisibilityEvent::WindowClosed(id) => {
            // Закрытое окно уже не существует — решений по нему нет, а
            // остальным это событие ничего не говорит: их видимость
            // определяют другие события.
            let decisions = members
                .iter()
                .filter(|m| m.id != id)
                .map(|m| WindowDecision {
                    window: m.id,
                    action: WindowAction::None,
                })
                .collect();
            (state, decisions)
        }
    }
}

/// Решения по хоткею: каждое окно приводится к желаемой видимости, но
/// только если фактическое состояние отличается — уже показанное не
/// трогаем (правило 6 «уже показанное не трогаем»), уже спрятанное тоже.
///
/// Хоткей — хозяин группы, и он НЕ щадит закреплённые окна: «по повторному
/// нажатию вся группа прячется, включая закреплённые» (запрос пользователя
/// 2026-08-26). Различие проходит по событию, а не по типу окна: уход на
/// постороннее окно ([`foreign_foreground_decisions`]) закреплённое щадит —
/// там его прямое назначение «оставаться поверх всего» вступает в силу;
/// здесь же пользователь явно командует именно этой группой, и команда
/// сильнее стиля отдельного окна.
fn hotkey_decisions(members: &[MemberFacts], state: GroupVisibilityState) -> Vec<WindowDecision> {
    let want_visible = !state.shown;
    members
        .iter()
        .map(|m| {
            let action = if m.has_host_rules {
                // Окно «между окнами» (правило 7): при показе группы
                // поднимается со всеми; при сокрытии НЕ прячется, а
                // возвращается на своё место — Hide сломал бы машину
                // соседства, которая сама решает его видимость.
                //
                // Ветка стоит ПЕРВОЙ намеренно: если факты противоречат и
                // окно одновременно закреплено «поверх всех», приоритет у
                // правил соседства — это более сильное ограничение
                // (доккомент [`MemberFacts::has_host_rules`]).
                if want_visible && !m.visible {
                    WindowAction::ShowTopmost
                } else if !want_visible && m.visible {
                    WindowAction::RestoreBetweenWindows
                } else {
                    WindowAction::None
                }
            } else if m.pinned {
                // Закреплённое «поверх всех»: хоткей прячет его вместе со
                // всеми (см. докфункцию). Показ касается его только когда
                // оно невидимо (пользователь свернул его вручную) — хоткей
                // возвращает его вместе со всеми.
                if want_visible && !m.visible {
                    WindowAction::ShowTopmost
                } else if !want_visible && m.visible {
                    WindowAction::Hide
                } else {
                    WindowAction::None
                }
            } else if want_visible && !m.visible {
                WindowAction::ShowTopmost
            } else if !want_visible && m.visible {
                WindowAction::Hide
            } else {
                WindowAction::None
            };
            WindowDecision {
                window: m.id,
                action,
            }
        })
        .collect()
}

/// Решения по переключателю закрепления группы: каждый член приводится к
/// желаемому стилю, но только если фактический стиль отличается — окно,
/// которое и так в нужном состоянии, не трогаем (тот же принцип «не
/// дёргать зря», что в [`hotkey_decisions`]).
///
/// Видимость этими решениями не меняется: закрепление — свойство окна,
/// а не команда показа/сокрытия. Свёрнутое окно тоже получает решение —
/// при следующем показе оно уже будет поверх всего.
fn pin_toggle_decisions(
    members: &[MemberFacts],
    state: GroupVisibilityState,
) -> Vec<WindowDecision> {
    let want_pinned = !state.pinned;
    members
        .iter()
        .map(|m| {
            let action = if want_pinned && !m.pinned {
                WindowAction::PinTopmost
            } else if !want_pinned && m.pinned {
                WindowAction::UnpinTopmost
            } else {
                WindowAction::None
            };
            WindowDecision {
                window: m.id,
                action,
            }
        })
        .collect()
}

/// Решения, когда фокус ушёл на окно вне группы (правила 3, 4, 7).
fn foreign_foreground_decisions(
    members: &[MemberFacts],
    state: GroupVisibilityState,
) -> Vec<WindowDecision> {
    if !state.shown {
        // Группа уже спрятана: всё, что видно, открыл сам пользователь
        // (правило 6) или машина соседства — с его выбором не спорим, как
        // обычное окно оно и остаётся.
        return members
            .iter()
            .map(|m| WindowDecision {
                window: m.id,
                action: WindowAction::None,
            })
            .collect();
    }
    members
        .iter()
        .map(|m| {
            let action = if m.pinned {
                // Правило 4: закреплённое «поверх всех» от Alt+Tab не
                // прячется — это его прямое назначение.
                WindowAction::None
            } else if m.has_host_rules {
                // Правило 7: окно с правилами соседства не исчезает
                // совсем, а возвращается на своё место между окнами.
                if m.visible {
                    WindowAction::RestoreBetweenWindows
                } else {
                    WindowAction::None
                }
            } else if m.visible {
                // Правило 3: обычное окно группы прячется.
                WindowAction::Hide
            } else {
                WindowAction::None
            };
            WindowDecision {
                window: m.id,
                action,
            }
        })
        .collect()
}

/// Решения, когда Alt+Tab пришёлся на окно группы (правило 5).
fn member_foreground_decisions(members: &[MemberFacts]) -> Vec<WindowDecision> {
    members
        .iter()
        .map(|m| {
            let action = if m.visible || !m.is_foreground {
                // Alt+Tab уже поднял фокусное окно, а остальные окна группы
                // он не касался — трогать их нельзя, «поднимается именно
                // оно» (правило 5).
                WindowAction::None
            } else {
                // Страховка: фокусное окно почему-то осталось свёрнутым
                // (Alt+Tab на сворачиваемые нами окна срабатывает не
                // всегда) — показать его обычным окном, группу целиком
                // не поднимая.
                WindowAction::ShowNormal
            };
            WindowDecision {
                window: m.id,
                action,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Член группы по умолчанию: обычное окно, не закреплено, не видимо,
    /// не в фокусе.
    fn member(id: u64) -> MemberFacts {
        MemberFacts {
            id,
            pinned: false,
            has_host_rules: false,
            visible: false,
            is_foreground: false,
        }
    }

    fn actions(decisions: &[WindowDecision]) -> WindowAction {
        assert_eq!(decisions.len(), 1, "тесты ниже рассчитаны на одно окно");
        decisions[0].action
    }

    // --- правило 1: состояние группы ---

    /// Группа имеет ровно два состояния — показана и спрятана, и хоткей
    /// переключает их (правило 1).
    #[test]
    fn hotkey_toggles_group_between_shown_and_hidden() {
        let hidden = GroupVisibilityState::default();
        let (shown, _) = decide_visibility(GroupVisibilityEvent::HotkeyPressed, hidden, &[]);
        assert!(
            shown.shown,
            "хоткей на спрятанной группе обязан показать её"
        );
        let (hidden_again, _) = decide_visibility(GroupVisibilityEvent::HotkeyPressed, shown, &[]);
        assert!(
            !hidden_again.shown,
            "хоткей на показанной группе обязан спрятать её"
        );
    }

    // --- правило 2: хоткей показывает/прячет все окна ---

    /// Показ группы поднимает все свёрнутые окна поверх всего — даже те,
    /// что свёрнуты уже давно (правило 2).
    #[test]
    fn hotkey_show_raises_all_minimized_members_topmost() {
        let a = member(1);
        let b = member(2);
        let (state, decisions) = decide_visibility(
            GroupVisibilityEvent::HotkeyPressed,
            GroupVisibilityState::default(),
            &[a, b],
        );
        assert!(state.shown);
        assert_eq!(
            decisions,
            vec![
                WindowDecision {
                    window: 1,
                    action: WindowAction::ShowTopmost,
                },
                WindowDecision {
                    window: 2,
                    action: WindowAction::ShowTopmost,
                },
            ],
            "обе свёрнутые окна обязаны подняться поверх всего"
        );
    }

    /// Сокрытие группы сворачивает все показанные окна (правило 2),
    /// а уже свёрнутые не трогает: «спрятать» для спрятанного — шум.
    #[test]
    fn hotkey_hide_collapses_visible_members_and_ignores_hidden_ones() {
        let mut a = member(1);
        a.visible = true;
        let b = member(2); // уже свёрнуто
        let (state, decisions) = decide_visibility(
            GroupVisibilityEvent::HotkeyPressed,
            GroupVisibilityState {
                shown: true,
                pinned: false,
            },
            &[a, b],
        );
        assert!(!state.shown);
        assert_eq!(
            decisions,
            vec![
                WindowDecision {
                    window: 1,
                    action: WindowAction::Hide,
                },
                WindowDecision {
                    window: 2,
                    action: WindowAction::None,
                },
            ],
            "прячется ровно видимое; свёрнутое не дёргается"
        );
    }

    // --- правило 3: уход на постороннее окно прячет группу ---

    /// Alt+Tab на окно вне группы сворачивает показанные обычные окна
    /// группы (правило 3).
    #[test]
    fn switching_to_foreign_window_hides_unpinned_members() {
        let mut a = member(1);
        a.visible = true;
        let (state, decisions) = decide_visibility(
            GroupVisibilityEvent::ForegroundChanged,
            GroupVisibilityState {
                shown: true,
                pinned: false,
            },
            &[a],
        );
        assert!(!state.shown, "группа обязана перейти в спрятанную");
        assert_eq!(
            actions(&decisions),
            WindowAction::Hide,
            "обычное окно показанной группы прячется при уходе"
        );
    }

    /// Уже спрятанная группа при уходе на постороннее окно ничего не
    /// делает: видимое в ней — открыто пользователем, и спорить с ним
    /// нельзя (правило 3 буквально про «группа прячется», но прятать
    /// уже нечего).
    #[test]
    fn foreign_switch_does_not_touch_manually_opened_window_of_hidden_group() {
        let mut a = member(1);
        a.visible = true; // открыто пользователем вручную, группа спрятана
        let (state, decisions) = decide_visibility(
            GroupVisibilityEvent::ForegroundChanged,
            GroupVisibilityState::default(),
            &[a],
        );
        assert!(!state.shown);
        assert_eq!(
            actions(&decisions),
            WindowAction::None,
            "вручную открытое окно ведёт себя как обычное и остаётся на экране"
        );
    }

    // --- правило 4: закреплённое «поверх всех» не прячется ---

    /// Окно, закреплённое пользователем как «поверх всех», переживает
    /// Alt+Tab на постороннее окно: постоянная видимость — его прямое
    /// назначение (правило 4).
    #[test]
    fn user_pinned_window_survives_switch_to_foreign_window() {
        let mut a = member(1);
        a.pinned = true;
        a.visible = true;
        let (_, decisions) = decide_visibility(
            GroupVisibilityEvent::ForegroundChanged,
            GroupVisibilityState {
                shown: true,
                pinned: false,
            },
            &[a],
        );
        assert_eq!(
            actions(&decisions),
            WindowAction::None,
            "закреплённое окно не прячется при уходе"
        );
    }

    /// Закреплённое окно прячет и хоткей-сокрытие: хоткей — хозяин группы,
    /// и команда «спрятать группу» сильнее стиля отдельного окна (запрос
    /// пользователя 2026-08-26: «по повторному нажатию вся группа прячется,
    /// включая закреплённые»). Щадит закреплённое только уход на постороннее
    /// окно — там его прямое назначение, см.
    /// `user_pinned_window_survives_switch_to_foreign_window`.
    #[test]
    fn hotkey_hide_collapses_user_pinned_window_too() {
        let mut a = member(1);
        a.pinned = true;
        a.visible = true;
        let (_, decisions) = decide_visibility(
            GroupVisibilityEvent::HotkeyPressed,
            GroupVisibilityState {
                shown: true,
                pinned: false,
            },
            &[a],
        );
        assert_eq!(
            actions(&decisions),
            WindowAction::Hide,
            "хоткей-сокрытие сворачивает и закреплённое окно"
        );
    }

    /// Показанный группой хоткей возвращает и закреплённое окно, если
    /// пользователь свернул его вручную: группа показывается целиком.
    #[test]
    fn hotkey_show_returns_user_pinned_window_that_was_minimized_manually() {
        let mut a = member(1);
        a.pinned = true;
        let (_, decisions) = decide_visibility(
            GroupVisibilityEvent::HotkeyPressed,
            GroupVisibilityState::default(),
            &[a],
        );
        assert_eq!(
            actions(&decisions),
            WindowAction::ShowTopmost,
            "закреплённое окно участвует в показе группы"
        );
    }

    // --- правило 5: Alt+Tab на окно группы поднимает именно его ---

    /// Alt+Tab на видимое окно группы не трогает ни его, ни остальные
    /// окна и не показывает группу целиком (правило 5).
    #[test]
    fn alt_tab_to_group_member_leaves_group_hidden_and_touches_nothing() {
        let mut a = member(1);
        a.is_foreground = true;
        a.visible = true;
        let mut b = member(2);
        b.visible = false;
        let (state, decisions) = decide_visibility(
            GroupVisibilityEvent::ForegroundChanged,
            GroupVisibilityState::default(), // группа спрятана
            &[a, b],
        );
        assert!(!state.shown, "группа целиком при этом не показывается");
        assert_eq!(
            decisions,
            vec![
                WindowDecision {
                    window: 1,
                    action: WindowAction::None,
                },
                WindowDecision {
                    window: 2,
                    action: WindowAction::None,
                },
            ],
            "Alt+Tab поднял окно сам, остальным ничего не нужно"
        );
    }

    /// Alt+Tab на свёрнутое окно группы, которое переключатель почему-то
    /// не развернул, — показываем его обычным окном, без подъёма группы
    /// (правило 5, страховка).
    #[test]
    fn alt_tab_to_minimized_group_member_restores_it_as_plain_window() {
        let mut a = member(1);
        a.is_foreground = true;
        let (state, decisions) = decide_visibility(
            GroupVisibilityEvent::ForegroundChanged,
            GroupVisibilityState::default(),
            &[a],
        );
        assert!(!state.shown);
        assert_eq!(
            actions(&decisions),
            WindowAction::ShowNormal,
            "свёрнутое фокусное окно показывается обычным, без topmost"
        );
    }

    // --- правило 6: частично показанная группа ---

    /// Полный сценарий правила 6: группа спрятана, одно окно открыто
    /// пользователем вручную. Первый хоткей показывает только остальные
    /// окна (уже показанное не трогает), второй — прячет все.
    #[test]
    fn hotkey_after_manual_window_opens_the_rest_then_next_hotkey_hides_everything() {
        let mut a = member(1);
        a.visible = true; // открыто пользователем вручную
        let b = member(2); // свёрнуто
        let hidden = GroupVisibilityState::default();

        let (shown, decisions) =
            decide_visibility(GroupVisibilityEvent::HotkeyPressed, hidden, &[a, b]);
        assert!(shown.shown);
        assert_eq!(
            decisions,
            vec![
                WindowDecision {
                    window: 1,
                    action: WindowAction::None,
                },
                WindowDecision {
                    window: 2,
                    action: WindowAction::ShowTopmost,
                },
            ],
            "первый хоткей показывает остальные, уже показанное не трогая"
        );

        // На втором нажатии оба окна видимы — прячем всё.
        a.visible = true;
        let mut b2 = b;
        b2.visible = true;
        let (hidden_again, decisions) =
            decide_visibility(GroupVisibilityEvent::HotkeyPressed, shown, &[a, b2]);
        assert!(!hidden_again.shown);
        assert_eq!(
            decisions,
            vec![
                WindowDecision {
                    window: 1,
                    action: WindowAction::Hide,
                },
                WindowDecision {
                    window: 2,
                    action: WindowAction::Hide,
                },
            ],
            "второй хоткей прячет все окна, включая открытое вручную"
        );
    }

    // --- правило 7: окно «между окнами» не исчезает, а возвращается ---

    /// Окно с правилами соседства при уходе на постороннее окно получает
    /// «вернуть на место между окнами», а не «спрятать» (правило 7):
    /// его видимостью заведует машина соседства, и Hide её сломал бы.
    #[test]
    fn window_with_host_rules_returns_between_windows_instead_of_hiding() {
        let mut a = member(1);
        a.has_host_rules = true;
        a.visible = true;
        let (_, decisions) = decide_visibility(
            GroupVisibilityEvent::ForegroundChanged,
            GroupVisibilityState {
                shown: true,
                pinned: false,
            },
            &[a],
        );
        assert_eq!(
            actions(&decisions),
            WindowAction::RestoreBetweenWindows,
            "окно с правилами не прячется совсем, а возвращается на место"
        );
    }

    /// Окно с правилами соседства участвует в показе группы вместе со
    /// всеми: правило 2 требует поднять все окна, и «между окнами» —
    /// его состояние ПОСЛЕ показа, а не ограничение на участие в нём.
    #[test]
    fn window_with_host_rules_is_raised_with_the_group_on_show() {
        let mut a = member(1);
        a.has_host_rules = true;
        let (_, decisions) = decide_visibility(
            GroupVisibilityEvent::HotkeyPressed,
            GroupVisibilityState::default(),
            &[a],
        );
        assert_eq!(
            actions(&decisions),
            WindowAction::ShowTopmost,
            "показ группы поднимает и окно с правилами"
        );
    }

    /// Хоткей-сокрытие для окна с правилами — то же «вернуть на место»,
    /// а не сворачивание: сокрытие группы снимает его с topmost и отдаёт
    /// машине соседства.
    #[test]
    fn hotkey_hide_returns_host_rules_window_between_windows() {
        let mut a = member(1);
        a.has_host_rules = true;
        a.visible = true;
        let (_, decisions) = decide_visibility(
            GroupVisibilityEvent::HotkeyPressed,
            GroupVisibilityState {
                shown: true,
                pinned: false,
            },
            &[a],
        );
        assert_eq!(
            actions(&decisions),
            WindowAction::RestoreBetweenWindows,
            "сокрытие не сворачивает окно с правилами"
        );
    }

    // --- событие закрытия окна ---

    /// Закрытие окна группы не трогает остальные: их видимость решают
    /// другие события, а закрытому окну решений не выдаётся вовсе.
    #[test]
    fn closed_window_gets_no_decision_and_others_stay_untouched() {
        let mut a = member(1);
        a.visible = true;
        let mut b = member(2);
        b.visible = true;
        let (state, decisions) = decide_visibility(
            GroupVisibilityEvent::WindowClosed(1),
            GroupVisibilityState {
                shown: true,
                pinned: false,
            },
            &[a, b],
        );
        assert!(state.shown, "закрытие окна не меняет состояние группы");
        assert_eq!(
            decisions,
            vec![WindowDecision {
                window: 2,
                action: WindowAction::None,
            }],
            "закрытое окно не получает решений, остальные не трогаются"
        );
    }

    // --- вырожденные входы ---

    /// Пустая группа не паникует и возвращает пустой список решений на
    /// любое событие; хоткей при этом честно переключает состояние.
    #[test]
    fn empty_group_never_panics_and_returns_no_decisions() {
        let hidden = GroupVisibilityState::default();
        let (shown, decisions) =
            decide_visibility(GroupVisibilityEvent::HotkeyPressed, hidden, &[]);
        assert!(shown.shown);
        assert!(decisions.is_empty());
        let (hidden_again, decisions) =
            decide_visibility(GroupVisibilityEvent::HotkeyPressed, shown, &[]);
        assert!(!hidden_again.shown);
        assert!(decisions.is_empty());
        let (same, decisions) =
            decide_visibility(GroupVisibilityEvent::ForegroundChanged, hidden, &[]);
        assert_eq!(same, hidden);
        assert!(decisions.is_empty());
        let (same, decisions) =
            decide_visibility(GroupVisibilityEvent::WindowClosed(1), hidden, &[]);
        assert_eq!(same, hidden);
        assert!(decisions.is_empty());
    }

    /// Все окна закрылись — группа пуста, но машина остаётся согласованной:
    /// следующий хоткей не паникует и состояние переключается как обычно.
    #[test]
    fn group_survives_all_windows_closing() {
        let a = member(1);
        let b = member(2);
        let (state, decisions) = decide_visibility(
            GroupVisibilityEvent::WindowClosed(1),
            GroupVisibilityState {
                shown: true,
                pinned: false,
            },
            &[a, b],
        );
        assert_eq!(
            decisions,
            vec![WindowDecision {
                window: 2,
                action: WindowAction::None,
            }]
        );
        // Второе окно закрылось — группа пуста, состояние остаётся shown.
        let (state, decisions) =
            decide_visibility(GroupVisibilityEvent::WindowClosed(2), state, &[]);
        assert!(state.shown);
        assert!(decisions.is_empty());
        // Хоткей на пустую показанную группу прячет её без паники.
        let (state, decisions) = decide_visibility(GroupVisibilityEvent::HotkeyPressed, state, &[]);
        assert!(!state.shown);
        assert!(decisions.is_empty());
    }

    // --- ошибка трактовки правила: хоткей прячет закреплённые ---

    /// Хоткей-сокрытие прячет закреплённое окно, даже если пользователь
    /// открыл его вручную: правило 6 (уже показанное не трогаем при ПОКАЗЕ)
    /// не отменяет хозяина группы при СКРЫТИИ.
    #[test]
    fn hotkey_after_manual_show_hides_pinned_window_opened_by_user() {
        let mut a = member(1);
        a.pinned = true;
        a.visible = true; // открыто пользователем вручную, группа спрятана
        let hidden = GroupVisibilityState::default();

        // Первый хоткей: показывать нечего — окно уже видимо.
        let (shown, decisions) =
            decide_visibility(GroupVisibilityEvent::HotkeyPressed, hidden, &[a]);
        assert!(shown.shown);
        assert_eq!(
            actions(&decisions),
            WindowAction::None,
            "уже показанное не трогаем при показе группы"
        );

        // Второй хоткей: прячем всё, включая закреплённое.
        let (hidden_again, decisions) =
            decide_visibility(GroupVisibilityEvent::HotkeyPressed, shown, &[a]);
        assert!(!hidden_again.shown);
        assert_eq!(
            actions(&decisions),
            WindowAction::Hide,
            "хоткей-сокрытие сворачивает закреплённое окно"
        );
    }

    /// Окно одновременно закреплённое и с правилами соседства: приоритет у
    /// правил — ветка `has_host_rules` стоит первой и сокрытие возвращает
    /// окно между окнами, а не сворачивает его (доккомент
    /// [`MemberFacts::has_host_rules`]).
    #[test]
    fn hotkey_hide_prefers_host_rules_over_conflicting_pinned_fact() {
        let mut a = member(1);
        a.pinned = true; // факты противоречат — окно «и поверх всех, и между»
        a.has_host_rules = true;
        a.visible = true;
        let (_, decisions) = decide_visibility(
            GroupVisibilityEvent::HotkeyPressed,
            GroupVisibilityState {
                shown: true,
                pinned: false,
            },
            &[a],
        );
        assert_eq!(
            actions(&decisions),
            WindowAction::RestoreBetweenWindows,
            "правила соседства сильнее закрепления"
        );
    }

    /// То же при показе: окно с правилами поднимается со всеми, а не
    /// молчит из-за того, что закреплено.
    #[test]
    fn hotkey_show_prefers_host_rules_over_conflicting_pinned_fact() {
        let mut a = member(1);
        a.pinned = true;
        a.has_host_rules = true;
        let (_, decisions) = decide_visibility(
            GroupVisibilityEvent::HotkeyPressed,
            GroupVisibilityState::default(),
            &[a],
        );
        assert_eq!(
            actions(&decisions),
            WindowAction::ShowTopmost,
            "показ поднимает окно с правилами, закрепление не мешает"
        );
    }

    // --- переключатель закрепления всей группы ---

    /// Переключатель закрепления на незакреплённой группе выдаёт каждому
    /// члену «закрепить поверх всех» и помечает группу закреплённой.
    #[test]
    fn pin_toggle_pins_all_members_topmost() {
        let a = member(1);
        let mut b = member(2);
        b.pinned = true; // уже закреплено — трогать не надо
        let (state, decisions) = decide_visibility(
            GroupVisibilityEvent::PinTogglePressed,
            GroupVisibilityState::default(),
            &[a, b],
        );
        assert!(state.pinned, "группа становится закреплённой");
        assert_eq!(
            decisions,
            vec![
                WindowDecision {
                    window: 1,
                    action: WindowAction::PinTopmost,
                },
                WindowDecision {
                    window: 2,
                    action: WindowAction::None,
                },
            ],
            "незакреплённые закрепляются, уже закреплённые не трогаются"
        );
    }

    /// Переключатель на закреплённой группе снимает закрепление со всех,
    /// но НЕ прячет их: окна остаются на экране, просто в обычном
    /// z-порядке.
    #[test]
    fn pin_toggle_unpins_all_members_without_hiding_them() {
        let mut a = member(1);
        a.pinned = true;
        a.visible = true;
        let mut b = member(2);
        b.visible = true; // закреплена группой? нет — факт не выставлен
        let (state, decisions) = decide_visibility(
            GroupVisibilityEvent::PinTogglePressed,
            GroupVisibilityState {
                shown: true,
                pinned: true,
            },
            &[a, b],
        );
        assert!(!state.pinned, "группа перестаёт быть закреплённой");
        assert_eq!(
            decisions,
            vec![
                WindowDecision {
                    window: 1,
                    action: WindowAction::UnpinTopmost,
                },
                WindowDecision {
                    window: 2,
                    action: WindowAction::None,
                },
            ],
            "закреплённые снимаются, незакреплённые не трогаются, никто не прячется"
        );
    }

    /// Переключатель закрепления не меняет показанность группы: закрепить
    /// можно и показанную, и спрятанную — это разные вещи.
    #[test]
    fn pin_toggle_does_not_change_shown_state() {
        let mut a = member(1);
        a.visible = true;
        let (state, decisions) = decide_visibility(
            GroupVisibilityEvent::PinTogglePressed,
            GroupVisibilityState {
                shown: true,
                pinned: false,
            },
            &[a],
        );
        assert!(state.shown, "показанность не тронута закреплением");
        assert_eq!(
            actions(&decisions),
            WindowAction::PinTopmost,
            "закрепление не поднимает и не прячет — только ставит стиль"
        );
    }

    /// Переключатель закрепления на пустой группе честно переключает
    /// состояние и не паникует — тот же принцип, что у хоткея показа.
    #[test]
    fn pin_toggle_on_empty_group_switches_state_without_panic() {
        let (state, decisions) = decide_visibility(
            GroupVisibilityEvent::PinTogglePressed,
            GroupVisibilityState::default(),
            &[],
        );
        assert!(state.pinned);
        assert!(decisions.is_empty());
        let (state, decisions) =
            decide_visibility(GroupVisibilityEvent::PinTogglePressed, state, &[]);
        assert!(!state.pinned);
        assert!(decisions.is_empty());
    }

    // --- вырожденный вход: все члены невидимы ---

    /// Группа, где все окна невидимы, на хоткей-сокрытие отвечает только
    /// None (прятать нечего), но состояние переключается честно — следующий
    /// хоткей покажет её заново.
    #[test]
    fn hotkey_hide_on_all_invisible_members_only_toggles_state() {
        let a = member(1);
        let b = member(2);
        let (state, decisions) = decide_visibility(
            GroupVisibilityEvent::HotkeyPressed,
            GroupVisibilityState {
                shown: true,
                pinned: false,
            },
            &[a, b],
        );
        assert!(!state.shown);
        assert_eq!(
            decisions,
            vec![
                WindowDecision {
                    window: 1,
                    action: WindowAction::None,
                },
                WindowDecision {
                    window: 2,
                    action: WindowAction::None,
                },
            ],
            "невидимые окна не дёргаются при сокрытии"
        );
    }
}
