//! Виртуальные рабочие столы через `IVirtualDesktopManager` (ROADMAP.md M6,
//! «Виртуальные рабочие столы через IVirtualDesktopManager с fallback»).
//!
//! Публичный COM-интерфейс (Windows 10 1607+,
//! `shobjidl_core.h`/`IVirtualDesktopManager`) даёт ровно одну полезную
//! координатору операцию: [`is_window_on_current_desktop`] — «окно на
//! текущем столе или пользователь переключился и уехал вместе с ним».
//! Этого достаточно для M6: закреплённый стикер прячется/не рисуется,
//! когда окно-таргет ушло на другой рабочий стол.
//!
//! COM-паттерн — как в `file_dialog.rs`/`window_icon.rs`: инициализация на
//! вызывающем потоке на время одного вызова (`CoInitializeEx`/
//! `CoUninitialize`), никакого состояния между вызовами.
//!
//! # Почему `MoveWindowToDesktop` НЕ реализован
//!
//! Публичный `IVirtualDesktopManager::MoveWindowToDesktop(hwnd, guid)`
//! требует GUID целевого стола, а публичный API Windows не позволяет
//! перечислить GUID'ы рабочих столов — только недокументированные
//! приватные интерфейсы explorer (`IVirtualDesktopManagerInternal`/
//! `IVirtualDesktop`, нестабильные между сборками Windows). ROADMAP M6
//! явно просит «fallback, не обход», поэтому перенос окна на другой стол
//! сознательно не реализуется: перечислять столы нечем, а угадывать GUID
//! нельзя. Если окно уехало — координатор скрывает стикер (fallback),
//! а не тащит окно обратно.

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoUninitialize,
};
use windows::Win32::UI::Shell::IVirtualDesktopManager;
use windows::core::GUID;

use crate::error::Win32Error;

/// CLSID `VirtualDesktopManager` (`{AA509086-5CA9-4C25-8F95-589D3C07B48A}`,
/// `shobjidl_core.h`) — в этой версии `windows-rs` константа не
/// генерируется (та же причина, что у `CLSID_FILE_OPEN_DIALOG` в
/// `file_dialog.rs`), поэтому задана вручную по значению из заголовка.
const CLSID_VIRTUAL_DESKTOP_MANAGER: GUID = GUID::from_u128(0xAA509086_5CA9_4C25_8F95_589D3C07B48A);

/// Окно `hwnd` на текущем виртуальном рабочем столе?
///
/// `Ok(true)` — окно на том же столе, что и процесс; `Ok(false)` — окно
/// уехало на другой стол (координатор прячет стикер). Ошибка — только
/// когда менеджер столов вообще недоступен: [`Win32Error::VirtualDesktopManagerUnavailable`]
/// (нет Windows 10 1607+, компонент не зарегистрирован) или низкоуровневая
/// ошибка COM-вызова.
///
/// `hwnd` — числовой ключ окна (как в `window_pin`); проверка «окно живо»
/// остаётся за вызывающим кодом (`IsWindow`-семантика), сюда достаточно
/// передать hwnd живого top-level окна.
pub fn is_window_on_current_desktop(hwnd: usize) -> Result<bool, Win32Error> {
    // SAFETY: инициализация COM на вызывающем потоке на время одного
    // вызова; ошибка игнорируется, как в file_dialog.rs/window_icon.rs
    // (повторная инициализация — RPC_E_CHANGED_MODE, не фатальна для
    // шелл-вызовов).
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let result = check_inner(HWND(hwnd as *mut core::ffi::c_void));
    // SAFETY: парный `CoUninitialize` для `CoInitializeEx` выше — на том же
    // потоке, после того как COM-объект менеджера уже отпущен (он локальна
    // для `check_inner` и падает из области видимости до этого вызова).
    unsafe { CoUninitialize() };
    result
}

