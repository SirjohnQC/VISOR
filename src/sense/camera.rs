use crate::core::types::FaceResult;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

pub trait Camera: Send {
    /// Errors are reported as `FaceResult::Unknown` from `probe`, never here,
    /// so callers cannot accidentally treat a failure as absence.
    fn open(&mut self);
    fn close(&mut self);
    /// Grab one frame and detect. Returns `Unknown` on any failure.
    fn probe(&mut self) -> FaceResult;
}

pub struct FakeCamera {
    script: Mutex<std::vec::IntoIter<FaceResult>>,
    opens: Arc<AtomicUsize>,
    closes: Arc<AtomicUsize>,
}

impl FakeCamera {
    pub fn new(script: Vec<FaceResult>) -> Self {
        Self {
            script: Mutex::new(script.into_iter()),
            opens: Arc::new(AtomicUsize::new(0)),
            closes: Arc::new(AtomicUsize::new(0)),
        }
    }
    pub fn open_count(&self) -> Arc<AtomicUsize> {
        self.opens.clone()
    }
    pub fn close_count(&self) -> Arc<AtomicUsize> {
        self.closes.clone()
    }
}

impl Camera for FakeCamera {
    fn open(&mut self) {
        self.opens.fetch_add(1, Ordering::Relaxed);
    }
    fn close(&mut self) {
        self.closes.fetch_add(1, Ordering::Relaxed);
    }
    fn probe(&mut self) -> FaceResult {
        self.script
            .lock()
            .unwrap()
            .next()
            .unwrap_or(FaceResult::NoFace)
    }
}
