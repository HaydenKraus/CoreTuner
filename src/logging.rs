//! Файловый лог с ротацией по размеру и запись паник в crash.log.
//!
//! В release-сборке нет консоли (windows-подсистема) и `panic = "abort"`, поэтому без этого
//! модуля диагностировать сбой нечем.
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

/// Размер файла лога, после которого он ротируется.
const MAX_LOG_BYTES: u64 = 1024 * 1024;

/// Каталог логов: `<config_dir>\logs`.
pub fn log_dir() -> PathBuf {
    let dir = crate::profiles::storage::config_dir().join("logs");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Файл `coretuner.log`; при превышении лимита переименовывается в `coretuner.log.1`.
struct RotatingFile {
    path: PathBuf,
    file: Option<File>,
    written: u64,
    max: u64,
}

impl RotatingFile {
    fn new(path: PathBuf, max: u64) -> Self {
        let mut r = RotatingFile { path, file: None, written: 0, max };
        r.open();
        r
    }

    fn open(&mut self) {
        self.file = OpenOptions::new().create(true).append(true).open(&self.path).ok();
        self.written = self.file.as_ref().and_then(|f| f.metadata().ok()).map_or(0, |m| m.len());
    }

    fn rotate(&mut self) {
        self.file = None;
        let mut old = self.path.clone().into_os_string();
        old.push(".1");
        let _ = std::fs::remove_file(&old);
        let _ = std::fs::rename(&self.path, &old);
        self.open();
    }
}

impl Write for RotatingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.written >= self.max {
            self.rotate();
        }
        // Лог не должен ронять приложение: недоступный файл молча пропускаем.
        if let Some(f) = self.file.as_mut() {
            if f.write_all(buf).is_ok() {
                self.written += buf.len() as u64;
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.file.as_mut() {
            Some(f) => f.flush(),
            None => Ok(()),
        }
    }
}

#[derive(Clone)]
struct SharedWriter(Arc<Mutex<RotatingFile>>);

impl Write for SharedWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).flush()
    }
}

/// Инициализировать логирование в файл и panic hook.
pub fn init() {
    let level = match std::env::var("CORETUNER_LOG").as_deref() {
        Ok("info") => tracing::Level::INFO,
        Ok("debug") => tracing::Level::DEBUG,
        _ => tracing::Level::WARN,
    };
    let writer = SharedWriter(Arc::new(Mutex::new(RotatingFile::new(log_dir().join("coretuner.log"), MAX_LOG_BYTES))));
    tracing_subscriber::fmt()
        .with_max_level(level)
        .with_target(false)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .init();
    install_panic_hook();
}

/// Паника пишется в `crash.log` (hook срабатывает до abort).
fn install_panic_hook() {
    let path = crate::profiles::storage::config_dir().join("crash.log");
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let text = format!(
            "[{}] panic в потоке {}: {info}\n{}\n\n",
            crate::app::timestamp(),
            thread.name().unwrap_or("?"),
            std::backtrace::Backtrace::force_capture()
        );
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
            let _ = f.write_all(text.as_bytes());
        }
        tracing::error!("panic: {info}");
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotates_when_limit_exceeded() {
        let dir = std::env::temp_dir().join(format!("ct-log-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("t.log");
        let mut w = RotatingFile::new(path.clone(), 10);
        assert!(w.write_all(b"0123456789AB").is_ok());
        assert!(w.write_all(b"next").is_ok());
        assert!(dir.join("t.log.1").exists());
        assert_eq!(std::fs::read_to_string(&path).unwrap_or_default(), "next");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
