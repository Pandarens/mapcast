//! Captures the top-left corner of the WoW window, where the addon draws the strip.
//!
//! The corner is copied from the screen while the game is in view. When another window covers it
//! the game window is asked to draw itself instead (PrintWindow), which is slower but sees it.

use crate::protocol::Frame;

pub const DEFAULT_REGION: (usize, usize) = (1024, 48);

/// Where frames come from: the game's window, or the simulator.
pub trait Source: Send {
    fn grab(&mut self) -> Option<Frame>;
    fn resize(&mut self, width: usize, height: usize);
    fn window_found(&self) -> bool;
}

#[cfg(windows)]
mod win {
    use super::*;
    use std::time::{Duration, Instant};
    use windows_sys::core::BOOL;
    use windows_sys::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
    use windows_sys::Win32::Graphics::Gdi::{
        BitBlt, ClientToScreen, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC,
        GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HDC, SRCCOPY,
    };
    use windows_sys::Win32::UI::HiDpi::{SetProcessDpiAwareness, PROCESS_PER_MONITOR_DPI_AWARE};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetClassNameW, GetClientRect, GetWindowTextW, IsIconic, IsWindow, IsWindowVisible,
    };

    // user32's PrintWindow: draws a window into a DC even while other windows cover it.
    #[link(name = "user32")]
    extern "system" {
        fn PrintWindow(hwnd: HWND, hdc: HDC, flags: u32) -> BOOL;
    }
    const PW_CLIENTONLY: u32 = 1;
    const PW_RENDERFULLCONTENT: u32 = 2; // what the compositor shows: needed for a game's 3D window

    const WOW_TITLE: &str = "World of Warcraft";
    const WOW_CLASSES: [&str; 3] = ["waApplication Window", "GxWindowClass", "GxWindowClassD3d"];

    fn wide_to_string(buf: &[u16], n: i32) -> String {
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }

    unsafe extern "system" fn enum_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let found = &mut *(lparam as *mut Vec<HWND>);
        if IsWindowVisible(hwnd) == 0 || IsIconic(hwnd) != 0 {
            return 1;
        }
        let mut title = [0u16; 256];
        let n = GetWindowTextW(hwnd, title.as_mut_ptr(), 256);
        let mut class = [0u16; 256];
        let m = GetClassNameW(hwnd, class.as_mut_ptr(), 256);
        let (title, class) = (wide_to_string(&title, n), wide_to_string(&class, m));
        if title == WOW_TITLE || WOW_CLASSES.contains(&class.as_str()) {
            found.push(hwnd);
        }
        1
    }

    fn find_wow_window() -> Option<HWND> {
        let mut found: Vec<HWND> = Vec::new();
        unsafe {
            EnumWindows(Some(enum_callback), &mut found as *mut Vec<HWND> as LPARAM);
        }
        found.into_iter().next()
    }

    /// Screen position and size of a window's client area.
    fn client_rect(hwnd: HWND) -> Option<(i32, i32, i32, i32)> {
        unsafe {
            let mut rect: RECT = std::mem::zeroed();
            if GetClientRect(hwnd, &mut rect) == 0 {
                return None;
            }
            let mut origin = POINT { x: 0, y: 0 };
            ClientToScreen(hwnd, &mut origin);
            Some((origin.x, origin.y, rect.right, rect.bottom))
        }
    }

    /// A rectangle of a DC as RGB. `screen` is the display DC the copy's bitmap is made for.
    fn copy_rect(screen: HDC, source: HDC, x: i32, y: i32, w: i32, h: i32) -> Option<Frame> {
        unsafe {
            let mem = CreateCompatibleDC(screen);
            let bitmap = CreateCompatibleBitmap(screen, w, h);
            let old = SelectObject(mem, bitmap);
            let ok = BitBlt(mem, 0, 0, w, h, source, x, y, SRCCOPY) != 0;
            let mut info: BITMAPINFO = std::mem::zeroed();
            info.bmiHeader = BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // top-down rows
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..std::mem::zeroed()
            };
            let mut bgra = vec![0u8; (w * h * 4) as usize];
            let rows = if ok {
                GetDIBits(mem, bitmap, 0, h as u32, bgra.as_mut_ptr().cast(), &mut info, DIB_RGB_COLORS)
            } else {
                0
            };
            SelectObject(mem, old);
            DeleteObject(bitmap);
            DeleteDC(mem);
            if rows == 0 {
                return None;
            }
            let mut frame = Frame::new(w as usize, h as usize);
            for (px, out) in bgra.chunks_exact(4).zip(frame.rgb.chunks_exact_mut(3)) {
                out[0] = px[2];
                out[1] = px[1];
                out[2] = px[0];
            }
            Some(frame)
        }
    }

    /// A copy of a screen rectangle as RGB (the same pixels a screenshot has).
    fn grab_screen(x: i32, y: i32, w: i32, h: i32) -> Option<Frame> {
        unsafe {
            let screen = GetDC(std::ptr::null_mut());
            if screen.is_null() {
                return None;
            }
            let frame = copy_rect(screen, screen, x, y, w, h);
            ReleaseDC(std::ptr::null_mut(), screen);
            frame
        }
    }

    /// The top-left corner of a window's client area, drawn by the window itself: works while
    /// other windows cover it (not while it is minimized). The whole client area has to be
    /// drawn to get the corner, so this costs tens of milliseconds.
    fn grab_window(hwnd: HWND, client_w: i32, client_h: i32, w: i32, h: i32) -> Option<Frame> {
        unsafe {
            let screen = GetDC(std::ptr::null_mut());
            if screen.is_null() {
                return None;
            }
            let full_dc = CreateCompatibleDC(screen);
            let full = CreateCompatibleBitmap(screen, client_w, client_h);
            let old = SelectObject(full_dc, full);
            let drawn = PrintWindow(hwnd, full_dc, PW_CLIENTONLY | PW_RENDERFULLCONTENT) != 0;
            let frame = if drawn { copy_rect(screen, full_dc, 0, 0, w, h) } else { None };
            SelectObject(full_dc, old);
            DeleteObject(full);
            DeleteDC(full_dc);
            ReleaseDC(std::ptr::null_mut(), screen);
            frame
        }
    }

    /// Whether a capture starts with the strip's magenta marker.
    fn has_strip(frame: &Frame) -> bool {
        if frame.width == 0 || frame.height == 0 {
            return false;
        }
        let p = frame.pixel(0, 0);
        p[0] > 200 && p[1] < 60 && p[2] > 200
    }

    pub struct ScreenSource {
        hwnd: Option<HWND>,
        next_search: Instant,
        region: (usize, usize),
        found: bool,
    }

    // HWND is a raw pointer; the handle is only used from the capture thread.
    unsafe impl Send for ScreenSource {}

    impl ScreenSource {
        pub fn new() -> ScreenSource {
            unsafe {
                SetProcessDpiAwareness(PROCESS_PER_MONITOR_DPI_AWARE);
            }
            ScreenSource { hwnd: None, next_search: Instant::now(), region: DEFAULT_REGION, found: false }
        }
    }

    impl Source for ScreenSource {
        fn resize(&mut self, width: usize, height: usize) {
            self.region = (width.max(16), height.max(8));
        }

        fn window_found(&self) -> bool {
            self.found
        }

        fn grab(&mut self) -> Option<Frame> {
            if let Some(h) = self.hwnd {
                if unsafe { IsWindow(h) } == 0 {
                    self.hwnd = None;
                }
            }
            if self.hwnd.is_none() && Instant::now() >= self.next_search {
                self.next_search = Instant::now() + Duration::from_secs(1);
                self.hwnd = find_wow_window();
            }
            let rect = self.hwnd.and_then(client_rect);
            self.found = rect.is_some();
            let (x, y, w, h) = rect?;
            let width = (self.region.0 as i32).min(w);
            let height = (self.region.1 as i32).min(h);
            if width <= 0 || height <= 0 {
                return None;
            }
            let on_screen = grab_screen(x, y, width, height);
            if on_screen.as_ref().is_some_and(has_strip) {
                return on_screen;
            }
            // not in view: another window is over the corner
            let hwnd = self.hwnd?;
            grab_window(hwnd, w, h, width, height).filter(has_strip).or(on_screen)
        }
    }
}

#[cfg(windows)]
pub use win::ScreenSource;

#[cfg(not(windows))]
pub struct ScreenSource;

#[cfg(not(windows))]
impl ScreenSource {
    pub fn new() -> ScreenSource {
        ScreenSource
    }
}

#[cfg(not(windows))]
impl Source for ScreenSource {
    fn grab(&mut self) -> Option<Frame> {
        None
    }
    fn resize(&mut self, _: usize, _: usize) {}
    fn window_found(&self) -> bool {
        false
    }
}
