//! Логирование через `tracing`, файл `resticker-YYYY-MM-DD.log` в
//! `%LOCALAPPDATA%\resticker\logs\` (CONFIG.md, ROADMAP.md M0).
//!
//! При старте чистим только файлы собственного формата: замер на 2026-09-07
//! показал 75 МБ в 24 файлах за 24 дня, причём один день занимал 42 МБ.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::Context;
use chrono::{Duration, NaiveDate};

const MAX_TOTAL_BYTES: u64 = 50 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = 20 * 1024 * 1024;
const RETENTION_DAYS: i64 = 7;

struct FileWriter(Mutex<File>);

impl Write for &FileWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).flush()
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for FileWriter {
    type Writer = &'a FileWriter;
    fn make_writer(&'a self) -> Self::Writer {
        self
    }
}

/// Каталог `%LOCALAPPDATA%\resticker\logs\`.
pub fn logs_dir() -> anyhow::Result<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA не задана")?;
    Ok(PathBuf::from(base).join("resticker").join("logs"))
}

/// Инициализирует `tracing` с записью в файл дня и возвращает путь к нему.
pub fn init() -> anyhow::Result<PathBuf> {
    let dir = logs_dir()?;
    fs::create_dir_all(&dir).with_context(|| format!("создание {}", dir.display()))?;

    let today = chrono::Utc::now().date_naive();
    // Ошибка чистки не должна превращать резидентный процесс в не запускающийся:
    // логгер ещё не поднят, поэтому сообщить об отказе всё равно некуда.
    let _ = cleanup_directory(&dir, today);
    let path = current_log_path(&dir, today);
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("открытие {}", path.display()))?;

    let writer = FileWriter(Mutex::new(file));
    tracing_subscriber::fmt()
        .with_writer(writer)
        .with_ansi(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    Ok(path)
}

/// Планирует удаление старых и лишних по объёму логов.
///
/// Дата в кортеже берётся из имени файла при чтении каталога. Повторно
/// проверяем имя здесь, чтобы чистая логика не могла запланировать удаление
/// `resticker.log` или другого файла, случайно переданного вызывающим кодом.
fn plan_cleanup(files: &[(String, u64, NaiveDate)], today: NaiveDate) -> Vec<String> {
    let mut valid = files
        .iter()
        .filter_map(|(name, size, date)| {
            let (parsed_date, suffix) = parse_log_name(name)?;
            (parsed_date == *date).then_some((name, *size, *date, suffix))
        })
        .collect::<Vec<_>>();

    // Последний сегмент сегодняшнего дня — активный. Именно его нельзя
    // удалять: при повторном запуске это может быть уже `.2.log`, а не база.
    let current_today = valid
        .iter()
        .filter(|(_, _, date, _)| *date == today)
        .max_by_key(|(_, _, _, suffix)| suffix.unwrap_or(1))
        .map(|(name, _, _, _)| (*name).clone());
    let cutoff = today - Duration::days(RETENTION_DAYS);
    let mut deletions = Vec::new();
    let mut survivors = Vec::new();
    let mut total = 0_u64;

    for (name, size, date, _) in valid.drain(..) {
        if date < cutoff {
            deletions.push(name.clone());
        } else {
            survivors.push((name, size, date));
            total = total.saturating_add(size);
        }
    }

    // Удаляем самые старые файлы. Активный файл уже исключён из списка
    // кандидатов, поэтому сегодняшний единственный файл переживает лимит.
    if total > MAX_TOTAL_BYTES {
        survivors.sort_by(|a, b| a.2.cmp(&b.2).then_with(|| a.0.cmp(b.0)));
        for (name, size, _) in survivors {
            if total <= MAX_TOTAL_BYTES {
                break;
            }
            if current_today.as_deref() == Some(name.as_str()) {
                continue;
            }
            total = total.saturating_sub(size);
            deletions.push(name.clone());
        }
    }

    deletions.sort();
    deletions
}

fn parse_log_name(name: &str) -> Option<(NaiveDate, Option<u32>)> {
    let stem = name.strip_prefix("resticker-")?.strip_suffix(".log")?;
    let (date_part, suffix) = match stem.split_once('.') {
        Some((date, suffix)) => {
            let suffix = suffix.parse::<u32>().ok()?;
            (date, Some((suffix >= 2).then_some(suffix)?))
        }
        None => (stem, None),
    };
    let date = NaiveDate::parse_from_str(date_part, "%Y-%m-%d").ok()?;
    (date.format("%Y-%m-%d").to_string() == date_part).then_some((date, suffix))
}

