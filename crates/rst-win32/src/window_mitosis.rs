//! Митоз окон (docs/M9_WINDOW_MITOSIS_DESIGN.md, §4.2): нижняя платформенная
//! прослойка — замер веса процесса и свободной памяти, запуск второго
//! экземпляра приложения и ожидание его НОВОГО окна.
//!
//! Разбиение по дизайну: отбор нового окна (`match_sibling_window`) — ЧИСТАЯ
//! функция, чтобы юнит-тесты гоняли её на синтетических снимках без реальных
//! окон и Win32; весь `unsafe` живёт в обёртках вокруг неё
//! (`process_private_bytes`, `spawn_sibling`, `wait_for_sibling_window`).
//! Вызывающий слой (координатор) сначала сжимает оригинал и лишь потом
//! блокируется на `wait_for_sibling_window` — и потому именно здесь стоит
//! предупреждение звать её только с фонового потока.

use std::collections::HashSet;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::{
    CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, DETACHED_PROCESS, OpenProcess,
    PROCESS_QUERY_LIMITED_INFORMATION,
};

use crate::error::Win32Error;
use crate::window_enum::WindowInfo;

/// RAII-обёртка над хэндлом процесса: `CloseHandle` обязан выполняться на
/// ЛЮБОМ пути — и при отказе `GetProcessMemoryInfo`, и при успехе. Без
/// гарантированного закрытия каждый замер нового окна тек бы дескрипторами
/// (лимит на процесс велик, но в долгой сессии дескрипторы копятся, а это
/// чужие процессы, которые к тому же нельзя освободить повторным закрытием).
struct ProcessHandle(HANDLE);

impl ProcessHandle {
    /// Открыть процесс по pid с минимальным правом чтения
    /// (`PROCESS_QUERY_LIMITED_INFORMATION`). Его достаточно для
    /// `GetProcessMemoryInfo`, а прав, которых у нас нет (чужой
    /// elevated-процесс), он не требует — запрашиваем ровно минимум, чтобы
    /// обычный запуск (не админ) читал максимум процессов.
    fn open(pid: u32) -> Option<Self> {
        // SAFETY: OpenProcess — регистрация доступа по pid, никакую память мы
        // не трогаем; на закрытый доступ вернёт ошибку, а не UB.
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
        Some(Self(handle))
    }
}

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        // SAFETY: self.0 — хэндл, открытый этим же объектом и не закрытый
        // ранее (единственный владелец — обёртка); CloseHandle для валидного
        // хэндла безопасен, возврат ошибки здесь невозможен по построению.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Приватная (закоммиченная) память процесса, байты
/// (`PROCESS_MEMORY_COUNTERS_EX::PrivateUsage`).
///
/// Зачем именно она (M9, §3): отказы `TooHeavy`/`NotEnoughMemory` решаются ДО
/// разреза — вес процесса сравнивается с лимитом `mitosis_max_memory_mb` и со
/// свободной физической памятью. Приватная память — единственная метрика, не
/// скачущая вместе с переиспользуемыми страницами: это ровно то, что процесс
/// удерживает под себя, и именно она отвечает за риск завалить систему вторым
/// экземпляром.
///
/// `None` — доступ закрыт (`OpenProcess` или `GetProcessMemoryInfo` отказали;
/// защищённые процессы не отдают даже минимальное право). Это НЕ отказ митоза:
/// `preflight` получает `Option` и пропускает неизвестный вес (M9, §4.1).
pub fn process_private_bytes(pid: u32) -> Option<u64> {
    let process = ProcessHandle::open(pid)?;
    let mut counters = PROCESS_MEMORY_COUNTERS_EX {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    // EX-структура — надстройка над базовой PROCESS_MEMORY_COUNTERS: поля
    // совпадают, EX добавляет PrivateUsage в конец, поэтому PSAPI принимает
    // указатель на старшую структуру как на младшую — отсюда приведение.
    let counters_ptr: *mut PROCESS_MEMORY_COUNTERS = (&raw mut counters).cast();
    // SAFETY: process — валидный открытый хэндл (ProcessHandle живёт до конца
    // вызова); counters — валидный буфер под EX-структуру с обязательным полем
    // cb; GetProcessMemoryInfo пишет ровно cb байт.
    let ok = unsafe {
        GetProcessMemoryInfo(
            process.0,
            counters_ptr,
            std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        )
    }
    .is_ok();
    ok.then_some(counters.PrivateUsage as u64)
}

/// Свободная физическая память системы, байты (`GlobalMemoryStatusEx`).
///
/// Вторая половина отсечки `NotEnoughMemory` (M9, §3): если приватный вес
/// процесса превышает свободную физическую память, второй экземпляр утопит
/// систему в своп до того, как пользователь увидит окно. `None` — `Ex`-вызов
/// отказал (крайне редко); тогда префлайт получает `None` и не отказывает,
/// как и при неизвестном весе.
pub fn available_physical_bytes() -> Option<u64> {
    let mut status = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // SAFETY: status — валидный буфер под MEMORYSTATUSEX; dwLength заполнен
    // обязательным первым полем (без него вызов честно вернёт ошибку, а не
    // мусор в структуре).
    unsafe { GlobalMemoryStatusEx(&mut status) }.ok()?;
    Some(status.ullAvailPhys)
}

/// Дескриптор запущенного второго экземпляра.
///
/// Содержит только pid — и сознательно НЕ хэндл процесса. Приложения,
/// открывающие второе окно через брокера (Проводник), заставляют дочерний
/// процесс тут же выйти, а держать хэндл над зомби-процессом бессмысленно:
/// найти новое окно можно только по владельцу (`WindowInfo::pid`) или пути
/// (`WindowInfo::exe_path`), и обоих pid достаточно.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sibling {
    pub pid: u32,
}

