//! The single error type for the backend.
//!
//! Every `#[tauri::command]` returns `Result<T, Error>` so the frontend always
//! receives a structured, serializable reason instead of a panic taking down
//! the core process (see CLAUDE.md §11).

use serde::{Serialize, Serializer};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("pane not found: {0}")]
    PaneNotFound(u64),

    #[error("pty error: {0}")]
    Pty(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    File(String),

    #[error("watch error: {0}")]
    Watch(String),

    #[error("remote control: {0}")]
    Remote(String),

    #[error("state lock poisoned")]
    Poisoned,
}

/// Serialize as a plain string so the frontend receives a readable reason.
impl Serialize for Error {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
