//! Preview frames — what the tuning window draws.
//!
//! Pure data and pure logic. The camera adapter fills these in; nothing here
//! touches WinRT, so the awkward part (row stride) is testable without a
//! webcam.
//!
//! A preview frame is **luminance, one byte per pixel**. That is not a
//! compromise for bandwidth, it is what the design asks to be drawn: the
//! window renders the preview in grey to say *we are measuring shape, not
//! looking at you*. It also happens to be nearly free: the adapter already
//! converts every frame for the detector, and plane 0 of that conversion is
//! luminance whether the detector asked for `Gray8` or `NV12`, so the preview
//! is a copy of bytes that had to exist anyway.

/// A detected face, in source-frame pixels.
///
/// `probe` keeps only `height / frame_height` because that is all the state
/// machine needs. The window needs the whole rectangle to draw a box around
/// the person, so the preview path carries all four numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FaceBox {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// One frame for the preview, as plain `Send` data.
///
/// This is what crosses the thread boundary. `WinRtCamera` is `!Send` and
/// lives on the engine thread; the window lives on the pump thread (the same
/// rule that put the overlay windows there). So the hardware stays put and
/// only bytes travel — the mirror image of how `DisplayLevel`s already flow
/// the other way through `ChannelDisplay`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewFrame {
    pub width: u32,
    pub height: u32,
    /// Luminance, `width * height` bytes, no row padding.
    pub luma: Vec<u8>,
    pub faces: Vec<FaceBox>,
}

impl PreviewFrame {
    /// The ratio the state machine would compute from this frame, so the
    /// window can show the same number the machine acts on rather than a
    /// separately-derived one that could drift from it.
    pub fn largest_ratio(&self) -> Option<f32> {
        if self.height == 0 {
            return None;
        }
        self.faces
            .iter()
            .map(|f| f.h as f32 / self.height as f32)
            .fold(None, |acc: Option<f32>, r| {
                Some(acc.map_or(r, |a| a.max(r)))
            })
    }
}

/// Copy a strided bitmap buffer into a tightly packed one.
///
/// WinRT hands back rows padded to an alignment boundary, so a 641-pixel-wide
/// `Gray8` frame arrives with rows 644 bytes apart. Treating the buffer as
/// `width * height` contiguous bytes shifts every row a little further left
/// than the last and shears the picture diagonally — which looks like a broken
/// camera rather than a broken index, so it is worth pinning down here.
///
/// Returns `None` when the buffer cannot hold the frame it claims to be.
pub fn tighten_rows(buf: &[u8], stride: usize, width: u32, height: u32) -> Option<Vec<u8>> {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 || stride < w || buf.len() < stride * h {
        return None;
    }
    let mut out = Vec::with_capacity(w * h);
    for row in 0..h {
        let start = row * stride;
        out.extend_from_slice(&buf[start..start + w]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_padded_buffer_is_packed_without_shearing() {
        // 3x2 image whose rows are 5 bytes apart. The padding bytes (9) must
        // not appear in the output, and row 1 must start at output index 3 --
        // an off-by-stride here is exactly the diagonal shear.
        let buf = [1, 2, 3, 9, 9, 4, 5, 6, 9, 9];
        let out = tighten_rows(&buf, 5, 3, 2).unwrap();
        assert_eq!(out, vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn an_unpadded_buffer_is_copied_unchanged() {
        let buf = [1, 2, 3, 4, 5, 6];
        assert_eq!(tighten_rows(&buf, 3, 3, 2).unwrap(), vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn a_buffer_too_short_for_its_frame_is_refused() {
        // Fail toward no preview rather than reading past the end or drawing
        // garbage: the window shows the last good frame instead.
        assert_eq!(tighten_rows(&[1, 2, 3], 3, 3, 2), None);
        assert_eq!(
            tighten_rows(&[1, 2, 3, 4, 5, 6], 2, 3, 2),
            None,
            "stride < width"
        );
        assert_eq!(
            tighten_rows(&[], 0, 0, 0),
            None,
            "an empty frame is not a frame"
        );
    }

    #[test]
    fn the_preview_reports_the_same_ratio_the_machine_would() {
        // largest_ratio is FaceBox.Height / frame height (spec §4.2). If the
        // window derived this differently it could show a number that clears
        // the threshold while the machine dims anyway -- the exact confusion
        // the tuning window exists to end.
        let f = PreviewFrame {
            width: 320,
            height: 240,
            luma: vec![0; 320 * 240],
            faces: vec![
                FaceBox {
                    x: 10,
                    y: 10,
                    w: 20,
                    h: 24,
                },
                FaceBox {
                    x: 90,
                    y: 10,
                    w: 30,
                    h: 48,
                },
            ],
        };
        assert_eq!(f.largest_ratio(), Some(0.2), "the largest face wins");
    }

    #[test]
    fn a_frame_with_no_faces_has_no_ratio() {
        let f = PreviewFrame {
            width: 4,
            height: 4,
            luma: vec![0; 16],
            faces: Vec::new(),
        };
        assert_eq!(f.largest_ratio(), None);
    }
}