fn cleanup_directory(dir: &Path, today: NaiveDate) -> io::Result<()> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        if !entry
            .file_type()
            .map(|kind| kind.is_file())
            .unwrap_or(false)
        {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some((date, _)) = parse_log_name(&name) else {
            continue;
        };
        let size = match entry.metadata() {
            Ok(metadata) => metadata.len(),
            Err(_) => continue,
        };
        files.push((name, size, date));
    }

    for name in plan_cleanup(&files, today) {
        // Отказ удаления (занятый файл, права, исчезнувший каталог) допустим:
        // следующий запуск попробует снова, не ломая запуск приложения сейчас.
        let _ = fs::remove_file(dir.join(name));
    }
    Ok(())
}

fn current_log_path(dir: &Path, today: NaiveDate) -> PathBuf {
    let mut segments = fs::read_dir(dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.to_owned();
            let (date, suffix) = parse_log_name(&name)?;
            (date == today).then_some((name, suffix.unwrap_or(1)))
        })
        .collect::<Vec<_>>();
    segments.sort_by_key(|(_, suffix)| *suffix);

    let (name, suffix) = segments
        .last()
        .cloned()
        .unwrap_or_else(|| (format!("resticker-{today}.log"), 1));
    let path = dir.join(&name);
    let too_large = fs::metadata(&path)
        .map(|metadata| metadata.len() > MAX_FILE_BYTES)
        .unwrap_or(false);
    if too_large {
        let next_suffix = suffix.saturating_add(1).max(2);
        dir.join(format!("resticker-{today}.{next_suffix}.log"))
    } else {
        path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).unwrap()
    }

    fn file(name: &str, size: u64, date: NaiveDate) -> (String, u64, NaiveDate) {
        (name.to_owned(), size, date)
    }

    #[test]
    fn files_older_than_a_week_go_fresh_ones_stay() {
        let today = date(2026, 9, 7);
        let files = vec![
            file("resticker-2026-08-30.log", 1, date(2026, 8, 30)),
            file("resticker-2026-08-31.log", 1, date(2026, 8, 31)),
            file("resticker-2026-09-07.log", 1, today),
        ];
        assert_eq!(
            plan_cleanup(&files, today),
            vec!["resticker-2026-08-30.log".to_owned()]
        );
    }

    #[test]
    fn over_the_size_cap_the_oldest_go_first() {
        let today = date(2026, 9, 7);
        let files = vec![
            file(
                "resticker-2026-09-01.log",
                20 * 1024 * 1024,
                date(2026, 9, 1),
            ),
            file(
                "resticker-2026-09-02.log",
                20 * 1024 * 1024,
                date(2026, 9, 2),
            ),
            file(
                "resticker-2026-09-03.log",
                20 * 1024 * 1024,
                date(2026, 9, 3),
            ),
            file("resticker-2026-09-07.log", 1, today),
        ];
        assert_eq!(
            plan_cleanup(&files, today),
            vec!["resticker-2026-09-01.log".to_owned()]
        );
    }

    #[test]
    fn todays_file_survives_even_alone_over_the_cap() {
        let today = date(2026, 9, 7);
        let files = vec![file("resticker-2026-09-07.log", 60 * 1024 * 1024, today)];
        assert!(plan_cleanup(&files, today).is_empty());
    }

    #[test]
    fn foreign_names_are_never_planned_for_deletion() {
        let today = date(2026, 9, 7);
        let files = vec![
            file("notes.txt", 60 * 1024 * 1024, date(2020, 1, 1)),
            file("resticker.log", 60 * 1024 * 1024, date(2020, 1, 1)),
            file(
                "resticker-2026-13-99.log",
                60 * 1024 * 1024,
                date(2020, 1, 1),
            ),
        ];
        assert!(plan_cleanup(&files, today).is_empty());
    }

    #[test]
    fn an_empty_directory_plans_nothing() {
        assert!(plan_cleanup(&[], date(2026, 9, 7)).is_empty());
    }

    #[test]
    fn cleanup_of_a_real_directory_leaves_exactly_what_is_expected() {
        let today = date(2026, 9, 7);
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("resticker-logging-test-{unique}"));
        fs::create_dir_all(&dir).unwrap();
        let files = [
            ("resticker-2026-08-20.log", 1_u64),
            ("resticker-2026-09-01.log", 20 * 1024 * 1024),
            ("resticker-2026-09-02.log", 20 * 1024 * 1024),
            ("resticker-2026-09-03.log", 20 * 1024 * 1024),
            ("resticker-2026-09-07.log", 1),
            ("notes.txt", 100),
        ];
        for (name, size) in files {
            let file = File::create(dir.join(name)).unwrap();
            file.set_len(size).unwrap();
        }

        cleanup_directory(&dir, today).unwrap();

        assert!(!dir.join("resticker-2026-08-20.log").exists());
        assert!(!dir.join("resticker-2026-09-01.log").exists());
        assert!(dir.join("resticker-2026-09-02.log").exists());
        assert!(dir.join("resticker-2026-09-03.log").exists());
        assert!(dir.join("resticker-2026-09-07.log").exists());
        assert!(dir.join("notes.txt").exists());
        fs::remove_dir_all(dir).unwrap();
    }
}