/// Запустить второй экземпляр `exe_path`; рабочий каталог — каталог exe.
///
/// `DETACHED_PROCESS` — дочерний не наследует консоль resticker и не получает
/// свою; `CREATE_NO_WINDOW` — страховка от мелькнувшего консольного окна;
/// `CREATE_NEW_PROCESS_GROUP` — чтобы дочерний не делил с нами процесс-группу
/// и не выживал/не умирал вместе с ней при завершении resticker. stdin/stdout/
/// stderr заткнуты: вывод второго экземпляра не должен никуда течь, а
/// наследованные дескрипторы держали бы pipe живым.
pub fn spawn_sibling(exe_path: &Path) -> Result<Sibling, Win32Error> {
    let dir = exe_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let child = Command::new(exe_path)
        .current_dir(dir)
        .creation_flags(CREATE_NO_WINDOW.0 | DETACHED_PROCESS.0 | CREATE_NEW_PROCESS_GROUP.0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(Win32Error::SiblingSpawnFailed)?;
    // Намеренно не держим `Child`: не ждём и не убиваем — для single-instance
    // приложений дочерний должен спокойно выйти сразу после передачи окна
    // брокеру, а его каналы заткнуты (null), так что drop ни на что не влияет.
    Ok(Sibling { pid: child.id() })
}

/// ЧИСТАЯ функция отбора (тестируется без Win32): НОВОЕ окно этого
/// приложения в снимке. Совпадение — `hwnd` отсутствует в `known` И
/// (`pid == sibling_pid` ИЛИ `exe_path` совпадает без учёта регистра).
///
/// Ветка по exe обязательна (M9, §4.2): Проводник и любое приложение с
/// брокером открывают новое окно в УЖЕ ЖИВУЩЕМ процессе, а запущенный нами
/// дочерний процесс тут же выходит — по одному pid новое окно не найти
/// никогда. Совпадение по обоим признакам (pid И exe) — обычный случай
/// приложения, которое заводит собственный второй процесс; совпадение только
/// по exe при чужом pid — случай Проводника. `known` — окна, уже бывшие на
/// экране до разреза: без него «новым» оказалось бы старое окно того же
/// приложения.
///
/// Сравнение exe без учёта регистра: пути Windows нечувствительны к регистру,
/// и `C:\Windows\Explorer.exe` от перечисления к перечислению пишется в том
/// виде, в котором его отдал процесс — «правильного» регистра не существует.
/// Приближение `to_lowercase` — ординальная инвариантная схема регистра,
/// чистая и без Win32; пустой путь (защищённый процесс, exe не прочитался)
/// считается НЕсовпадением — «не знаю» не должно быть «совпало».
pub fn match_sibling_window<'a>(
    snapshot: &'a [WindowInfo],
    sibling_pid: u32,
    exe_path: &Path,
    known: &HashSet<usize>,
) -> Option<&'a WindowInfo> {
    snapshot.iter().find(|w| {
        !known.contains(&w.hwnd) && (w.pid == sibling_pid || exe_paths_equal(&w.exe_path, exe_path))
    })
}

/// Сравнение путей exe без учёта регистра ([`match_sibling_window`]).
fn exe_paths_equal(a: &Path, b: &Path) -> bool {
    if a.as_os_str().is_empty() || b.as_os_str().is_empty() {
        return false;
    }
    a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
}

