//! Theater Reader: the chat reader as an OBS plugin of its own
//! (theater-reader.dll), installed or not.
//!
//! It reads Twitch, Kick and YouTube chats and alerts (reader) and hands them
//! on like the Theater Reader app did: through the relay, a websocket on port
//! 41837 that Theater Scene and other apps connect to, and through the
//! `theater_reader_event` OBS signal, for the Control Panel inside OBS (obs).
//! Theater Scene works the same without it, with the Theater Reader app or
//! anything else serving the relay.
//!
//! Its window, "📡 Chat & Alerts" (window, console), opens from the shared
//! 🎭 Theater menu, where its item shows the reader's icon.

pub mod app;
pub mod console;
pub mod dialog;
pub mod feed;
pub mod obs;
pub mod reader;
pub mod window;

use std::path::PathBuf;
use std::sync::OnceLock;

/// The plugin's id: its folder's name, and its items' name in the 🎭 Theater
/// menu (theater-control-panel/docs/shared-menu.md). Never change it.
pub const PLUGIN_ID: &str = "theater-reader";

/// The reader's tasks run here.
pub fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("theater-reader")
            .enable_all()
            .build()
            .expect("Failed to start Theater Reader's runtime")
    })
}

/// Its settings' folder: OBS's plugin_config\theater-reader.
pub fn config_dir() -> PathBuf {
    if cfg!(test) {
        return std::env::temp_dir().join("theater_reader_test_config");
    }
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = obs::module_config_dir().unwrap_or_else(|| obs_user_dir().join("plugin_config").join(PLUGIN_ID));
        let _ = std::fs::create_dir_all(&dir);
        dir
    })
    .clone()
}

/// OBS's own folder of the user's (its logs, plugin settings)
fn obs_user_dir() -> PathBuf {
    if cfg!(target_os = "windows") {
        std::env::var("APPDATA").map(PathBuf::from).unwrap_or_default().join("obs-studio")
    } else {
        std::env::var("HOME").map(PathBuf::from).unwrap_or_default().join(".config").join("obs-studio")
    }
}

/// A line in OBS's logs folder, theater_reader.log, timed like OBS's log.
pub fn log(msg: &str) {
    // Not from the tests: their lines would read as OBS's
    if cfg!(test) {
        return;
    }
    use std::io::Write;
    let dir = obs_user_dir().join("logs");
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("theater_reader.log")) {
        let _ = writeln!(f, "{}: {}", console::local_time_ms(), msg);
    }
}

/// Panics in its own threads (the window's, the reader's) otherwise vanish
/// on Windows, where OBS has no console: logged, then the default hook runs.
fn install_panic_logger() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let thread = std::thread::current();
            log(&format!("[Reader PANIC] thread '{}': {}", thread.name().unwrap_or("<unnamed>"), info));
            default_hook(info);
        }));
    });
}

// ---------------------------------------------------------------------------
// The OBS module

#[unsafe(no_mangle)]
pub extern "C" fn obs_module_ver() -> u32 {
    (30u32 << 24) | (0u32 << 16) | 0u32
}

#[unsafe(no_mangle)]
pub extern "C" fn obs_module_set_pointer(module: *mut std::ffi::c_void) {
    obs::set_module(module);
}

#[unsafe(no_mangle)]
pub extern "C" fn obs_module_name() -> *const std::ffi::c_char {
    c"Theater Reader".as_ptr()
}

#[unsafe(no_mangle)]
pub extern "C" fn obs_module_description() -> *const std::ffi::c_char {
    c"Twitch, Kick and YouTube chat and alerts for Theater Scene and the Control Panel".as_ptr()
}

/// The trigger lists' chats (the tester's): the ones it reads
fn reader_chats(platform: Option<theater_shared::Platform>) -> Vec<String> {
    reader::config()
        .chats
        .iter()
        .filter(|c| platform.is_none_or(|p| p == c.platform))
        .map(|c| c.input.trim().to_string())
        .collect()
}

#[unsafe(no_mangle)]
pub extern "C" fn obs_module_load() -> bool {
    install_panic_logger();
    theater_shared::seen::set_file(config_dir().join("seen.json"));
    theater_shared::builder::set_host(theater_shared::builder::Host { chats: reader_chats, subscribe: feed::subscribe });
    // Before the reader starts: its first messages signal
    obs::register();
    reader::start();
    obs::setup_menu();
    log("[Reader] 🚀 Theater Reader loaded");
    true
}

#[unsafe(no_mangle)]
pub extern "C" fn obs_module_unload() {
    reader::stop();
    log("[Reader] 🛑 Theater Reader unloaded");
}

/// Keeps the DLL loaded until OBS exits: its menu item and the reader's
/// threads call into it, and may outlive obs_module_unload.
#[cfg(target_os = "windows")]
fn pin_module() {
    unsafe extern "system" {
        fn GetModuleHandleExW(flags: u32, module_name: *const u16, module: *mut *mut std::ffi::c_void) -> i32;
    }
    const GET_MODULE_HANDLE_EX_FLAG_PIN: u32 = 0x1;
    const GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS: u32 = 0x4;
    let mut module: *mut std::ffi::c_void = std::ptr::null_mut();
    unsafe {
        GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_PIN, pin_module as *const u16, &mut module);
    }
}

#[cfg(target_os = "windows")]
#[unsafe(no_mangle)]
pub extern "system" fn DllMain(_module: *mut std::ffi::c_void, reason: u32, _reserved: *mut std::ffi::c_void) -> i32 {
    const DLL_PROCESS_ATTACH: u32 = 1;
    if reason == DLL_PROCESS_ATTACH {
        pin_module();
    }
    1
}
