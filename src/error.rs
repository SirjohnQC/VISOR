use std::path::PathBuf;

#[allow(dead_code)]
#[derive(Debug, thiserror::Error)]
pub enum VisorError {
    #[error("config validation failed: {0}")]
    Config(String),
    #[error("io error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("windows api call failed: {0}")]
    Windows(String),
}

pub type Result<T> = std::result::Result<T, VisorError>;
