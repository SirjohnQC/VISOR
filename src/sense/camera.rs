use crate::core::types::FaceResult;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Capture::Frames::{MediaFrameReader, MediaFrameSource, MediaFrameSourceKind};
use windows::Media::Capture::{
    MediaCapture, MediaCaptureInitializationSettings, MediaCaptureMemoryPreference,
    StreamingCaptureMode,
};
use windows::Media::FaceAnalysis::FaceDetector;
use windows::core::HSTRING;

/// Deliberately **not** `Camera: Send`. `WinRtCamera` holds a `MediaCapture`,
/// which is not `Send` (verified in task 12: `windows-rs` gives
/// `Windows.Media.Capture.Frames.MediaFrameReader` and
/// `Windows.Media.FaceAnalysis.FaceDetector` explicit `unsafe impl Send`, but
/// grants `MediaCapture` no such impl, so `Option<MediaCapture>` is `!Send`
/// even while the option is `None` -- the same shape of problem task 10 hit
/// with `PHYSICAL_MONITOR`). `Engine`, which owns a `Box<dyn Camera>`, is
/// therefore never moved across a thread boundary as a value either: `main`
/// constructs both the camera and the `Engine` inside the spawned thread's
/// closure (ruling F10) so nothing here ever needs to cross threads.
pub trait Camera {
    /// Errors are reported as `FaceResult::Unknown` from `probe`, never here,
    /// so callers cannot accidentally treat a failure as absence.
    fn open(&mut self);
    fn close(&mut self);
    /// Grab one frame and detect. Returns `Unknown` on any failure.
    fn probe(&mut self) -> FaceResult;
}

/// Local webcam presence detection via `Windows.Media.Capture` and
/// `Windows.Media.FaceAnalysis` (spec §5.1) -- no bundled model, no extra
/// dependency, and nothing that leaves this machine: a frame is decoded,
/// handed to the detector for a count and a bounding-box ratio, and dropped.
/// It is never written to disk or encoded (spec §2.1, "no images leave this
/// machine" is the whole point of routing vision through WinRT).
///
/// `capture`/`reader` hold real WinRT objects only while the camera is open;
/// both are `None` otherwise. `MediaCapture` is not `Send` (see the `Camera`
/// module doc / task report), so a `WinRtCamera` must be constructed and used
/// entirely on one thread.
pub struct WinRtCamera {
    device: String,
    capture: Option<MediaCapture>,
    reader: Option<MediaFrameReader>,
    detector: Option<FaceDetector>,
    format: BitmapPixelFormat,
}

impl WinRtCamera {
    /// `device` is a video device id (`MediaCaptureInitializationSettings::VideoDeviceId`);
    /// an empty string lets Windows pick the default camera.
    pub fn new(device: &str) -> Self {
        Self {
            device: device.to_string(),
            capture: None,
            reader: None,
            detector: None,
            format: BitmapPixelFormat::Gray8,
        }
    }

    /// All the fallible setup in one place so `open` can swallow errors and
    /// leave the camera closed -- `probe` then reports `Unknown` (spec §2.1,
    /// §4.7). Nothing here is hard-coded that the platform can tell us
    /// instead: the pixel format comes from `FaceDetector::GetSupportedBitmapPixelFormats`,
    /// and the frame source is selected by kind rather than assumed to be
    /// first (ruling F11 -- a Windows Hello IR/depth source is often first).
    fn try_open(&mut self) -> windows::core::Result<()> {
        let detector = FaceDetector::CreateAsync()?.get()?;

        let supported = FaceDetector::GetSupportedBitmapPixelFormats()?;
        self.format = if supported.Size()? > 0 {
            supported.GetAt(0)?
        } else {
            BitmapPixelFormat::Gray8
        };

        let capture = MediaCapture::new()?;
        let settings = MediaCaptureInitializationSettings::new()?;
        settings.SetStreamingCaptureMode(StreamingCaptureMode::Video)?;
        settings.SetMemoryPreference(MediaCaptureMemoryPreference::Cpu)?;
        if !self.device.is_empty() {
            settings.SetVideoDeviceId(&HSTRING::from(&self.device))?;
        }
        capture.InitializeWithSettingsAsync(&settings)?.get()?;

        // Ruling F11: pick a colour source. `FrameSources()` can list an
        // infrared or depth stream first on Windows Hello hardware, and face
        // detection on those silently misbehaves.
        let mut colour: Option<MediaFrameSource> = None;
        for kv in &capture.FrameSources()? {
            let source = kv.Value()?;
            if source.Info()?.SourceKind()? == MediaFrameSourceKind::Color {
                colour = Some(source);
                break;
            }
        }
        let Some(source) = colour else {
            return Err(windows::core::Error::new(
                windows::Win32::Foundation::E_FAIL,
                "no colour frame source on this camera",
            ));
        };

        let reader = capture.CreateFrameReaderAsync(&source)?.get()?;
        reader.StartAsync()?.get()?;

        self.detector = Some(detector);
        self.capture = Some(capture);
        self.reader = Some(reader);
        Ok(())
    }

