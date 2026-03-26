//! SVG icon rendering for the system tray.
//!
//! Rasterizes the Omnidea pinwheel SVG into RGBA pixel buffers at various
//! rotation angles, producing pre-rendered frames for smooth spin animation.

use resvg::tiny_skia;
use resvg::usvg;

/// Icon size in pixels (22pt x 2 for Retina).
const ICON_SIZE: u32 = 44;

/// Number of rotation frames for the spin animation.
const FRAME_COUNT: usize = 12;

/// Pre-rendered icon frames for tray icon animation.
///
/// Contains a static (non-rotated) frame for idle state and 12 rotation
/// frames for the spinning animation. All frames are non-premultiplied RGBA.
pub struct IconFrames {
    /// Static frame (no rotation) -- used when idle.
    pub static_frame: Vec<u8>,
    /// Rotation frames for spin animation (12 frames, 30 degrees apart).
    pub spin_frames: Vec<Vec<u8>>,
    /// Pixel width of each frame.
    pub width: u32,
    /// Pixel height of each frame.
    pub height: u32,
}

impl IconFrames {
    /// Render all frames from the embedded pinwheel SVG.
    ///
    /// Parses the black pinwheel SVG (suitable for macOS template icon / light mode)
    /// and rasterizes it at `ICON_SIZE` x `ICON_SIZE` pixels with 12 rotation steps.
    pub fn render() -> Result<Self, IconError> {
        let svg_data = include_bytes!("../assets/pinwheel_black.svg");

        let tree = usvg::Tree::from_data(svg_data, &usvg::Options::default())
            .map_err(IconError::SvgParse)?;

        let static_frame = render_frame(&tree, 0.0)?;

        let mut spin_frames = Vec::with_capacity(FRAME_COUNT);
        for i in 0..FRAME_COUNT {
            let angle = (i as f32) * (360.0 / FRAME_COUNT as f32);
            spin_frames.push(render_frame(&tree, angle)?);
        }

        Ok(Self {
            static_frame,
            spin_frames,
            width: ICON_SIZE,
            height: ICON_SIZE,
        })
    }
}

/// Rasterize the SVG tree at the given rotation angle (in degrees).
///
/// The SVG (512x512) is scaled down to `ICON_SIZE` and rotated around its center.
/// Returns non-premultiplied RGBA pixel data.
fn render_frame(tree: &usvg::Tree, angle_degrees: f32) -> Result<Vec<u8>, IconError> {
    let size = ICON_SIZE;
    let mut pixmap =
        tiny_skia::Pixmap::new(size, size).ok_or(IconError::PixmapCreation)?;

    // The SVG viewBox is 512x512. We need to:
    // 1. Translate SVG center (256, 256) to origin
    // 2. Rotate by the desired angle
    // 3. Translate back to center
    // 4. Scale from 512 to ICON_SIZE
    // All composed in SVG-to-pixel order.
    let svg_center = 256.0;
    let scale = size as f32 / 512.0;

    let transform = tiny_skia::Transform::from_scale(scale, scale)
        .pre_translate(svg_center, svg_center)
        .pre_rotate(angle_degrees)
        .pre_translate(-svg_center, -svg_center);

    resvg::render(tree, transform, &mut pixmap.as_mut());

    // resvg produces premultiplied RGBA; tray-icon expects non-premultiplied.
    let mut pixels = pixmap.take();
    unpremultiply(&mut pixels);

    Ok(pixels)
}

/// Convert premultiplied RGBA pixels to non-premultiplied (straight) RGBA.
///
/// `tray-icon::Icon::from_rgba` expects non-premultiplied data, but `tiny_skia`
/// works internally with premultiplied alpha. This function reverses that.
fn unpremultiply(data: &mut [u8]) {
    for chunk in data.chunks_exact_mut(4) {
        let a = chunk[3] as f32 / 255.0;
        if a > 0.0 {
            chunk[0] = (chunk[0] as f32 / a).min(255.0) as u8;
            chunk[1] = (chunk[1] as f32 / a).min(255.0) as u8;
            chunk[2] = (chunk[2] as f32 / a).min(255.0) as u8;
        }
    }
}

/// Errors that can occur during icon rendering.
#[derive(Debug)]
pub enum IconError {
    /// Failed to parse the embedded SVG.
    SvgParse(usvg::Error),
    /// Failed to create a pixel buffer (zero-size dimensions).
    PixmapCreation,
}

impl std::fmt::Display for IconError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IconError::SvgParse(e) => write!(f, "SVG parse error: {e}"),
            IconError::PixmapCreation => write!(f, "failed to create pixmap"),
        }
    }
}

impl std::error::Error for IconError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            IconError::SvgParse(e) => Some(e),
            IconError::PixmapCreation => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_produces_correct_dimensions() {
        let frames = IconFrames::render().expect("should render frames");
        let expected_bytes = (ICON_SIZE * ICON_SIZE * 4) as usize;

        assert_eq!(frames.width, ICON_SIZE);
        assert_eq!(frames.height, ICON_SIZE);
        assert_eq!(frames.static_frame.len(), expected_bytes);
    }

    #[test]
    fn test_render_produces_correct_frame_count() {
        let frames = IconFrames::render().expect("should render frames");
        assert_eq!(frames.spin_frames.len(), FRAME_COUNT);

        let expected_bytes = (ICON_SIZE * ICON_SIZE * 4) as usize;
        for (i, frame) in frames.spin_frames.iter().enumerate() {
            assert_eq!(
                frame.len(),
                expected_bytes,
                "frame {i} has wrong byte count"
            );
        }
    }

    #[test]
    fn test_static_frame_has_nonzero_pixels() {
        let frames = IconFrames::render().expect("should render frames");
        // The pinwheel should produce at least some non-transparent pixels
        let has_content = frames
            .static_frame
            .chunks_exact(4)
            .any(|px| px[3] > 0);
        assert!(has_content, "static frame should have visible pixels");
    }

    #[test]
    fn test_rotated_frames_differ_from_static() {
        let frames = IconFrames::render().expect("should render frames");
        // Frame at 90 degrees (index 3) should differ from the static frame
        assert_ne!(
            frames.static_frame, frames.spin_frames[3],
            "90-degree frame should differ from static"
        );
    }

    #[test]
    fn test_unpremultiply_fully_opaque() {
        let mut data = vec![128, 64, 32, 255];
        unpremultiply(&mut data);
        // Fully opaque pixels should be unchanged
        assert_eq!(data, vec![128, 64, 32, 255]);
    }

    #[test]
    fn test_unpremultiply_half_alpha() {
        // Premultiplied: R=64, G=32, B=16 with A=128 (0.502)
        // Non-premultiplied: R=127, G=63, B=31
        let mut data = vec![64, 32, 16, 128];
        unpremultiply(&mut data);
        assert_eq!(data[3], 128, "alpha unchanged");
        // Allow rounding tolerance
        assert!((data[0] as i32 - 127).unsigned_abs() <= 1, "R: got {}", data[0]);
        assert!((data[1] as i32 - 63).unsigned_abs() <= 1, "G: got {}", data[1]);
        assert!((data[2] as i32 - 31).unsigned_abs() <= 1, "B: got {}", data[2]);
    }

    #[test]
    fn test_unpremultiply_transparent() {
        let mut data = vec![0, 0, 0, 0];
        unpremultiply(&mut data);
        // Transparent pixels should remain zero
        assert_eq!(data, vec![0, 0, 0, 0]);
    }
}
