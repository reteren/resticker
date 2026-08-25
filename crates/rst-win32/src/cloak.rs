//! Скрытие чужих окон для воркспейсов тайлинга (M9, docs/TILING_DESIGN.md §Р1).
//!
//! Воркспейсы у resticker свои, а не системные виртуальные столы Windows:
//! публичный API не даёт перечислить столы вообще, а приватный COM ломается
//! почти на каждом обновлении Windows 11. komorebi, GlazeWM и Whim пришли к
//! тому же решению — прятать окна неактивного воркспейса самим.
//!
//! ## Почему `DWMWA_CLOAK`, а не `ShowWindow`
//!
//! Три способа спрятать чужое окно, и два из них плохи (замеры чужих
//! проектов — docs/research/tiling/R2_PRIOR_ART.md §2):
//!
//! * `SW_HIDE` — окно «умирает» для своего приложения; Electron от этого
//!   ломается, у komorebi этот режим помечен устаревшим;
//! * `SW_MINIMIZE` — окна прыгают в панели задач и обратно, при частом
//!   переключении это заметно и раздражает;
//! * `DWMWA_CLOAK` — композитор просто перестаёт рисовать окно. Для
//!   приложения оно остаётся видимым и живым, анимации сворачивания нет,
//!   из Task View и переключателей оно исчезает. Это то, что нужно.
//!
//! ## Watchdog — обязательная часть, а не опция
//!
//! Скрытое окно не видно НИКАК: ни на экране, ни в Task View. Если
//! resticker упадёт со скрытыми окнами, пользователь получит бесследно
//! пропавшие окна и никакого способа их вернуть, кроме перезапуска
//! приложений. GlazeWM держит ради этого отдельный процесс-сторож.
//!
//! Здесь дешевле: на каждое скрытое окно ставится наш маркер
//! ([`CLOAK_PROP`]) — ровно тот же приём, которым пины помечают чужие окна
//! (window_pin.rs:92). При старте [`recover_orphans`] проходит по всем окнам
//! рабочего стола, находит помеченные и показывает их обратно. Маркер живёт
//! на ЧУЖОМ окне и переживает наше падение, поэтому уборка возможна и после
//! аварийного завершения.
//!
//! Обход в [`recover_orphans`] — свой, а не через
//! [`crate::window_enum::enumerate`]: тот фильтрует скрытые окна как
//! «ненастоящие», то есть не отдал бы ровно те окна, ради которых уборка и
//! существует.

use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, TRUE};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAK, DwmSetWindowAttribute};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetPropW, IsWindow, RemovePropW, SetPropW,
};
use windows::core::{BOOL, PCWSTR, w};

/// Маркер «это окно спрятано тайлингом resticker».
///
/// Отдельное имя от маркера пинов (`resticker`): у окна может быть и то, и
/// другое, а уборка одного не должна снимать чужой маркер.
const CLOAK_PROP: PCWSTR = w!("resticker.tiling.cloaked");

/// Значение маркера. Само по себе не значит ничего — важен факт наличия;
/// `SetPropW` не считает нулевой хэндл установленным свойством.
///
/// `without_provenance_mut`, а не `1 as *mut _`: это не указатель, а
/// непрозрачное значение, и такая запись говорит об этом и компилятору, и
/// читателю.
fn mark() -> HANDLE {
    HANDLE(std::ptr::without_provenance_mut(1))
}

/// Спрятать окно.
///
/// `false` — окна нет или DWM отказал (например, окно процесса с более
/// высоким уровнем целостности: UIPI не пускает нас и сюда).
///
/// Порядок важен: маркер ставится ДО скрытия. Если процесс умрёт между двумя
/// вызовами, в худшем случае останется маркер на видимом окне — уборка его
/// снимет и ничего не сломает. Обратный порядок дал бы худший исход:
/// скрытое окно без маркера, то есть невидимое навсегда.
pub fn cloak(hwnd: usize) -> bool {
    let target = HWND(hwnd as *mut core::ffi::c_void);
    // SAFETY: IsWindow безопасен для любого значения хэндла.
    if !unsafe { IsWindow(Some(target)) }.as_bool() {
        return false;
    }
    // SAFETY: SetPropW безопасен для чужого окна; строка-имя статическая.
    if unsafe { SetPropW(target, CLOAK_PROP, Some(mark())) }.is_err() {
        return false;
    }
    if set_cloaked(target, true) {
        true
    } else {
        // Скрыть не вышло — маркер обязан уйти, иначе следующая уборка
        // сочтёт видимое окно нашим и молча снимет с него cloak, которого
        // мы не ставили.
        // SAFETY: RemovePropW безопасен для чужого окна.
        let _ = unsafe { RemovePropW(target, CLOAK_PROP) };
        false
    }
}

