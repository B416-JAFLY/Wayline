//! Borderless GPUI surfaces keep compositor alpha instead of a binary HWND clip.
use std::mem::size_of_val;
use windows::Win32::{
    Foundation::*,
    Graphics::{Dwm::*, Gdi::*},
    UI::{HiDpi::GetDpiForWindow, WindowsAndMessaging::*},
};

pub const CORNER_RADIUS: f32 = 28.;

/// Called on the owning UI thread before a frame-changing SetWindowPos.
pub fn configure(hwnd: HWND, width: i32, height: i32) {
    // SAFETY: callers pass their live GPUI HWND on its owning thread. No driver
    // buffers or borrowed pointers survive any of these synchronous API calls.
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
        SetWindowLongPtrW(
            hwnd,
            GWL_STYLE,
            ((style & !WS_OVERLAPPEDWINDOW.0) | WS_POPUP.0) as isize,
        );
        let policy = DWMNCRP_DISABLED;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_NCRENDERING_POLICY,
            &policy as *const _ as _,
            size_of_val(&policy) as u32,
        );
        let corners = DWMWCP_DONOTROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corners as *const _ as _,
            size_of_val(&corners) as u32,
        );
        let border = 0xfffffffeu32; // DWMWA_COLOR_NONE
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            &border as *const _ as _,
            size_of_val(&border) as u32,
        );
        let composited =
            GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_NOREDIRECTIONBITMAP.0 != 0;
        if composited {
            // GPUI's DirectComposition swapchain already supplies per-pixel
            // alpha. An HRGN would discard its antialiased corner pixels.
            let _ = SetWindowRgn(hwnd, None, true);
        } else {
            // Preserve a usable shape if GPUI DirectComposition was explicitly
            // disabled: its opaque fallback cannot show transparent corners.
            let scale = GetDpiForWindow(hwnd).max(96) as f32 / 96.;
            let diameter = (CORNER_RADIUS * 2. * scale).round() as i32;
            let region = CreateRoundRectRgn(0, 0, width + 1, height + 1, diameter, diameter);
            if SetWindowRgn(hwnd, Some(region), true) == 0 {
                let _ = DeleteObject(region.into());
            }
        }
    }
}

fn contains(x: f32, y: f32, width: f32, height: f32, radius: f32) -> bool {
    if x < 0. || y < 0. || x >= width || y >= height {
        return false;
    }
    let radius = radius.min(width / 2.).min(height / 2.);
    let dx = x - x.clamp(radius, width - radius);
    let dy = y - y.clamp(radius, height - radius);
    dx * dx + dy * dy <= radius * radius
}

/// Ignore mouse hits in the transparent portions of the rectangular HWND.
pub fn transparent_corner(hwnd: HWND, location: LPARAM) -> bool {
    let mut point = POINT {
        x: location.0 as u16 as i16 as i32,
        y: (location.0 as u32 >> 16) as u16 as i16 as i32,
    };
    let mut rect = RECT::default();
    // SAFETY: Win32 writes only to these live stack values. Signed coordinates
    // are preserved for windows on monitors left/above the primary monitor.
    unsafe {
        if !ScreenToClient(hwnd, &mut point).as_bool() || GetClientRect(hwnd, &mut rect).is_err() {
            return false;
        }
        let radius = CORNER_RADIUS * GetDpiForWindow(hwnd).max(96) as f32 / 96.;
        !contains(
            point.x as f32,
            point.y as f32,
            (rect.right - rect.left) as f32,
            (rect.bottom - rect.top) as f32,
            radius,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::contains;

    #[test]
    fn transparent_corners_do_not_capture_hits_at_any_dpi() {
        for scale in [1., 1.25, 1.5, 2.] {
            for (width, height) in [(356., 476.), (200., 56.)] {
                let hit = |x, y| {
                    contains(
                        x * scale,
                        y * scale,
                        width * scale,
                        height * scale,
                        28. * scale,
                    )
                };
                for (x, y) in [
                    (1., 1.),
                    (width - 1., 1.),
                    (1., height - 1.),
                    (width - 1., height - 1.),
                ] {
                    assert!(!hit(x, y));
                }
                assert!(hit(28., 0.));
                assert!(hit(0., 28.));
                assert!(hit(width / 2., height / 2.));
                assert!(!hit(-1., height / 2.));
            }
        }
    }
}
