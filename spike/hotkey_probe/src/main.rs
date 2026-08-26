//! Проба поведения Windows-хоткеев для разведки H2 (docs/research/hotkeys).
//!
//! Измеряет на реальной машине:
//! 1. Какие комбинации RegisterHotKey вообще регистрирует, а какие отвергает
//!    системой (ERROR_HOTKEY_ALREADY_REGISTERED).
//! 2. Для зарегистрированных комбинаций — доходит ли нажатие до WM_HOTKEY
//!    при эмуляции через SendInput, и не переключилась ли вместо этого
//!    раскладка (переключатель языка/раскладки живёт ниже слоя хоткеев).
//! 3. Зависимость от ПОРЯДКА нажатия модификаторов: переключатель
//!    «Left Alt+Shift» срабатывает на keydown Alt при зажатом Shift —
//!    а не наоборот.
//! 4. Что происходит, когда низкоуровневый хук WH_KEYBOARD_LL глотает
//!    клавишу: регистрация хоткея успешна, но WM_HOTKEY не приходит.
//!
//! Разделы 5-6 временно пишут HKCU\Keyboard Layout\Toggle и затем удаляют
//! ключ (на этой машине его изначально не было) — исходное состояние
//! восстанавливается всегда.
//!
//! Это разведочный инструмент, в workspace не входит (spike исключён).

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetKeyboardLayout, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
    KEYEVENTF_KEYUP, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, RegisterHotKey,
    SendInput, UnregisterHotKey, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetForegroundWindow, GetWindowThreadProcessId, KBDLLHOOKSTRUCT,
    PeekMessageW, PM_REMOVE, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx,
    WH_KEYBOARD_LL, WM_HOTKEY, CallNextHookEx,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ, RegCloseKey,
    RegCreateKeyExW, RegDeleteKeyW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};
use windows::core::{PCWSTR, w};

// --- виртуальные коды, которых нет константами в windows 0.62 ---
const VK_ESCAPE: u32 = 0x1B;
const VK_SPACE: u32 = 0x20;
const VK_DELETE: u32 = 0x2E;
const VK_LSHIFT: u32 = 0xA0;
const VK_LMENU: u32 = 0xA4;
const VK_LCONTROL: u32 = 0xA2;
const VK_LWIN: u32 = 0x5B;
const VK_S: u32 = 0x53;
const VK_D: u32 = 0x44;
const VK_R: u32 = 0x52;
const VK_E: u32 = 0x45;
const VK_L: u32 = 0x4C;
const VK_TAB: u32 = 0x09;
const VK_F4: u32 = 0x73;
const VK_F12: u32 = 0x7B;
const VK_F24: u32 = 0x87;
const VK_SNAPSHOT: u32 = 0x2C;

static NEXT_ID: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0x4000);

fn next_id() -> i32 {
    NEXT_ID.fetch_add(1, Ordering::SeqCst)
}

fn vk_down(vk: u32, up: bool) {
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk as u16),
                wScan: 0,
                dwFlags: if up { KEYEVENTF_KEYUP } else { Default::default() },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    // SAFETY: корректный INPUT, размер — как у C-структуры INPUT.
    unsafe {
        SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
    }
}

/// Эмуляция нажатия с заданным порядком модификаторов и паузой между клавишами.
fn press_seq(mods: &[u32], main: u32, gap_ms: u64) {
    for &m in mods {
        vk_down(m, false);
        if gap_ms > 0 {
            std::thread::sleep(Duration::from_millis(gap_ms));
        }
    }
    vk_down(main, false);
    std::thread::sleep(Duration::from_millis(40));
    vk_down(main, true);
    for &m in mods.iter().rev() {
        std::thread::sleep(Duration::from_millis(40));
        vk_down(m, true);
    }
    std::thread::sleep(Duration::from_millis(60));
}

