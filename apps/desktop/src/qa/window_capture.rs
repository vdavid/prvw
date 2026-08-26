//! The debug-only `screenshot_window` MCP tool: the whole native window as a person sees it.
//!
//! Distinct from the `screenshot` tool next to it, which re-renders the image quad into an
//! offscreen wgpu target. That one is portable and pixel-exact about the image, and it draws no
//! overlays, no title bar, and no window chrome. This one photographs the window: the title
//! strip, the zoom pill, the EXIF panel, the histogram, the traffic lights, whatever a modal put
//! on top. Visual QA needs the second kind, which is why both exist.
//!
//! Both platforms return PNG bytes, so `mcp.rs` has one arm and one contract above this module.
//! macOS shells out to `screencapture`, which already hands back a PNG. Windows asks the window
//! to draw itself into a bitmap, so the PNG gets encoded here.

use crate::commands::AppCommand;
use std::sync::mpsc;
use std::time::Duration;
use winit::event_loop::EventLoopProxy;

/// How long to wait for the event loop to answer with the window's id.
const WINDOW_ID_TIMEOUT: Duration = Duration::from_secs(2);

/// Capture the main viewer window and return PNG bytes.
pub(super) fn capture_main_window(proxy: &EventLoopProxy<AppCommand>) -> Result<Vec<u8>, String> {
    let (tx, rx) = mpsc::channel();
    proxy
        .send_event(AppCommand::GetNativeWindowId(tx))
        .map_err(|_| "The event loop has closed.".to_string())?;
    let window_id = rx
        .recv_timeout(WINDOW_ID_TIMEOUT)
        .map_err(|_| "The event loop didn't answer with a window id.".to_string())?;
    if window_id == 0 {
        return Err("The main window isn't ready yet.".to_string());
    }

    capture_native(window_id)
}

/// Photograph the window through `/usr/sbin/screencapture -l <windowNumber>`. Cheaper to
/// maintain than the `CGWindowListCreateImage` FFI dance, which is also deprecated as of macOS
/// 14.4. The 300 to 500 ms spawn cost is fine for a QA tool.
///
/// Needs Screen Recording permission. macOS prompts on first use, and the first call may come
/// back black until someone grants it.
#[cfg(target_os = "macos")]
fn capture_native(window_number: u64) -> Result<Vec<u8>, String> {
    use std::process::Command;

    // `-o` no shadow, `-x` silent (no shutter sound), `-t png`, file path last. Skip stdout
    // capture: `screencapture` writes a fixed string we don't need, and ignoring it dodges
    // interleaving.
    let tmp = std::env::temp_dir().join(format!(
        "prvw-screenshot-window-{}-{window_number}.png",
        std::process::id()
    ));
    let output = Command::new("/usr/sbin/screencapture")
        .args(["-l", &window_number.to_string(), "-o", "-x", "-t", "png"])
        .arg(&tmp)
        .output()
        .map_err(|why| format!("Couldn't run screencapture: {why}"))?;
    if !output.status.success() {
        let _ = std::fs::remove_file(&tmp);
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "screencapture exited with {}: {stderr}",
            output.status
        ));
    }

    let png = std::fs::read(&tmp).map_err(|why| format!("Couldn't read the screenshot: {why}"))?;
    let _ = std::fs::remove_file(&tmp);
    if png.is_empty() {
        return Err(
            "The screenshot came back empty. Grant Screen Recording permission to prvw and try again."
                .to_string(),
        );
    }
    Ok(png)
}

/// Ask the window to draw itself into a bitmap, then encode that. See
/// `platform::windows::window_capture` for why it's `PrintWindow` and not a screen blit.
#[cfg(target_os = "windows")]
fn capture_native(hwnd: u64) -> Result<Vec<u8>, String> {
    let frame = crate::platform::windows::window_capture::capture(hwnd)?;
    bgra_frame_to_png(&frame.bgra, frame.width, frame.height)
}

/// Turn a top-down 32-bit BGRA frame into PNG bytes.
///
/// Two fixups, both of which a GDI-blitted DIB needs. The channel order is BGRA where PNG wants
/// RGBA, and GDI writes nothing to the alpha byte, so a straight encode gives a fully
/// transparent image. A window photograph has nothing to be transparent against, so every pixel
/// is forced opaque.
// Windows is the only caller; it compiles everywhere so the tests below can run on any host,
// which is the only way this gets checked at all before it meets a Windows box.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn bgra_frame_to_png(bgra: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    use image::ImageEncoder;

    if width == 0 || height == 0 {
        return Err("The window has no size to capture.".to_string());
    }
    let expected = width as usize * height as usize * 4;
    if bgra.len() != expected {
        return Err(format!(
            "The captured frame is {} bytes, but {width}x{height} needs {expected}.",
            bgra.len()
        ));
    }

    let mut rgba = bgra.to_vec();
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2);
        pixel[3] = 0xff;
    }

    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(&rgba, width, height, image::ColorType::Rgba8.into())
        .map_err(|why| format!("Couldn't encode the window as a PNG: {why}"))?;
    Ok(png)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One pixel per byte-pattern, in the BGRA order GDI writes and with the zero alpha it
    /// leaves behind.
    fn sample_frame() -> Vec<u8> {
        vec![
            0x11, 0x22, 0x33, 0x00, // B=0x11 G=0x22 R=0x33
            0xff, 0x00, 0x00, 0x00, // pure blue
        ]
    }

    fn decode(png: &[u8]) -> image::RgbaImage {
        image::load_from_memory_with_format(png, image::ImageFormat::Png)
            .expect("the encoder should produce a readable PNG")
            .to_rgba8()
    }

    #[test]
    fn the_channels_come_back_in_png_order() {
        let png = bgra_frame_to_png(&sample_frame(), 2, 1).expect("a 2x1 frame should encode");
        let decoded = decode(&png);
        assert_eq!(decoded.get_pixel(0, 0).0, [0x33, 0x22, 0x11, 0xff]);
        assert_eq!(decoded.get_pixel(1, 0).0, [0x00, 0x00, 0xff, 0xff]);
    }

    #[test]
    fn a_frame_gdi_left_transparent_comes_back_opaque() {
        let png = bgra_frame_to_png(&sample_frame(), 2, 1).expect("a 2x1 frame should encode");
        assert!(decode(&png).pixels().all(|pixel| pixel.0[3] == 0xff));
    }

    #[test]
    fn the_result_is_a_png_the_mcp_contract_can_carry() {
        let png = bgra_frame_to_png(&sample_frame(), 2, 1).expect("a 2x1 frame should encode");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn a_frame_that_doesnt_match_its_size_is_refused() {
        let why = bgra_frame_to_png(&sample_frame(), 4, 1).expect_err("2 pixels aren't 4");
        assert!(why.contains("8 bytes"), "unhelpful message: {why}");
    }

    #[test]
    fn a_window_with_no_size_is_refused() {
        assert!(bgra_frame_to_png(&[], 0, 0).is_err());
    }
}