/// Показать окно обратно и снять маркер.
pub fn uncloak(hwnd: usize) -> bool {
    let target = HWND(hwnd as *mut core::ffi::c_void);
    // SAFETY: IsWindow безопасен для любого значения хэндла.
    if !unsafe { IsWindow(Some(target)) }.as_bool() {
        return false;
    }
    if !set_cloaked(target, false) {
        // Маркер НЕ снимаем: показать окно не удалось, и если убрать метку,
        // уборка при следующем запуске его уже не найдёт — окно останется
        // невидимым навсегда. Зеркальная дыра к порядку в [`cloak`], нашло
        // второе ревью (docs/research/tiling/REVIEW_T4_T6.md, находка 1).
        return false;
    }
    // SAFETY: RemovePropW безопасен для чужого окна; отсутствие маркера —
    // не ошибка.
    let _ = unsafe { RemovePropW(target, CLOAK_PROP) };
    true
}

/// На окне стоит наш маркер скрытия?
pub fn is_marked(hwnd: usize) -> bool {
    let target = HWND(hwnd as *mut core::ffi::c_void);
    // SAFETY: GetPropW безопасен для чужих и мёртвых окон.
    !unsafe { GetPropW(target, CLOAK_PROP) }.0.is_null()
}

/// Стартовая уборка: показать все окна, помеченные прошлым запуском.
///
/// Возвращает число возвращённых окон. Вызывать ОДИН РАЗ при старте, до того
/// как тайлинг начнёт прятать что-либо своё, — иначе уборка снимет скрытие с
/// окон текущей сессии.
pub fn recover_orphans() -> usize {
    let mut marked: Vec<HWND> = Vec::new();
    // SAFETY: `marked` живёт весь вызов и не разделяется; колбэк
    // синхронный, на этом же потоке.
    unsafe {
        let _ = EnumWindows(Some(collect_marked), LPARAM(&raw mut marked as isize));
    }
    let mut recovered = 0;
    for hwnd in marked {
        if uncloak(hwnd.0 as usize) {
            recovered += 1;
        }
    }
    if recovered > 0 {
        tracing::info!(
            recovered,
            "вернул окна, спрятанные тайлингом в прошлом запуске"
        );
    }
    recovered
}

/// Колбэк `EnumWindows`: собирает окна с нашим маркером.
extern "system" fn collect_marked(hwnd: HWND, data: LPARAM) -> BOOL {
    // SAFETY: `data` — &mut Vec<HWND> из recover_orphans, живой на всё время
    // перечисления; колбэк синхронный.
    let out = unsafe { &mut *(data.0 as *mut Vec<HWND>) };
    // SAFETY: GetPropW безопасен для чужих окон.
    if !unsafe { GetPropW(hwnd, CLOAK_PROP) }.0.is_null() {
        out.push(hwnd);
    }
    BOOL(1)
}

/// Общая часть [`cloak`] и [`uncloak`]: сам вызов DWM.
fn set_cloaked(hwnd: HWND, cloaked: bool) -> bool {
    let value: BOOL = if cloaked { TRUE } else { BOOL(0) };
    // SAFETY: значение живёт до конца вызова, размер соответствует типу
    // атрибута (BOOL); DwmSetWindowAttribute безопасен для чужих и мёртвых
    // окон — на мёртвом просто вернёт ошибку.
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_CLOAK,
            (&raw const value).cast(),
            size_of::<BOOL>() as u32,
        )
    }
    .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Заведомо невалидный хэндл: окно, закрывшееся между снимком и
    /// операцией, — штатная гонка, а не исключительная ситуация.
    const DEAD: usize = 0xDEAD_BEEF;

    #[test]
    fn cloaking_a_dead_window_fails_instead_of_panicking() {
        assert!(!cloak(DEAD));
    }

    #[test]
    fn uncloaking_a_dead_window_fails_instead_of_panicking() {
        assert!(!uncloak(DEAD));
    }

    #[test]
    fn a_dead_window_carries_no_marker() {
        assert!(!is_marked(DEAD));
    }

    #[test]
    fn recovery_on_a_clean_desktop_finds_nothing() {
        // Уборка трогает ТОЛЬКО окна с нашим маркером. Если бы она умела
        // задеть чужое окно, этот тест на живой машине разработчика
        // показал бы ненулевой результат — и это было бы поводом
        // остановиться, а не поправить ожидание.
        assert_eq!(recover_orphans(), 0);
    }
}
