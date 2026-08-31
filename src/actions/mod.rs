use crate::core::types::DisplayLevel;
use std::sync::{Arc, Mutex};

pub trait DisplayControl: Send {
    /// Bring every target monitor to `level`. Errors are logged internally;
    /// a failure must never propagate into a state change (spec §8).
    fn apply(&mut self, level: DisplayLevel);
}

pub struct SpyDisplay {
    log: Arc<Mutex<Vec<DisplayLevel>>>,
}

impl SpyDisplay {
    pub fn new() -> Self {
        Self {
            log: Arc::new(Mutex::new(Vec::new())),
        }
    }
    pub fn log(&self) -> Arc<Mutex<Vec<DisplayLevel>>> {
        self.log.clone()
    }
}

impl Default for SpyDisplay {
    fn default() -> Self {
        Self::new()
    }
}

impl DisplayControl for SpyDisplay {
    fn apply(&mut self, level: DisplayLevel) {
        self.log.lock().unwrap().push(level);
    }
}

/// Spec §7 `hold_awake_while_present`. Real implementation in Task 13.
pub fn set_awake_hold(_on: bool) {}
