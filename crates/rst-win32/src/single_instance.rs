//! Защита от повторного запуска: именованный мьютекс на весь пользовательский
//! сеанс. Без неё второй `resticker.exe` (автозапуск + ручной запуск, или
//! повторный клик по ярлыку до того, как первый успел открыть трей) поднимает
//! второй набор оверлей-окон `WS_EX_TOPMOST` на тех же мониторах — оба
//! получают одни и те же реальные события мыши/клавиатуры вперемешку
//! (какое окно сейчас выше в z-order — то и получает клик), независимо
//! перерисовывают HUD и независимо дерутся за `SetCapture`. Внешне это
//! выглядит как «программа живёт своей жизнью»: панели дёргаются, клики не
//! попадают куда нужно, а мышь может залипнуть в чужом захвате, пока не
//! убить оба процесса разом (`taskkill /IM resticker.exe`).

use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::PCWSTR;

/// Владение мьютексом единственного экземпляра — держать живым (не дропать)
/// всё время работы процесса; освобождается автоматически ОС при выходе,
/// даже если процесс упал без штатного `Drop` (аварийное завершение,
/// `taskkill`).
pub struct SingleInstance(HANDLE);

impl Drop for SingleInstance {
    fn drop(&mut self) {
        // SAFETY: self.0 — валидный хэндл мьютекса, полученный в `acquire` и
        // ещё не закрытый (единственное место закрытия).
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Результат попытки стать единственным экземпляром.
pub enum SingleInstanceResult {
    /// Мьютекс захвачен этим процессом — он первый; держите значение живым
    /// (например, в переменной `main()`) до конца работы программы.
    Acquired(SingleInstance),
    /// Мьютекс уже держит другой живой процесс resticker.
    AlreadyRunning,
    /// Не удалось даже создать мьютекс (крайне редкий системный сбой) — не
    /// повод блокировать запуск: лучше один лишний (маловероятный) второй
    /// экземпляр, чем ни одного рабочего в системе, где `CreateMutex` почему-то
    /// недоступен.
    Error,
}

/// Имя именованного мьютекса для единственного экземпляра в пользовательском сеансе.
pub const MUTEX_NAME: &str = "resticker_single_instance";

/// Попытаться стать единственным запущенным экземпляром resticker в этом
/// пользовательском сеансе. Имя мьютекса без префикса `Global\` — это и есть
/// желаемая область: разные пользователи (быстрое переключение сеансов,
/// RDP) должны иметь каждый свой экземпляр, конкурирует только повторный
/// запуск в ОДНОМ сеансе.
pub fn acquire() -> SingleInstanceResult {
    acquire_named(MUTEX_NAME)
}

/// Попытаться захватить мьютекс с произвольным именем в пространстве сеанса.
///
/// Используется как реализация для [`acquire`] с боевым именем [`MUTEX_NAME`],
/// а также в тестах для изолированной проверки жизненного цикла и конкуренции
/// без конфликта с уже запущенным в этом же сеансе экземпляром приложения.
pub fn acquire_named(name: &str) -> SingleInstanceResult {
    let mut wide: Vec<u16> = name.encode_utf16().collect();
    wide.push(0);

    // SAFETY: wide — нуль-терминированная wide-строка; bInitialOwner
    // передаём false (владение не забираем безусловно, только по факту
    // создания против уже существующего).
    let result = unsafe { CreateMutexW(None, false, PCWSTR(wide.as_ptr())) };
    match result {
        Ok(handle) => {
            // SAFETY: GetLastError читается сразу после CreateMutexW на этом
            // же потоке — валидно и при Ok (CreateMutexW может вернуть
            // валидный хэндл на УЖЕ существующий объект и одновременно
            // выставить ERROR_ALREADY_EXISTS, это штатный Win32-паттерн).
            let already_exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
            if already_exists {
                // SAFETY: handle — валидный (хоть и чужой) хэндл; закрываем
                // свою ссылку на него, само событие «второй экземпляр» уже
                // зафиксировано выше.
                unsafe {
                    let _ = CloseHandle(handle);
                }
                SingleInstanceResult::AlreadyRunning
            } else {
                SingleInstanceResult::Acquired(SingleInstance(handle))
            }
        }
        Err(_) => SingleInstanceResult::Error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn unique_test_mutex_name(tag: &str) -> String {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let pid = std::process::id();
        let count = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        format!("resticker_test_{tag}_{pid}_{nanos}_{count}")
    }

    #[test]
    fn prod_mutex_name_is_stable_and_session_local() {
        // Имя боевого мьютекса не должно случайно измениться в коде,
        // иначе два экземпляра приложения перестанут видеть друг друга.
        assert_eq!(
            MUTEX_NAME, "resticker_single_instance",
            "продовое имя мьютекса изменилось! Это сломает защиту от повторного запуска"
        );
        // Должно быть без префикса "Global\" — разные сеансы Windows (RDP,
        // свитч пользователей) не должны конфликтовать между собой.
        assert!(
            !MUTEX_NAME.starts_with(r"Global\"),
            "мьютекс не должен быть глобальным на всю систему (только пользовательский сеанс)"
        );
    }

    #[test]
    fn second_acquire_in_same_process_sees_already_running() {
        // Причина изменения (2026-09-08): тест использовал фиксированное имя
        // "resticker_single_instance" и был перманентно красным на машине
        // разработчика, пока запущен настоящий resticker.exe. Проверка
        // жизненного цикла мьютекса (захват -> занятость -> освобождение ->
        // повторный захват) выполняется с уникальным именем сеанса, сохраняя
        // всю строгость и не конкурируя с живым процессом.
        let name = unique_test_mutex_name("lifecycle");
        let first = acquire_named(&name);
        assert!(
            matches!(first, SingleInstanceResult::Acquired(_)),
            "первый вызов в процессе без чужого держателя должен захватить"
        );
        let second = acquire_named(&name);
        assert!(
            matches!(second, SingleInstanceResult::AlreadyRunning),
            "второй вызов, пока первый хэндл ещё жив, должен увидеть занятость"
        );
        drop(first);
        let third = acquire_named(&name);
        assert!(
            matches!(third, SingleInstanceResult::Acquired(_)),
            "после освобождения первого хэндла мьютекс снова свободен"
        );
    }

    #[test]
    fn independent_names_do_not_interfere() {
        // Проверяем изоляцию по имени: удержание мьютекса с именем A
        // не должно препятствовать захвату мьютекса с именем B.
        let name_a = unique_test_mutex_name("isolated_a");
        let name_b = unique_test_mutex_name("isolated_b");

        let lock_a = acquire_named(&name_a);
        assert!(
            matches!(lock_a, SingleInstanceResult::Acquired(_)),
            "захват мьютекса A должен пройти успешно"
        );

        let lock_b = acquire_named(&name_b);
        assert!(
            matches!(lock_b, SingleInstanceResult::Acquired(_)),
            "захват мьютекса B должен пройти успешно, несмотря на удерживаемый A"
        );

        let lock_a_again = acquire_named(&name_a);
        assert!(
            matches!(lock_a_again, SingleInstanceResult::AlreadyRunning),
            "повторный захват мьютекса A по-прежнему должен сообщать о занятости"
        );

        drop(lock_a);
        drop(lock_b);
    }

    #[test]
    fn invalid_mutex_name_returns_error() {
        // Имена объектов ядра Windows не могут содержать символ '\' кроме
        // префиксов пространства имён ("Global\" / "Local\"). Невалидное имя
        // возвращает ERROR_INVALID_NAME, что должно транслироваться в SingleInstanceResult::Error.
        let result = acquire_named(r"resticker\invalid\name");
        assert!(
            matches!(result, SingleInstanceResult::Error),
            "невалидное имя мьютекса со слэшем должно приводить к SingleInstanceResult::Error"
        );
    }
}
