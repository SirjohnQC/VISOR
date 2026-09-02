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
    /// Run a short probe and report whether the camera can actually see the
    /// user. Every way this can fail is otherwise silent — a covered lens, a
    /// bad angle, or a face below `min_face_ratio` all just look like absence.
    CheckCamera,
    /// Hold the camera open and stream preview frames for the tuning window,
    /// or stop doing so.
    ///
    /// This is not a state-machine command -- `Machine::command` ignores it.
    /// The engine handles it alone, because what it changes is what the engine
    /// does with the camera, not what the machine believes about presence.
    SetPreview(bool),
    Quit,
}
