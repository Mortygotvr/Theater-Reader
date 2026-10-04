//! Theater Reader in OBS: libobs's functions it uses, the Control Panel's
//! signal and procedures, and its item in the 🎭 Theater menu.
//!
//! On OBS's global handlers, for the Control Panel inside OBS (Theater Control Panel):
//! - signal `theater_reader_event(string json)`: each message the reader
//!   sends (chat, alert, moderation), and `{"type":"reader_state","enabled"}`
//!   when it starts or stops;
//! - `theater_reader_state(out bool enabled, out string json)`: whether it
//!   reads the chats, and `{"enabled","relay":{"port","up","apps"},
//!   "chats":[{"platform","channel"}]}` (Theater Scene's trigger lists take
//!   the chats from it);
//! - `theater_reader_seen(out string json)`: channels and redeem titles seen,
//!   `{"Twitch":{"channel":["title",…]},…}`;
//! - `theater_reader_test(in string trigger, in string user, in string amount, out bool ok)`:
//!   a test alert, like Theater Reader's tester, to everything listening.

use std::ffi::{CStr, CString, c_char, c_void};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicPtr, Ordering};

const READER_EVENT_DECL: &CStr = c"void theater_reader_event(string json)";
const READER_EVENT: &CStr = c"theater_reader_event";
const READER_STATE_DECL: &CStr = c"void theater_reader_state(out bool enabled, out string json)";
const READER_SEEN_DECL: &CStr = c"void theater_reader_seen(out string json)";
const READER_TEST_DECL: &CStr = c"void theater_reader_test(in string trigger, in string user, in string amount, out bool ok)";

/// libobs's calldata_t.
#[repr(C)]
struct Calldata {
    stack: *mut u8,
    size: usize,
    capacity: usize,
    fixed: bool,
}

impl Calldata {
    fn new() -> Self {
        Calldata { stack: std::ptr::null_mut(), size: 0, capacity: 0, fixed: false }
    }
}

type ProcFn = unsafe extern "C" fn(data: *mut c_void, cd: *mut Calldata);
type MenuCb = unsafe extern "C" fn(data: *mut c_void);

struct Obs {
    proc_handler_add: unsafe extern "C" fn(*mut c_void, *const c_char, ProcFn, *mut c_void),
    signal_handler_add: unsafe extern "C" fn(*mut c_void, *const c_char) -> bool,
    signal_handler_signal: unsafe extern "C" fn(*mut c_void, *const c_char, *mut Calldata),
    calldata_get_string: unsafe extern "C" fn(*const Calldata, *const c_char, *mut *const c_char) -> bool,
    calldata_set_data: unsafe extern "C" fn(*mut Calldata, *const c_char, *const c_void, usize),
    bfree: unsafe extern "C" fn(*mut c_void),
    proc_handler: *mut c_void,
    signal_handler: *mut c_void,
}

// The handlers are OBS's global ones, which live as long as OBS.
unsafe impl Send for Obs {}
unsafe impl Sync for Obs {}

static OBS: OnceLock<Obs> = OnceLock::new();
/// OBS's handle of this module (obs_module_set_pointer)
static MODULE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

pub fn set_module(module: *mut c_void) {
    MODULE.store(module, Ordering::SeqCst);
}

// ---------------------------------------------------------------------------
// Finding libobs's functions