fn pump(ms: u64) -> bool {
    let deadline = Instant::now() + Duration::from_millis(ms);
    let mut fired = false;
    while Instant::now() < deadline {
        let mut msg = windows::Win32::UI::WindowsAndMessaging::MSG::default();
        // SAFETY: msg — валидный буфер; None — сообщения любых окон потока.
        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
            if msg.message == WM_HOTKEY {
                fired = true;
            }
            // SAFETY: msg пришёл из PeekMessageW.
            unsafe {
                let _ = TranslateMessage(&msg);
                let _ = DispatchMessageW(&msg);
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    fired
}

fn foreground_layout() -> u32 {
    // SAFETY: handle может быть NULL — GetWindowThreadProcessId с NULL
    // вернёт 0, GetKeyboardLayout(0) вернёт раскладку текущего потока.
    unsafe {
        let hwnd = GetForegroundWindow();
        let tid = GetWindowThreadProcessId(hwnd, None);
        GetKeyboardLayout(tid).0 as u32
    }
}

/// Физическое состояние модификаторов сразу после нажатия: «залип» ли какой-то
/// из них из-за переключателя раскладки (известный артефакт).
fn mod_state() -> String {
    let mut parts = Vec::new();
    // SAFETY: GetAsyncKeyState — без побочных эффектов.
    unsafe {
        for (name, vk) in [
            ("Ctrl", VK_LCONTROL),
            ("Alt", VK_LMENU),
            ("Shift", VK_LSHIFT),
            ("Win", VK_LWIN),
        ] {
            if GetAsyncKeyState(vk as i32) & i16::MIN != 0 {
                parts.push(name.to_string());
            }
        }
    }
    if parts.is_empty() {
        "все отпущены".to_string()
    } else {
        format!("залипли: {}", parts.join(", "))
    }
}

fn mods_of(alt: bool, ctrl: bool, shift: bool, win: bool) -> u32 {
    let mut m = MOD_NOREPEAT.0;
    if alt {
        m |= MOD_ALT.0;
    }
    if ctrl {
        m |= MOD_CONTROL.0;
    }
    if shift {
        m |= MOD_SHIFT.0;
    }
    if win {
        m |= MOD_WIN.0;
    }
    m
}

fn vks_of(alt: bool, ctrl: bool, shift: bool, win: bool) -> Vec<u32> {
    let mut v = Vec::new();
    if ctrl {
        v.push(VK_LCONTROL);
    }
    if alt {
        v.push(VK_LMENU);
    }
    if shift {
        v.push(VK_LSHIFT);
    }
    if win {
        v.push(VK_LWIN);
    }
    v
}

/// Только регистрация: пытаемся занять комбинацию и сразу снимаем.
fn try_register(name: &str, mods: u32, vk: u32) {
    let id = next_id();
    // SAFETY: hwnd=None, id из допустимого диапазона.
    let r = unsafe { RegisterHotKey(None, id, windows::Win32::UI::Input::KeyboardAndMouse::HOT_KEY_MODIFIERS(mods), vk) };
    match r {
        Ok(()) => {
            // SAFETY: тот же поток, что регистрировал.
            unsafe {
                let _ = UnregisterHotKey(None, id);
            }
            println!("{name:32} -> ЗАРЕГИСТРИРОВАНО");
        }
        Err(e) => println!("{name:32} -> ОТКАЗ: 0x{:08X} ({}), code {}", e.code().0, e.message(), e.code().0 as i32),
    }
}

/// Полный тест доставки: регистрация, эмуляция нажатия с заданным порядком
/// модификаторов, проверка WM_HOTKEY и смены раскладки.
fn delivery_order(name: &str, mods: u32, vk: u32, order: &[u32], gap_ms: u64) {
    let id = next_id();
    // SAFETY: hwnd=None, id из допустимого диапазона.
    let r = unsafe { RegisterHotKey(None, id, windows::Win32::UI::Input::KeyboardAndMouse::HOT_KEY_MODIFIERS(mods), vk) };
    if let Err(e) = r {
        println!("{name:32} -> не зарегистрировался: 0x{:08X} ({})", e.code().0, e.message());
        return;
    }
    let layout_before = foreground_layout();
    press_seq(order, vk, gap_ms);
    let fired = pump(700);
    let layout_after = foreground_layout();
    let state = mod_state();
    // SAFETY: тот же поток, что регистрировал.
    unsafe {
        let _ = UnregisterHotKey(None, id);
    }
    let layout_note = if layout_before != layout_after {
        format!("раскладка СМЕНИЛАСЬ {:08X}->{:08X}", layout_before, layout_after)
    } else {
        "раскладка та же".to_string()
    };
    println!(
        "{name:32} -> WM_HOTKEY: {} | {layout_note} | {state}",
        if fired { "ДА" } else { "НЕТ" }
    );
}

fn read_toggle_registry() {
    let mut hkey = HKEY::default();
    let path = w!("Keyboard Layout\\Toggle");
    // SAFETY: валидная wide-строка, hkey — буфер под результат.
    let ret = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(path.as_ptr()), Some(0), KEY_QUERY_VALUE, &mut hkey) };
    if ret != windows::Win32::Foundation::ERROR_SUCCESS {
        println!("HKCU\\Keyboard Layout\\Toggle: ключ НЕ СУЩЕСТВУЕТ -> настройки по умолчанию (Alt+Shift переключает язык, Ctrl+Shift — раскладку)");
        return;
    }
    for name in ["Hotkey", "Language Hotkey", "Layout Hotkey"] {
        let mut data = [0u16; 32];
        let mut size = (data.len() * 2) as u32;
        let name_wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: data — буфер достаточного размера; size обновит система.
        let ret = unsafe {
            RegQueryValueExW(
                hkey,
                PCWSTR(name_wide.as_ptr()),
                None,
                None,
                Some(data.as_mut_ptr().cast::<u8>()),
                Some(&mut size),
            )
        };
        if ret == windows::Win32::Foundation::ERROR_SUCCESS {
            let len = size as usize / 2;
            let s: String = data[..len].iter().take_while(|&&c| c != 0).map(|&c| c as u8 as char).collect();
            println!("HKCU\\Keyboard Layout\\Toggle\\{name} = {s:?}");
        } else {
            println!("HKCU\\Keyboard Layout\\Toggle\\{name} = <нет>");
        }
    }
    // SAFETY: hkey открыт выше и жив до сих пор.
    unsafe {
        let _ = RegCloseKey(hkey);
    }
}

