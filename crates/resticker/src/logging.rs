//! Логирование через `tracing`, файл `resticker-YYYY-MM-DD.log` в
//! `%LOCALAPPDATA%\resticker\logs\` (CONFIG.md, ROADMAP.md M0).
//!
//! M0-уровень: файл открывается один раз при старте с текущей датой.
//! Ротация в полночь для долгоживущего процесса — за пределами этого среза.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::Context;

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

    let today = chrono::Utc::now().format("%Y-%m-%d");
    let path = dir.join(format!("resticker-{today}.log"));
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
