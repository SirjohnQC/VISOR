use crate::error::{Result, VisorError};
use std::path::PathBuf;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

/// Logs to `%APPDATA%\VISOR\visor.log`, rotated daily.
/// The returned guard must be kept alive for the process lifetime.
pub fn init(level: &str) -> Result<WorkerGuard> {
    let dir: PathBuf = crate::config::Config::default_path()
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .to_path_buf();
    std::fs::create_dir_all(&dir).map_err(|source| VisorError::Io {
        path: dir.clone(),
        source,
    })?;

    let appender = tracing_appender::rolling::daily(&dir, "visor.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);

    let filter = EnvFilter::try_new(level).unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .with_ansi(false)
        .init();

    Ok(guard)
}