    /// One frame in, one `FaceResult` out; the `SoftwareBitmap` never
    /// outlives this call.
    fn try_probe(&mut self) -> windows::core::Result<FaceResult> {
        let Some(reader) = self.reader.as_ref() else {
            return Ok(FaceResult::Unknown);
        };
        // A null/empty result here (no frame ready yet) surfaces as `Err`
        // from the WinRT projection (see windows-core's `Type::from_abi`),
        // which we fold into `Unknown` rather than treating as a hard error.
        let Ok(frame) = reader.TryAcquireLatestFrame() else {
            return Ok(FaceResult::Unknown);
        };
        let bitmap = frame.VideoMediaFrame()?.SoftwareBitmap()?;
        let converted = SoftwareBitmap::Convert(&bitmap, self.format)?;
        let height = converted.PixelHeight()? as f32;

        let detector = self
            .detector
            .as_ref()
            .expect("detector is set whenever reader is");
        let faces = detector.DetectFacesAsync(&converted)?.get()?;
        let count = faces.Size()? as u8;
        if count == 0 {
            return Ok(FaceResult::NoFace);
        }

        // largest_ratio is FaceBox.Height / frame height (spec §4.2). Do not
        // filter by ratio here -- `min_face_ratio` is applied by the state
        // machine, not the camera adapter.
        let mut largest = 0.0f32;
        for face in &faces {
            let bounds = face.FaceBox()?;
            largest = largest.max(bounds.Height as f32 / height);
        }
        Ok(FaceResult::Face {
            count,
            largest_ratio: largest,
        })
    }
}

impl Camera for WinRtCamera {
    fn open(&mut self) {
        if let Err(e) = self.try_open() {
            tracing::warn!(error = %e, "camera open failed");
            self.close();
        }
    }

    fn close(&mut self) {
        if let Some(reader) = self.reader.take()
            && let Ok(op) = reader.StopAsync()
        {
            let _ = op.get();
        }
        if let Some(capture) = self.capture.take() {
            let _ = capture.Close();
        }
        self.detector = None;
    }

    /// Never returns an error -- a failure is `Unknown`, which the state
    /// machine treats as fail-safe presence (spec §2.1, §4.7), never absence.
    fn probe(&mut self) -> FaceResult {
        match self.try_probe() {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "camera probe failed");
                FaceResult::Unknown
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    // Spec §2.1: the camera is shut unless it is needed, and a closed or
    // failed camera must never look like absence — that would make VISOR
    // blank the screen out from under someone whose webcam simply failed to
    // open. Neither test below opens a real camera, so both run in CI.

    #[test]
    fn probing_a_closed_camera_reports_unknown_not_absence() {
        let mut cam = WinRtCamera::new("");
        assert_eq!(
            cam.probe(),
            FaceResult::Unknown,
            "spec §2.1: a closed camera must never look like absence"
        );
    }

    #[test]
    fn close_is_idempotent_and_safe_before_open() {
        let mut cam = WinRtCamera::new("");
        cam.close();
        cam.close();
        assert_eq!(cam.probe(), FaceResult::Unknown);
    }
}
