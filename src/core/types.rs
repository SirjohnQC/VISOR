use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Active,
    Watching,
    Dimmed,
    Away,
    Deep,
    Paused,
    Degraded,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DisplayLevel {
    Full,
    /// Percent of the user's saved brightness.
    Dim(u8),
    /// Pixels off, panel still powered.
    Black,
    /// Panel powered down (DPMS).
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FaceResult {
    Face {
        count: u8,
        largest_ratio: f32,
    },
    NoFace,
    /// Camera or detector error. Spec §4.7 — never causes a step down.
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Effect {
    OpenCamera,
    CloseCamera,
    SetSampleInterval(Duration),
    SetDisplay(DisplayLevel),
    SetAwakeHold(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Pause,
    Resume,
    Reload,
    Quit,
}