/// БЛОКИРУЮЩЕЕ ожидание нового окна: опрос [`crate::window_enum::enumerate`]
/// каждые `poll` до истечения `timeout`, отбор — [`match_sibling_window`].
/// Вызывать только с фонового потока (координатор блокировать нельзя).
///
/// Первый опрос — сразу, без паузы: второй экземпляр часто успевает открыть
/// окно, пока мы добрались сюда, и ждать лишние `poll` не нужно. `None` —
/// время вышло, новый экземпляр окна не показал (single-instance приложение
/// лишь сфокусировало старое): это честный отказ `NoSecondWindow` (M9, §3).
/// Рекомендованные значения вызывающего слоя: `poll = 150 мс`,
/// `timeout = 8 с`.
pub fn wait_for_sibling_window(
    sibling_pid: u32,
    exe_path: &Path,
    known: &HashSet<usize>,
    poll: Duration,
    timeout: Duration,
) -> Option<WindowInfo> {
    let deadline = Instant::now() + timeout;
    loop {
        let snapshot = crate::window_enum::enumerate();
        if let Some(found) = match_sibling_window(&snapshot, sibling_pid, exe_path, known) {
            return Some(found.clone());
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(poll);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn info(hwnd: usize, pid: u32, exe: &str) -> WindowInfo {
        WindowInfo {
            hwnd,
            pid,
            exe_path: PathBuf::from(exe),
            ..Default::default()
        }
    }

    #[test]
    fn new_window_matched_by_sibling_pid() {
        let snapshot = [
            info(1, 1000, r"C:\apps\one.exe"),
            info(2, 4242, r"C:\apps\two.exe"),
        ];
        let known = HashSet::new();
        let found = match_sibling_window(&snapshot, 4242, Path::new(r"C:\apps\two.exe"), &known);
        assert_eq!(found.map(|w| w.hwnd), Some(2));
    }

    #[test]
    fn new_window_by_exe_when_pid_is_foreign_explorer_case() {
        // Проводник (и любой процесс-брокер) открывает новое окно в УЖЕ
        // ЖИВУЩЕМ процессе, а дочерний процесс выходит сразу — окна с нашим
        // sibling_pid в снимке нет вообще, отбор обязан найти окно по exe.
        let snapshot = [info(10, 555, r"C:\Windows\explorer.exe")];
        let known = HashSet::new();
        let found = match_sibling_window(
            &snapshot,
            4242,
            Path::new(r"C:\Windows\explorer.exe"),
            &known,
        );
        assert_eq!(found.map(|w| w.hwnd), Some(10));
    }

    #[test]
    fn window_from_known_set_ignored() {
        // Окно, бывшее на экране до разреза, «новым» быть не может — иначе
        // разрез старого окна принял бы его за результат.
        let snapshot = [info(7, 4242, r"C:\apps\two.exe")];
        let known = HashSet::from([7usize]);
        let found = match_sibling_window(&snapshot, 4242, Path::new(r"C:\apps\two.exe"), &known);
        assert_eq!(found, None);
    }

    #[test]
    fn new_window_preferred_over_known_window_of_same_app() {
        let snapshot = [
            info(7, 4242, r"C:\apps\two.exe"),
            info(9, 4242, r"C:\apps\two.exe"),
        ];
        let known = HashSet::from([7usize]);
        let found = match_sibling_window(&snapshot, 4242, Path::new(r"C:\apps\two.exe"), &known);
        assert_eq!(found.map(|w| w.hwnd), Some(9));
    }

    #[test]
    fn exe_path_compared_without_case() {
        let snapshot = [info(3, 999, r"c:\windows\explorer.exe")];
        let known = HashSet::new();
        let found = match_sibling_window(
            &snapshot,
            4242,
            Path::new(r"C:\WINDOWS\EXPLORER.EXE"),
            &known,
        );
        assert_eq!(found.map(|w| w.hwnd), Some(3));
    }

    #[test]
    fn empty_exe_path_never_matches() {
        let mut w = info(5, 4242, r"C:\apps\two.exe");
        w.exe_path = PathBuf::new();
        let known = HashSet::new();
        assert_eq!(
            match_sibling_window(&[w], 1111, Path::new(r"C:\apps\two.exe"), &known),
            None,
            "пустой exe — «не прочитано», а не «совпало»"
        );
    }

    #[test]
    fn empty_snapshot_yields_none() {
        let known = HashSet::new();
        assert_eq!(
            match_sibling_window(&[], 4242, Path::new(r"C:\apps\two.exe"), &known),
            None
        );
    }

    #[test]
    fn process_private_bytes_of_own_process_is_positive() {
        let bytes = process_private_bytes(std::process::id())
            .expect("свой процесс обязан читаться правом PROCESS_QUERY_LIMITED_INFORMATION");
        assert!(
            bytes > 0,
            "свой процесс не может иметь нулевую приватную память, получено {bytes}"
        );
    }

    #[test]
    fn available_physical_bytes_is_positive() {
        let bytes = available_physical_bytes().expect("GlobalMemoryStatusEx обязан работать");
        assert!(
            bytes > 0,
            "свободной физической памяти ноль, получено {bytes}"
        );
    }
}