fn check_inner(hwnd: HWND) -> Result<bool, Win32Error> {
    // SAFETY: CLSID_VIRTUAL_DESKTOP_MANAGER — валидный CLSID системного
    // объекта; COM инициализирован вызывающей функцией; out-интерфейс
    // создаётся как IVirtualDesktopManager (единственный публичный
    // интерфейс менеджера).
    let manager: IVirtualDesktopManager =
        unsafe { CoCreateInstance(&CLSID_VIRTUAL_DESKTOP_MANAGER, None, CLSCTX_INPROC_SERVER) }
            .map_err(|e| Win32Error::VirtualDesktopManagerUnavailable(e.to_string()))?;
    // SAFETY: вызов метода живого COM-объекта на потоке с инициализированным
    // COM; hwnd — числовой ключ, любые значения безопасны (функция вернёт
    // false/ошибку для несуществующего окна).
    let on_current = unsafe { manager.IsWindowOnCurrentVirtualDesktop(hwnd) }?;
    Ok(on_current.as_bool())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `from_u128` и `from_values` — два независимых способа собрать один и
    /// тот же CLSID; совпадение подтверждает, что литерал `from_u128`
    /// действительно кодирует `{AA509086-5CA9-4C25-8F95-589D3C07B48A}`
    /// побайтово так же, как канонические поля `Data1..Data4`.
    #[test]
    fn clsid_matches_canonical_fields() {
        let expected = GUID::from_values(
            0xAA509086,
            0x5CA9,
            0x4C25,
            [0x8F, 0x95, 0x58, 0x9D, 0x3C, 0x07, 0xB4, 0x8A],
        );
        assert_eq!(CLSID_VIRTUAL_DESKTOP_MANAGER, expected);
    }

    /// Реальная проверка на живом десктопе: создаём видимое top-level окно
    /// (тот же паттерн, что `RealWindow` в window_pin.rs) и спрашиваем
    /// менеджера про него. На машине С виртуальными столами свежесозданное
    /// окно обязано быть на текущем столе (`Ok(true)`); без менеджера —
    /// понятная ошибка доступности, а не падение.
    #[test]
    #[ignore = "требует реальный десктоп; запуск вручную: cargo test -p rst-win32 virtual_desktops -- --ignored"]
    fn fresh_window_is_on_current_desktop() {
        use windows::Win32::Foundation::{GetLastError, LPARAM, LRESULT, WPARAM};
        use windows::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassExW, WNDCLASSEXW,
            WS_OVERLAPPED, WS_VISIBLE,
        };
        use windows::core::w;

        unsafe extern "system" fn wndproc(
            hwnd: HWND,
            msg: u32,
            wparam: WPARAM,
            lparam: LPARAM,
        ) -> LRESULT {
            // SAFETY: делегирование системному обработчику.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }

        // SAFETY: GetModuleHandleW(None) — хэндл текущего модуля.
        let hinstance = unsafe { GetModuleHandleW(None) }.expect("хэндл модуля");
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance.into(),
            lpszClassName: w!("resticker_virtual_desktops_test"),
            ..Default::default()
        };
        // SAFETY: wc заполнена корректно; повторная регистрация
        // (параллельные тесты) — не ошибка.
        if unsafe { RegisterClassExW(&wc) } == 0 {
            // SAFETY: осмысленна сразу после провалившегося вызова.
            let err = unsafe { GetLastError() };
            assert_eq!(err, windows::Win32::Foundation::ERROR_CLASS_ALREADY_EXISTS);
        }
        // SAFETY: все аргументы — валидные константы и зарегистрированный
        // класс; окно видимое, top-level, на текущем столе по построению.
        let hwnd = unsafe {
            CreateWindowExW(
                Default::default(),
                w!("resticker_virtual_desktops_test"),
                w!("rst-win32 virtual desktops test"),
                WS_OVERLAPPED | WS_VISIBLE,
                0,
                0,
                160,
                120,
                None,
                None,
                Some(hinstance.into()),
                None,
            )
        }
        .expect("создание тестового окна");

        let result = is_window_on_current_desktop(hwnd.0 as usize);
        // SAFETY: окно создано этим же потоком выше.
        let _ = unsafe { DestroyWindow(hwnd) };

        match result {
            Ok(true) => {}
            Ok(false) => panic!(
                "свежесозданное окно обязано быть на текущем столе (иначе тест-поток \
                 создал окно на фоновом столе — проверьте окружение)"
            ),
            Err(Win32Error::VirtualDesktopManagerUnavailable(msg)) => {
                eprintln!(
                    "менеджер виртуальных столов недоступен на этой машине \
                     (нет Windows 10 1607+?) — тест пропущен: {msg}"
                );
            }
            Err(e) => panic!("неожиданная ошибка: {e}"),
        }
    }
}
