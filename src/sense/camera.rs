use crate::core::types::FaceResult;
use crate::sense::preview::{FaceBox, PreviewFrame, tighten_rows};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use windows::Graphics::Imaging::{BitmapBufferAccessMode, BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Capture::Frames::{MediaFrameReader, MediaFrameSource, MediaFrameSourceKind};
use windows::Media::Capture::{
    MediaCapture, MediaCaptureInitializationSettings, MediaCaptureMemoryPreference,
    StreamingCaptureMode,
};
use windows::Media::FaceAnalysis::FaceDetector;
use windows::Storage::Streams::{Buffer, DataReader};
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

    /// Turn preview capture on or off.
    ///
    /// Off by default, and deliberately so: with preview off `probe` does
    /// exactly the work it did before the tuning window existed, so the
    /// background cost of VISOR is unchanged for the 99% of its life when
    /// nobody is looking at a window. Copying a frame out is only worth
    /// paying for while something is drawing it.
    fn set_preview(&mut self, _on: bool) {}

    /// The most recent preview frame, consumed. `None` unless preview is on
    /// and a frame was captured since the last call.
    fn take_preview(&mut self) -> Option<PreviewFrame> {
        None
    }
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
    preview: bool,
    last_preview: Option<PreviewFrame>,
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
            preview: false,
            last_preview: None,
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

        // largest_ratio is FaceBox.Height / frame height (spec §4.2). Do not
        // filter by ratio here -- `min_face_ratio` is applied by the state
        // machine, not the camera adapter. The rectangles are collected in the
        // same pass because the preview needs all four numbers, not just the
        // ratio, to draw a box around the person.
        let mut largest = 0.0f32;
        let mut boxes = Vec::new();
        for face in &faces {
            let bounds = face.FaceBox()?;
            largest = largest.max(bounds.Height as f32 / height);
            if self.preview {
                boxes.push(FaceBox {
                    x: bounds.X,
                    y: bounds.Y,
                    w: bounds.Width,
                    h: bounds.Height,
                });
            }
        }

        // Captured BEFORE the no-face return below: "the camera works but
        // sees nobody" is a state the window has to be able to show video in,
        // and it is the state a user tuning their threshold sits in most.
        if self.preview {
            match Self::grab_preview(&converted, boxes) {
                Ok(frame) => self.last_preview = Some(frame),
                // A preview failure must never change what the machine is
                // told about presence -- the window going blank is survivable,
                // the screen blanking in the user's face is not.
                Err(e) => tracing::debug!(error = %e, "preview frame capture failed"),
            }
        }

        if count == 0 {
            return Ok(FaceResult::NoFace);
        }
        Ok(FaceResult::Face {
            count,
            largest_ratio: largest,
        })
    }

    /// Copy the already-converted `Gray8` bitmap out as plain bytes.
    ///
    /// The detector requires `Gray8`, so this buffer had to be built anyway --
    /// the preview is a copy of it rather than a second conversion. That is
    /// also what the window wants to draw: luminance, not colour.
    fn grab_preview(
        gray: &SoftwareBitmap,
        faces: Vec<FaceBox>,
    ) -> windows::core::Result<PreviewFrame> {
        let width = gray.PixelWidth()? as u32;
        let height = gray.PixelHeight()? as u32;

        // Walk the bitmap's planes. The detector does not necessarily hand
        // back Gray8: `GetSupportedBitmapPixelFormats` reports NV12 first on
        // this hardware, which is planar -- a full-size luminance plane
        // followed by a half-height interleaved chroma plane. The buffer has
        // to be big enough for every plane or `CopyToBuffer` refuses with
        // MF_E_BUFFERTOOSMALL, but only plane 0 is ever drawn: it is
        // luminance in NV12 and in Gray8 alike, which is precisely the grey
        // picture the preview wants.
        let (start_index, stride, total) = {
            let locked = gray.LockBuffer(BitmapBufferAccessMode::Read)?;
            let mut first: Option<(usize, usize)> = None;
            let mut total = 0usize;
            for plane in 0..4 {
                let Ok(d) = locked.GetPlaneDescription(plane) else {
                    break;
                };
                let end = d.StartIndex as usize + (d.Stride as usize) * (d.Height as usize);
                total = total.max(end);
                if plane == 0 {
                    first = Some((d.StartIndex as usize, d.Stride as usize));
                }
            }
            let _ = locked.Close();
            match first {
                Some((s, st)) if st >= width as usize && total > 0 => (s, st, total),
                // No usable plane description means no honest way to index the
                // bytes. Fail toward no preview -- the window keeps its last
                // good frame -- rather than drawing a sheared guess.
                _ => {
                    return Err(windows::core::Error::new(
                        windows::Win32::Foundation::E_FAIL,
                        "bitmap did not describe a usable luminance plane",
                    ));
                }
            }
        };

        let buffer = Buffer::Create(total as u32)?;
        // `Buffer::Create` sets CAPACITY but leaves Length at 0, and
        // `CopyToBuffer` validates against Length, so without this the copy
        // fails however much room was reserved.
        buffer.SetLength(total as u32)?;
        gray.CopyToBuffer(&buffer)?;
        let reader = DataReader::FromBuffer(&buffer)?;
        let len = reader.UnconsumedBufferLength()? as usize;
        let mut bytes = vec![0u8; len];
        reader.ReadBytes(&mut bytes)?;

        let luma = bytes
            .get(start_index..)
            .and_then(|plane| tighten_rows(plane, stride, width, height))
            .ok_or_else(|| {
                windows::core::Error::new(
                    windows::Win32::Foundation::E_FAIL,
                    "preview buffer smaller than the frame it describes",
                )
            })?;

        Ok(PreviewFrame {
            width,
            height,
            luma,
            faces,
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
        // A frame from before the lens shut is not a preview of anything.
        // Dropping it here is what stops the window showing a stale picture
        // of the user next to a chip that says the camera is off.
        self.last_preview = None;
    }

    fn set_preview(&mut self, on: bool) {
        self.preview = on;
        if !on {
            self.last_preview = None;
        }
    }

    fn take_preview(&mut self) -> Option<PreviewFrame> {
        self.last_preview.take()
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
    preview: bool,
    frames: Arc<AtomicUsize>,
}

impl FakeCamera {
    pub fn new(script: Vec<FaceResult>) -> Self {
        Self {
            script: Mutex::new(script.into_iter()),
            opens: Arc::new(AtomicUsize::new(0)),
            closes: Arc::new(AtomicUsize::new(0)),
            preview: false,
            frames: Arc::new(AtomicUsize::new(0)),
        }
    }
    /// How many preview frames have been handed out.
    pub fn frame_count(&self) -> Arc<AtomicUsize> {
        self.frames.clone()
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
    fn set_preview(&mut self, on: bool) {
        self.preview = on;
    }
    fn take_preview(&mut self) -> Option<PreviewFrame> {
        if !self.preview {
            return None;
        }
        self.frames.fetch_add(1, Ordering::Relaxed);
        // A 4x4 mid-grey frame with one face box half the frame high, so a
        // consumer can assert a ratio of 0.5 without a webcam.
        Some(PreviewFrame {
            width: 4,
            height: 4,
            luma: vec![128; 16],
            faces: vec![FaceBox {
                x: 1,
                y: 1,
                w: 2,
                h: 2,
            }],
        })
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
