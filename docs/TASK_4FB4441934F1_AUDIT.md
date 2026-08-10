# Отчёт task_4fb4441934f1 — UIPI-тост, жизненный цикл хуков, фильтр окон

**Вердикт: все три угла чистые, критичных находок нет. Фикс BTN_ADD_WINDOW
(`tracker_mask_needed`, `overlay_manager.rs:4203-4205`, вызов `:2864-2870`)
согласуется со всеми тремя смежными подсистемами.**

## 1) Тост PinAccessDenied долетает, не теряется

Цепочка полная: `window_pin.rs:81` `pin()` — `SetWindowPos`/`SetPropW` с
`ERROR_ACCESS_DENIED` мапятся в `Win32Error::PinAccessDenied`
(`window_pin.rs:234-236`). Обработчик клика `add_window_sticker` шлёт
`CoordinatorRequest::ShowNotification` (`overlay_manager.rs:7204-7208`) в
канал из `main.rs:342`. Приёмник — выделенный поток в `setup`
(`main.rs:422-462`): `main.rs:443-448` вызывает
`state::<TrayIcon>().show_balloon()`, ошибка — в `tracing::warn!`.
`TrayIcon` зарегистрирован `.manage(tray_icon)` в `main.rs:347` — до
`setup`, паника `state()` невозможна. `show_balloon` (`tray.rs:168-191`) —
NIM_MODIFY/NIF_INFO по живой иконке, тело (~190 символов) влезает в лимит
255 UTF-16. `let _ = send(...)` безопасно: ресивер живёт весь процесс, а
ранняя отправка (онбординг, `overlay_manager.rs:1256`) буферизуется mpsc до
старта потока. Единственная оговорка вне кода: Windows 11/Focus Assist
может подавить баллон на уровне ОС.

## 2) set_mask_needed идемпотентен, утечки SetWinEventHook нет

Тройная защита: координатор шлёт только на реальную смену
(`overlay_manager.rs:2864-2870`); обработчик `WM_APP_SET_MASK_NEEDED`
гейтится `needed != state.mask_needed` (`window_tracker.rs:632`); сам
`install_hooks` выходит при непустом `hooks` (`window_tracker.rs:320-322`).
`false` — `uninstall_hooks` с `drain` + `UnhookWinEvent` каждого хука
(`window_tracker.rs:354-361`, вызов `:651`). Быстрые серии true/false
обрабатываются FIFO на потоке трекера, install/uninstall симметричны, Vec
не накапливается. Выход: Drop → WM_CLOSE → WM_DESTROY снимает хуки
(`window_tracker.rs:700-709`, `:124-138`). Нюанс (не утечка): при частичном
фейле `SetWinEventHook` ретрай возможен только на цикле false→true, т.к.
`mask_needed` уже выставлен (`window_tracker.rs:339-348`).

## 3) Фильтр не режет «первые интуитивные» окна

`is_real_window` (`window_enum.rs:148-168`) отсекает лишь: невидимые,
cloaked (UWP-сон/другой виртуальный стол), не-root, `WS_EX_NOACTIVATE`,
TOOLWINDOW/owned без APPWINDOW. Требования заголовка/класса нет. Explorer
(CabinetWClass), Chrome/Edge/VS Code (Chrome_WidgetWin_1) — обычные
видимые root-окна, проходят все проверки. Свёрнутые (`iconic`) в снимке
сохраняются, а на пикинге отсекаются в `window_at` вместе с окнами своего
процесса (`window_pick.rs:50`) — корректно, их rect мусорный.