fn set_toggle(values: &[(&str, &str)]) {
    let mut hkey = HKEY::default();
    let path = w!("Keyboard Layout\\Toggle");
    // SAFETY: валидная строка; phkresult получает владение ключом при успехе.
    let ret = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(path.as_ptr()),
            Some(0),
            PCWSTR(std::ptr::null()),
            windows::Win32::System::Registry::REG_OPEN_CREATE_OPTIONS(0),
            KEY_SET_VALUE,
            None,
            &mut hkey,
            None,
        )
    };
    if ret != windows::Win32::Foundation::ERROR_SUCCESS {
        println!("не удалось создать ключ Toggle: {}", ret.0);
        return;
    }
    for (name, val) in values {
        let name_wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        let val_wide: Vec<u16> = val.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes: &[u8] = unsafe { std::slice::from_raw_parts(val_wide.as_ptr().cast::<u8>(), val_wide.len() * 2) };
        // SAFETY: hkey действителен, bytes — валидный слайс из живого Vec.
        let _ = unsafe { RegSetValueExW(hkey, PCWSTR(name_wide.as_ptr()), Some(0), REG_SZ, Some(bytes)) };
    }
    // SAFETY: hkey открыт выше.
    unsafe {
        let _ = RegCloseKey(hkey);
    }
}

fn clear_toggle() {
    let path = w!("Keyboard Layout\\Toggle");
    // SAFETY: валидная строка пути.
    let _ = unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, PCWSTR(path.as_ptr())) };
}

