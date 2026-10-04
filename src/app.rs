//! The window's thread: one eframe window, "📡 Chat & Alerts" (window),
//! made the first time the menu item is clicked. Closing it only hides it
//! (eframe can't start again in the same process); the menu shows it again.

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use parking_lot::Mutex;

/// The reader's icon (the Theater Reader app's), for the window
const ICON_PNG: &[u8] = include_bytes!("../assets/icon.png");

static THREAD_RUNNING: AtomicBool = AtomicBool::new(false);
static VISIBLE: AtomicBool = AtomicBool::new(false);
/// Show it on the next frame (the menu item was clicked)
static SHOW_REQUEST: AtomicBool = AtomicBool::new(false);
/// Its Win32 window (0: none)
static HWND: AtomicIsize = AtomicIsize::new(0);
static CTX: Mutex<Option<egui::Context>> = Mutex::new(None);

/// The window's Win32 handle, 0 when there's none (dialogs open on it).
pub fn hwnd() -> isize {
    HWND.load(Ordering::SeqCst)
}

/// Whether it's shown.
pub fn is_open() -> bool {
    VISIBLE.load(Ordering::SeqCst)
}

/// The menu item: shows the window, making it the first time.
pub fn open() {
    crate::console::start();
    VISIBLE.store(true, Ordering::SeqCst);
    SHOW_REQUEST.store(true, Ordering::SeqCst);
    if !THREAD_RUNNING.swap(true, Ordering::SeqCst) {
        spawn();
        return;
    }
    show_native();
    repaint();
}

/// Draw it again soon (something it shows changed). Any thread.
pub fn repaint() {
    if let Some(ctx) = CTX.lock().as_ref() {
        ctx.request_repaint();
    }
}

/// Windows doesn't paint a hidden window, and eframe only draws when it's
/// painted: shown natively first, its next frame handles SHOW_REQUEST.
fn show_native() {
    #[cfg(target_os = "windows")]
    {
        unsafe extern "system" {
            fn ShowWindowAsync(hwnd: isize, cmd: i32) -> i32;
            fn IsIconic(hwnd: isize) -> i32;
        }
        const SW_SHOW: i32 = 5;
        const SW_RESTORE: i32 = 9;
        let hwnd = hwnd();
        if hwnd != 0 {
            // Async: the window is the window thread's; OBS's UI thread doesn't wait on it
            unsafe { ShowWindowAsync(hwnd, if IsIconic(hwnd) != 0 { SW_RESTORE } else { SW_SHOW }) };
        }
    }
}

fn icon() -> Option<egui::IconData> {
    let rgba = image::load_from_memory(ICON_PNG).ok()?.to_rgba8();
    let (width, height) = rgba.dimensions();
    Some(egui::IconData { rgba: rgba.into_raw(), width, height })
}

fn spawn() {
    let _ = std::thread::Builder::new().name("theater-reader-window".into()).spawn(|| {
        let mut viewport = egui::ViewportBuilder::default()
            .with_title(crate::window::TITLE)
            .with_inner_size([720.0, 640.0])
            .with_min_inner_size([520.0, 360.0]);
        if let Some(icon) = icon() {
            viewport = viewport.with_icon(icon);
        }
        let options = eframe::NativeOptions {
            viewport,
            // Not OBS's main thread
            event_loop_builder: Some(Box::new(|builder| {
                #[cfg(target_os = "linux")]
                {
                    use winit::platform::wayland::EventLoopBuilderExtWayland;
                    use winit::platform::x11::EventLoopBuilderExtX11;
                    if std::env::var("DISPLAY").map(|s| !s.is_empty()).unwrap_or(false) {
                        EventLoopBuilderExtX11::with_x11(builder);
                        EventLoopBuilderExtX11::with_any_thread(builder, true);
                    } else {
                        EventLoopBuilderExtWayland::with_any_thread(builder, true);
                    }
                }
                #[cfg(target_os = "windows")]
                {
                    use winit::platform::windows::EventLoopBuilderExtWindows;
                    EventLoopBuilderExtWindows::with_any_thread(builder, true);
                }
            })),
            ..Default::default()
        };
        let result = eframe::run_native(
            "Theater Reader",
            options,
            Box::new(|cc| {
                *CTX.lock() = Some(cc.egui_ctx.clone());
                #[cfg(target_os = "windows")]
                {
                    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
                    if let Ok(handle) = cc.window_handle() {
                        if let RawWindowHandle::Win32(win32) = handle.as_raw() {
                            HWND.store(win32.hwnd.get(), Ordering::SeqCst);
                        }
                    }
                }
                Ok(Box::new(ReaderApp))
            }),
        );
        if let Err(e) = result {
            crate::log(&format!("[Reader] ❌ The window couldn't run: {:?}", e));
        }
        HWND.store(0, Ordering::SeqCst);
        *CTX.lock() = None;
        VISIBLE.store(false, Ordering::SeqCst);
        THREAD_RUNNING.store(false, Ordering::SeqCst);
    });
}

struct ReaderApp;

impl eframe::App for ReaderApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if SHOW_REQUEST.swap(false, Ordering::SeqCst) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            VISIBLE.store(false, Ordering::SeqCst);
            crate::window::closed();
            return;
        }
        if !is_open() {
            return;
        }
        crate::window::ui(ctx);
        // Statuses change on their own
        ctx.request_repaint_after(std::time::Duration::from_secs(1));
    }
}