#[cfg(target_os = "windows")]
unsafe extern "system" {
    fn GetModuleHandleW(name: *const u16) -> *mut c_void;
    fn LoadLibraryW(name: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
    fn GetModuleHandleExW(flags: u32, name: *const u16, module: *mut *mut c_void) -> i32;
    fn GetModuleFileNameW(module: *mut c_void, file: *mut u16, size: u32) -> u32;
}

#[cfg(not(target_os = "windows"))]
unsafe extern "C" {
    fn dlopen(file: *const c_char, mode: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
}

#[cfg(target_os = "windows")]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// `name` from the loaded library `lib` (Windows: "obs.dll"; elsewhere a
/// soname, or "" for what the process has loaded)
fn symbol(lib: &str, name: &CStr) -> *mut c_void {
    #[cfg(target_os = "windows")]
    {
        let module = unsafe { GetModuleHandleW(wide(lib).as_ptr()) };
        if module.is_null() {
            return std::ptr::null_mut();
        }
        unsafe { GetProcAddress(module, name.as_ptr()) }
    }
    #[cfg(not(target_os = "windows"))]
    {
        const RTLD_NOW: i32 = 2;
        let handle = if lib.is_empty() {
            std::ptr::null_mut()
        } else {
            let Ok(lib) = CString::new(lib) else { return std::ptr::null_mut() };
            unsafe { dlopen(lib.as_ptr(), RTLD_NOW) }
        };
        unsafe { dlsym(handle, name.as_ptr()) }
    }
}

const LIBOBS: &str = if cfg!(target_os = "windows") { "obs.dll" } else { "" };
const FRONTEND: &str = if cfg!(target_os = "windows") { "obs-frontend-api.dll" } else { "libobs-frontend-api.so.0" };

/// A function of `lib`, as `T` (its function pointer type)
fn function<T>(lib: &str, name: &CStr) -> Option<T> {
    let p = symbol(lib, name);
    // SAFETY: T is the function pointer type of the export `name`.
    (!p.is_null()).then(|| unsafe { std::mem::transmute_copy(&p) })
}

fn load() -> Option<Obs> {
    let get_proc_handler: unsafe extern "C" fn() -> *mut c_void = function(LIBOBS, c"obs_get_proc_handler")?;
    let get_signal_handler: unsafe extern "C" fn() -> *mut c_void = function(LIBOBS, c"obs_get_signal_handler")?;
    let proc_handler = unsafe { get_proc_handler() };
    let signal_handler = unsafe { get_signal_handler() };
    if proc_handler.is_null() || signal_handler.is_null() {
        return None;
    }
    Some(Obs {
        proc_handler_add: function(LIBOBS, c"proc_handler_add")?,
        signal_handler_add: function(LIBOBS, c"signal_handler_add")?,
        signal_handler_signal: function(LIBOBS, c"signal_handler_signal")?,
        calldata_get_string: function(LIBOBS, c"calldata_get_string")?,
        calldata_set_data: function(LIBOBS, c"calldata_set_data")?,
        bfree: function(LIBOBS, c"bfree")?,
        proc_handler,
        signal_handler,
    })
}

/// A string libobs allocated (bmalloc), as a path, freed
fn take_obs_string(p: *mut c_char) -> Option<PathBuf> {
    if p.is_null() {
        return None;
    }
    let path = PathBuf::from(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned());
    if let Some(bfree) = function::<unsafe extern "C" fn(*mut c_void)>(LIBOBS, c"bfree") {
        unsafe { bfree(p as *mut c_void) };
    }
    Some(path)
}

/// OBS's config folder for this plugin (obs_module_config_path's)
pub fn module_config_dir() -> Option<PathBuf> {
    let module = MODULE.load(Ordering::SeqCst);
    if module.is_null() {
        return None;
    }
    let get: unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_char = function(LIBOBS, c"obs_module_get_config_path")?;
    take_obs_string(unsafe { get(module, c"".as_ptr()) })
}

/// A file of the plugin's data folder (obs_module_file's)
fn module_file(file: &CStr) -> Option<PathBuf> {
    let module = MODULE.load(Ordering::SeqCst);
    if module.is_null() {
        return None;
    }
    let find: unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_char = function(LIBOBS, c"obs_find_module_file")?;
    take_obs_string(unsafe { find(module, file.as_ptr()) })
}

// ---------------------------------------------------------------------------
// The Control Panel's signal and procedures

/// Registers the signal and the procedures. Call from obs_module_load.
pub fn register() {
    if OBS.get().is_none() {
        if let Some(obs) = load() {
            let _ = OBS.set(obs);
        }
    }
    let Some(obs) = OBS.get() else {
        crate::log("[Reader] ⚠️ OBS's procedure handler isn't there: no Control Panel link");
        return;
    };
    unsafe {
        (obs.signal_handler_add)(obs.signal_handler, READER_EVENT_DECL.as_ptr());
        (obs.proc_handler_add)(obs.proc_handler, READER_STATE_DECL.as_ptr(), reader_state_proc, std::ptr::null_mut());
        (obs.proc_handler_add)(obs.proc_handler, READER_SEEN_DECL.as_ptr(), reader_seen_proc, std::ptr::null_mut());
        (obs.proc_handler_add)(obs.proc_handler, READER_TEST_DECL.as_ptr(), reader_test_proc, std::ptr::null_mut());
    }
    crate::log("[Reader] 🔗 Registered theater_reader_event / theater_reader_state / theater_reader_seen / theater_reader_test");
}

/// A reader message for the Control Panel (theater_reader_event). Any thread.
pub fn emit_reader_event(json: &str) {
    let Some(obs) = OBS.get() else { return };
    let mut cd = Calldata::new();
    set_string(obs, &mut cd, c"json", json);
    unsafe {
        (obs.signal_handler_signal)(obs.signal_handler, READER_EVENT.as_ptr(), &mut cd);
        if !cd.stack.is_null() {
            (obs.bfree)(cd.stack as *mut c_void);
        }
    }
}

/// What theater_reader_state says
pub fn state_json() -> serde_json::Value {
    let cfg = crate::reader::config();
    let chats: Vec<serde_json::Value> = cfg
        .chats
        .iter()
        .filter(|c| c.enabled && !c.input.trim().is_empty())
        .map(|c| serde_json::json!({ "platform": c.platform.name(), "channel": c.input.trim() }))
        .collect();
    serde_json::json!({
        "enabled": crate::reader::running(),
        "relay": {
            "port": cfg.relay_port,
            "up": crate::reader::relay::hosting(),
            "apps": crate::reader::relay::clients(),
        },
        "chats": chats,
    })
}

unsafe extern "C" fn reader_state_proc(_data: *mut c_void, cd: *mut Calldata) {
    let Some(obs) = OBS.get() else { return };
    let json = std::panic::catch_unwind(state_json).unwrap_or_default();
    set_bool(obs, cd, c"enabled", json.get("enabled").and_then(|e| e.as_bool()).unwrap_or(false));
    set_string(obs, cd, c"json", &json.to_string());
}

unsafe extern "C" fn reader_seen_proc(_data: *mut c_void, cd: *mut Calldata) {
    let Some(obs) = OBS.get() else { return };
    let json = std::panic::catch_unwind(|| serde_json::to_string(&theater_shared::seen::all()).unwrap_or_default())
        .unwrap_or_else(|_| "{}".to_string());
    set_string(obs, cd, c"json", &json);
}

unsafe extern "C" fn reader_test_proc(_data: *mut c_void, cd: *mut Calldata) {
    let Some(obs) = OBS.get() else { return };
    let trigger = get_string(obs, cd, c"trigger");
    let user = get_string(obs, cd, c"user");
    let amount = get_string(obs, cd, c"amount");
    let ok = !trigger.trim().is_empty() && std::panic::catch_unwind(|| crate::reader::test_alert(&trigger, &user, &amount)).is_ok();
    set_bool(obs, cd, c"ok", ok);
}

fn get_string(obs: &Obs, cd: *mut Calldata, name: &CStr) -> String {
    let mut s: *const c_char = std::ptr::null();
    if unsafe { (obs.calldata_get_string)(cd, name.as_ptr(), &mut s) } && !s.is_null() {
        unsafe { CStr::from_ptr(s) }.to_string_lossy().into_owned()
    } else {
        String::new()
    }
}

fn set_string(obs: &Obs, cd: *mut Calldata, name: &CStr, value: &str) {
    let value = CString::new(value.replace('\0', "")).unwrap_or_default();
    let bytes = value.as_bytes_with_nul();
    unsafe { (obs.calldata_set_data)(cd, name.as_ptr(), bytes.as_ptr() as *const c_void, bytes.len()) };
}

fn set_bool(obs: &Obs, cd: *mut Calldata, name: &CStr, value: bool) {
    unsafe { (obs.calldata_set_data)(cd, name.as_ptr(), &value as *const bool as *const c_void, size_of::<bool>()) };
}

// ---------------------------------------------------------------------------
// The 🎭 Theater menu

const MENU_TITLE: &CStr = c"\u{1F4E1} Chat & Alerts\u{2026}";
/// Where it goes when the Theater menu can't be reached (no Qt shim)
const TOOLS_TITLE: &CStr = c"\u{1F4E1} Theater Reader: Chat & Alerts";
const OBS_TASK_UI: u32 = 0;

unsafe extern "C" fn open_window_cb(_data: *mut c_void) {
    crate::app::open();
}

/// Adds its item to the 🎭 Theater menu once OBS's main window is up.
pub fn setup_menu() {
    let _ = std::thread::Builder::new().name("theater-reader-menu".into()).spawn(|| {
        if let Some(get_main_window) = function::<unsafe extern "C" fn() -> *mut c_void>(FRONTEND, c"obs_frontend_get_main_window") {
            for _ in 0..150 {
                if !unsafe { get_main_window() }.is_null() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
        type QueueTask = unsafe extern "C" fn(u32, unsafe extern "C" fn(*mut c_void), *mut c_void, bool);
        match function::<QueueTask>(LIBOBS, c"obs_queue_task") {
            Some(queue_task) => unsafe { queue_task(OBS_TASK_UI, add_menu_item_task, std::ptr::null_mut(), false) },
            None => unsafe { add_menu_item_task(std::ptr::null_mut()) },
        }
    });
}

/// On OBS's UI thread: the 🎭 Theater menu, else OBS's Tools menu.
unsafe extern "C" fn add_menu_item_task(_data: *mut c_void) {
    if add_to_theater_menu() {
        crate::log("[Reader] 🎭 Added '📡 Chat & Alerts…' to the Theater menu");
        return;
    }
    type AddToolsItem = unsafe extern "C" fn(*const c_char, MenuCb, *mut c_void);
    match function::<AddToolsItem>(FRONTEND, c"obs_frontend_add_tools_menu_item") {
        Some(add) => {
            unsafe { add(TOOLS_TITLE.as_ptr(), open_window_cb, std::ptr::null_mut()) };
            crate::log("[Reader] 🔧 No Qt shim: added '📡 Theater Reader: Chat & Alerts' to OBS's Tools menu");
        }
        None => crate::log("[Reader] ⚠️ Couldn't add a menu item: no obs-frontend-api"),
    }
}

/// Its group and item in the shared 🎭 Theater menu (theater-control-panel/docs/shared-menu.md),
/// through the Qt shim shipped next to this DLL (trust_qt.dll, Theater Scene's
/// own: it takes any plugin's id and icon).
fn add_to_theater_menu() -> bool {
    let Some(shim) = load_shim() else { return false };
    type StartGroup = unsafe extern "C" fn(*const c_char) -> bool;
    type AddItem = unsafe extern "C" fn(*const c_char, *const c_char, *const c_char, MenuCb, *mut c_void) -> bool;
    let (Some(start_group), Some(add_item)) = (
        shim_function::<StartGroup>(shim, c"trust_ui_menu_start_group"),
        shim_function::<AddItem>(shim, c"trust_ui_menu_add_item"),
    ) else {
        crate::log("[Reader] ⚠️ The Qt shim is too old for the Theater menu (no trust_ui_menu_add_item)");
        return false;
    };
    let id = CString::new(crate::PLUGIN_ID).unwrap_or_default();
    let icon = module_file(c"icon.png").map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    let icon = CString::new(icon).unwrap_or_default();
    unsafe { start_group(id.as_ptr()) && add_item(id.as_ptr(), icon.as_ptr(), MENU_TITLE.as_ptr(), open_window_cb, std::ptr::null_mut()) }
}

/// The folder this DLL is in
#[cfg(target_os = "windows")]
fn own_dir() -> Option<PathBuf> {
    const FROM_ADDRESS: u32 = 0x4;
    const UNCHANGED_REFCOUNT: u32 = 0x2;
    let mut module: *mut c_void = std::ptr::null_mut();
    if unsafe { GetModuleHandleExW(FROM_ADDRESS | UNCHANGED_REFCOUNT, own_dir as *const u16, &mut module) } == 0 {
        return None;
    }
    let mut buf = [0u16; 1024];
    let len = unsafe { GetModuleFileNameW(module, buf.as_mut_ptr(), buf.len() as u32) } as usize;
    (len > 0 && len < buf.len()).then(|| PathBuf::from(String::from_utf16_lossy(&buf[..len])))?.parent().map(PathBuf::from)
}

#[cfg(target_os = "windows")]
fn load_shim() -> Option<*mut c_void> {
    let path = own_dir()?.join("trust_qt.dll");
    if !path.is_file() {
        crate::log(&format!("[Reader] No Qt shim at {}", path.display()));
        return None;
    }
    let handle = unsafe { LoadLibraryW(wide(&path.to_string_lossy()).as_ptr()) };
    (!handle.is_null()).then_some(handle)
}

// Linux: Theater Scene's Linux shim has no shared-menu functions yet
#[cfg(not(target_os = "windows"))]
fn load_shim() -> Option<*mut c_void> {
    None
}

fn shim_function<T>(shim: *mut c_void, name: &CStr) -> Option<T> {
    #[cfg(target_os = "windows")]
    let p = unsafe { GetProcAddress(shim, name.as_ptr()) };
    #[cfg(not(target_os = "windows"))]
    let p = unsafe { dlsym(shim, name.as_ptr()) };
    // SAFETY: T is the function pointer type of the shim's export `name`.
    (!p.is_null()).then(|| unsafe { std::mem::transmute_copy(&p) })
}