fn main() {
    println!("=== Машина: {} ({}), Win11+ ===", std::env::consts::OS, std::env::consts::ARCH);
    read_toggle_registry();
    println!();

    println!("--- 1. Регистрация системно-зарезервированных комбинаций ---");
    try_register("Alt+Tab", MOD_ALT.0, VK_TAB);
    try_register("Alt+Esc", MOD_ALT.0, VK_ESCAPE);
    try_register("Ctrl+Esc", MOD_CONTROL.0, VK_ESCAPE);
    try_register("Alt+F4", MOD_ALT.0, VK_F4);
    try_register("Alt+Space", MOD_ALT.0, VK_SPACE);
    try_register("Ctrl+Alt+Del", MOD_CONTROL.0 | MOD_ALT.0, VK_DELETE);
    try_register("Win+L", MOD_WIN.0, VK_L);
    try_register("Win+Space", MOD_WIN.0, VK_SPACE);
    try_register("Win+Tab", MOD_WIN.0, VK_TAB);
    try_register("Win+D", MOD_WIN.0, VK_D);
    try_register("Win+R", MOD_WIN.0, VK_R);
    try_register("Win+E", MOD_WIN.0, VK_E);
    try_register("Win+Shift+S", MOD_WIN.0 | MOD_SHIFT.0, VK_S);
    try_register("Win+PrtScr", MOD_WIN.0, VK_SNAPSHOT);
    try_register("PrtScr", 0, VK_SNAPSHOT);
    try_register("F12", 0, VK_F12);
    println!();

    println!("--- 2. Доставка WM_HOTKEY: порядок нажатия модификаторов (ключ Toggle ОТСУТСТВУЕТ) ---");
    let as_mods = mods_of(true, false, true, false);
    delivery_order("Alt+Shift+S [Alt,Shift]", as_mods, VK_S, &vks_of(true, false, true, false), 40);
    delivery_order("Alt+Shift+S [Shift,Alt]", as_mods, VK_S, &[VK_LSHIFT, VK_LMENU], 40);
    delivery_order("Alt+Shift+S [Shift,Alt] 8мс", as_mods, VK_S, &[VK_LSHIFT, VK_LMENU], 8);
    let cs_mods = mods_of(false, true, true, false);
    delivery_order("Ctrl+Shift+S [Ctrl,Shift]", cs_mods, VK_S, &[VK_LCONTROL, VK_LSHIFT], 40);
    delivery_order("Ctrl+Shift+S [Shift,Ctrl]", cs_mods, VK_S, &[VK_LSHIFT, VK_LCONTROL], 40);
    delivery_order("Ctrl+Alt+S [Ctrl,Alt] (контроль)", mods_of(true, true, false, false), VK_S, &[VK_LCONTROL, VK_LMENU], 40);
    println!();

    println!("--- 3. Win+Space БЕЗ регистрации: переключает ли раскладку ---");
    let before = foreground_layout();
    press_seq(&[VK_LWIN], VK_SPACE, 40);
    std::thread::sleep(Duration::from_millis(500));
    let after = foreground_layout();
    println!("раскладка: {:08X} -> {:08X} {}", before, after, if before != after { "(переключилась)" } else { "(та же)" });
    println!();

    println!("--- 4. Голые модификаторы (без хоткея): когда срабатывает переключатель ---");
    // Переключатель раскладки чувствителен к таймингу: повторяем каждую
    // последовательность трижды с разными задержками между клавишами.
    for (name, mods, main) in [
        ("Alt+Shift [Alt,Shift]", vec![VK_LMENU, VK_LSHIFT], VK_S),
        ("Shift+Alt [Shift,Alt]", vec![VK_LSHIFT, VK_LMENU], VK_S),
        ("Ctrl+Shift [Ctrl,Shift]", vec![VK_LCONTROL, VK_LSHIFT], VK_S),
        ("Shift+Ctrl [Shift,Ctrl]", vec![VK_LSHIFT, VK_LCONTROL], VK_S),
    ] {
        let mut switched = Vec::new();
        for gap in [5u64, 15, 50] {
            let before = foreground_layout();
            press_seq(&mods, main, gap);
            std::thread::sleep(Duration::from_millis(300));
            let after = foreground_layout();
            let ok = before != after;
            switched.push(if ok { format!("{gap}мс:ДА") } else { format!("{gap}мс:нет") });
            if ok {
                break; // сработало — повторять смысла нет
            }
        }
        println!("{name:34} -> {} | {}", switched.join(", "), mod_state());
    }
    println!();

    println!("--- 5. Toggle=1 (Alt+Shift): те же тесты при явно включённом переключателе ---");
    set_toggle(&[("Hotkey", "1"), ("Language Hotkey", "1"), ("Layout Hotkey", "1")]);
    std::thread::sleep(Duration::from_millis(700));
    for (name, mods, main) in [
        ("Alt+Shift [Alt,Shift]", vec![VK_LMENU, VK_LSHIFT], VK_S),
        ("Shift+Alt [Shift,Alt]", vec![VK_LSHIFT, VK_LMENU], VK_S),
    ] {
        let mut switched = Vec::new();
        for gap in [5u64, 15, 50] {
            let before = foreground_layout();
            press_seq(&mods, main, gap);
            std::thread::sleep(Duration::from_millis(300));
            let after = foreground_layout();
            let ok = before != after;
            switched.push(if ok { format!("{gap}мс:ДА") } else { format!("{gap}мс:нет") });
            if ok {
                break;
            }
        }
        println!("{name:34} -> {} | {}", switched.join(", "), mod_state());
    }
    delivery_order("Alt+Shift+S [Alt,Shift] 5мс", as_mods, VK_S, &vks_of(true, false, true, false), 5);
    delivery_order("Alt+Shift+S [Shift,Alt] 5мс", as_mods, VK_S, &[VK_LSHIFT, VK_LMENU], 5);
    delivery_order("Ctrl+Shift+S [Ctrl,Shift] 5мс", cs_mods, VK_S, &[VK_LCONTROL, VK_LSHIFT], 5);
    clear_toggle();
    println!("Toggle удалён (исходное состояние восстановлено)");
    println!();

    println!("--- 6. Toggle=2 (Ctrl+Shift): те же тесты при переключателе Ctrl+Shift ---");
    set_toggle(&[("Hotkey", "2"), ("Language Hotkey", "2"), ("Layout Hotkey", "2")]);
    std::thread::sleep(Duration::from_millis(700));
    for (name, mods, main) in [
        ("Ctrl+Shift [Ctrl,Shift]", vec![VK_LCONTROL, VK_LSHIFT], VK_S),
        ("Shift+Ctrl [Shift,Ctrl]", vec![VK_LSHIFT, VK_LCONTROL], VK_S),
    ] {
        let mut switched = Vec::new();
        for gap in [5u64, 15, 50] {
            let before = foreground_layout();
            press_seq(&mods, main, gap);
            std::thread::sleep(Duration::from_millis(300));
            let after = foreground_layout();
            let ok = before != after;
            switched.push(if ok { format!("{gap}мс:ДА") } else { format!("{gap}мс:нет") });
            if ok {
                break;
            }
        }
        println!("{name:34} -> {} | {}", switched.join(", "), mod_state());
    }
    delivery_order("Ctrl+Shift+S [Ctrl,Shift] 5мс", cs_mods, VK_S, &[VK_LCONTROL, VK_LSHIFT], 5);
    delivery_order("Ctrl+Shift+S [Shift,Ctrl] 5мс", cs_mods, VK_S, &[VK_LSHIFT, VK_LCONTROL], 5);
    delivery_order("Alt+Shift+S [Alt,Shift] 5мс", as_mods, VK_S, &vks_of(true, false, true, false), 5);
    clear_toggle();
    println!("Toggle удалён (исходное состояние восстановлено)");
    println!();

    println!("--- 7. WH_KEYBOARD_LL глотает F24, проверка Ctrl+Alt+F24 ---");
    // SAFETY: ll_proc — статическая функция, живущая всё время жизни хука;
    // хук на нулевом потоке — глобальный, вызывается из потока ввода.
    let hook = unsafe {
        SetWindowsHookExW(WH_KEYBOARD_LL, Some(ll_proc), Some(HINSTANCE(GetModuleHandleW(None).unwrap().0)), 0)
    };
    match hook {
        Ok(h) => {
            SWALLOW_F24.store(true, Ordering::SeqCst);
            let id = next_id();
            // SAFETY: hwnd=None, id допустимый.
            let r = unsafe { RegisterHotKey(None, id, windows::Win32::UI::Input::KeyboardAndMouse::HOT_KEY_MODIFIERS(mods_of(true, true, false, false)), VK_F24) };
            match r {
                Ok(()) => {
                    println!("Ctrl+Alt+F24 зарегистрирован ПРИ живом хуке, глотающем F24:");
                    press_seq(&[VK_LCONTROL, VK_LMENU], VK_F24, 40);
                    let fired = pump(700);
                    println!("  с хуком: WM_HOTKEY = {}", if fired { "ДА" } else { "НЕТ" });
                    // SAFETY: тот же поток.
                    unsafe {
                        let _ = UnregisterHotKey(None, id);
                    }
                }
                Err(e) => println!("  Ctrl+Alt+F24 не зарегистрировался: {}", e.message()),
            }
            SWALLOW_F24.store(false, Ordering::SeqCst);
            let id = next_id();
            // SAFETY: hwnd=None, id допустимый.
            let r = unsafe { RegisterHotKey(None, id, windows::Win32::UI::Input::KeyboardAndMouse::HOT_KEY_MODIFIERS(mods_of(true, true, false, false)), VK_F24) };
            if let Ok(()) = r {
                press_seq(&[VK_LCONTROL, VK_LMENU], VK_F24, 40);
                let fired = pump(700);
                println!("  без хука: WM_HOTKEY = {}", if fired { "ДА" } else { "НЕТ" });
                // SAFETY: тот же поток.
                unsafe {
                    let _ = UnregisterHotKey(None, id);
                }
            }
            // SAFETY: хук жив, снимаем с того же потока.
            unsafe {
                let _ = UnhookWindowsHookEx(h);
            }
        }
        Err(e) => println!("хук не установился: {}", e.message()),
    }
    println!();

    println!("--- 8. Перерегистрация на лету: снять старый бинд, поставить новый ---");
    {
        let mods = mods_of(true, true, false, false);
        // Первичная регистрация: комбинация свободна.
        let id_a = next_id();
        // SAFETY: hwnd=None, id из допустимого диапазона.
        let r = unsafe {
            RegisterHotKey(
                None,
                id_a,
                windows::Win32::UI::Input::KeyboardAndMouse::HOT_KEY_MODIFIERS(mods),
                VK_F24,
            )
        };
        match r {
            Ok(()) => {
                press_seq(&[VK_LCONTROL, VK_LMENU], VK_F24, 40);
                let fired = pump(700);
                println!(
                    "первичная регистрация id={id_a}: WM_HOTKEY = {}",
                    if fired { "ДА" } else { "НЕТ" }
                );
                // Контроль: повторная регистрация ТОЙ ЖЕ комбинации без снятия
                // обязана дать конфликт — именно поэтому перерегистрация на
                // лету сначала снимает старые хоткеи, а потом ставит новые.
                let id_b = next_id();
                // SAFETY: hwnd=None, id допустимый.
                let dup = unsafe {
                    RegisterHotKey(
                        None,
                        id_b,
                        windows::Win32::UI::Input::KeyboardAndMouse::HOT_KEY_MODIFIERS(mods),
                        VK_F24,
                    )
                };
                match dup {
                    Ok(()) => {
                        println!("повторная регистрация без снятия: НЕОЖИДАННЫЙ УСПЕХ");
                        // SAFETY: тот же поток, что регистрировал.
                        unsafe {
                            let _ = UnregisterHotKey(None, id_b);
                        }
                    }
                    Err(e) => println!(
                        "повторная регистрация без снятия: КОНФЛИКТ 0x{:08X} — как и ожидалось",
                        e.code().0
                    ),
                }
                // Смена бинда «на ту же клавишу»: снять старую, поставить новую.
                // SAFETY: тот же поток — снятие и регистрация здесь же.
                unsafe {
                    let _ = UnregisterHotKey(None, id_a);
                }
                let id_c = next_id();
                // SAFETY: hwnd=None, id допустимый.
                let r2 = unsafe {
                    RegisterHotKey(
                        None,
                        id_c,
                        windows::Win32::UI::Input::KeyboardAndMouse::HOT_KEY_MODIFIERS(mods),
                        VK_F24,
                    )
                };
                match r2 {
                    Ok(()) => {
                        press_seq(&[VK_LCONTROL, VK_LMENU], VK_F24, 40);
                        let fired2 = pump(700);
                        println!(
                            "перерегистрация id={id_c}: WM_HOTKEY = {}",
                            if fired2 { "ДА" } else { "НЕТ" }
                        );
                        // SAFETY: тот же поток, что регистрировал.
                        unsafe {
                            let _ = UnregisterHotKey(None, id_c);
                        }
                    }
                    Err(e) => println!(
                        "перерегистрация после снятия: ОТКАЗ 0x{:08X} ({})",
                        e.code().0,
                        e.message()
                    ),
                }
            }
            Err(e) => println!(
                "первичная регистрация Ctrl+Alt+F24: ОТКАЗ 0x{:08X} ({})",
                e.code().0,
                e.message()
            ),
        }
    }
    println!();

    println!("Готово.");
}

static SWALLOW_F24: AtomicBool = AtomicBool::new(false);

/// Хук WH_KEYBOARD_LL: если SWALLOW_F24 — глотает keydown клавиши F24.
unsafe extern "system" fn ll_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && SWALLOW_F24.load(Ordering::SeqCst) {
        let kbd = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        if kbd.vkCode == VK_F24 && (wparam.0 as u32 & 0x80) == 0 {
            return LRESULT(1); // съесть: дальше (и до хоткеев) не пойдёт
        }
    }
    // SAFETY: вызов следующего хука в цепочке; код из системы.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}