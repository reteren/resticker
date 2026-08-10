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
use windows::core::w;

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

/// Попытаться стать единственным запущенным экземпляром resticker в этом
/// пользовательском сеансе. Имя мьютекса без префикса `Global\` — это и есть
/// желаемая область: разные пользователи (быстрое переключение сеансов,
/// RDP) должны иметь каждый свой экземпляр, конкурирует только повторный
/// запуск в ОДНОМ сеансе.
pub fn acquire() -> SingleInstanceResult {
    // SAFETY: имя — статическая nul-terminated wide-строка; bInitialOwner
    // не запрошен явным флагом, но это первый аргумент CreateMutexW —
    // передаём false (владение не забираем безусловно, только по факту
    // создания против уже существующего).
    let result = unsafe { CreateMutexW(None, false, w!("resticker_single_instance")) };
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

    // Мьютекс — процесс-wide singleton по имени; тесты этого модуля не
    // параллелятся между собой (общее имя), поэтому один тест на весь
    // жизненный цикл вместо нескольких — иначе `cargo test` в параллельных
    // потоках столкнул бы их друг с другом искусственно.
    #[test]
    fn second_acquire_in_same_process_sees_already_running() {
        let first = acquire();
        assert!(
            matches!(first, SingleInstanceResult::Acquired(_)),
            "первый вызов в процессе без чужого держателя должен захватить"
        );
        let second = acquire();
        assert!(
            matches!(second, SingleInstanceResult::AlreadyRunning),
            "второй вызов, пока первый хэндл ещё жив, должен увидеть занятость"
        );
        drop(first);
        let third = acquire();
        assert!(
            matches!(third, SingleInstanceResult::Acquired(_)),
            "после освобождения первого хэндла мьютекс снова свободен"
        );
    }
}
