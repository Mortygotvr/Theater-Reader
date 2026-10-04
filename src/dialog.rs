//! The Save As dialog (the console's 💾 Save log to file). Blocking: call it
//! from a thread of its own.

/// Asks where to save, suggesting `file_name` (`file_filter` as zenity says
/// it: "Name | *.a"); asks before replacing a file.
pub fn save_file_dialog(title: &str, file_filter: &str, file_name: &str) -> Option<std::path::PathBuf> {
    #[cfg(target_os = "windows")]
    return win_save_dialog(title, file_filter, file_name);
    #[cfg(not(target_os = "windows"))]
    {
        let mut cmd = std::process::Command::new("zenity");
        cmd.args(["--file-selection", "--save", "--confirm-overwrite"])
            .arg(format!("--title={}", title))
            .arg(format!("--filename={}", file_name))
            .arg(format!("--file-filter={}", file_filter));
        let mut kcmd = std::process::Command::new("kdialog");
        kcmd.arg("--getsavefilename").arg(file_name).arg(format!("--title={}", title));
        for mut c in [cmd, kcmd] {
            if let Ok(output) = c.output() {
                let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if output.status.success() && !s.is_empty() {
                    return Some(std::path::PathBuf::from(s));
                }
            }
        }
        None
    }
}

/// Windows' common Save As dialog, on top of the reader's window
#[cfg(target_os = "windows")]
fn win_save_dialog(title: &str, file_filter: &str, file_name: &str) -> Option<std::path::PathBuf> {
    #[repr(C)]
    struct OpenFileNameW {
        struct_size: u32,
        hwnd_owner: isize,
        instance: isize,
        filter: *const u16,
        custom_filter: *mut u16,
        max_cust_filter: u32,
        filter_index: u32,
        file: *mut u16,
        max_file: u32,
        file_title: *mut u16,
        max_file_title: u32,
        initial_dir: *const u16,
        title: *const u16,
        flags: u32,
        file_offset: u16,
        file_extension: u16,
        def_ext: *const u16,
        cust_data: isize,
        hook: *const std::ffi::c_void,
        template_name: *const u16,
        reserved_ptr: *mut std::ffi::c_void,
        reserved: u32,
        flags_ex: u32,
    }
    const _: () = assert!(std::mem::size_of::<OpenFileNameW>() == 152, "OPENFILENAMEW size mismatch");
    #[link(name = "comdlg32")]
    unsafe extern "system" {
        fn GetSaveFileNameW(ofn: *mut OpenFileNameW) -> i32;
    }
    #[link(name = "ole32")]
    unsafe extern "system" {
        fn CoInitializeEx(reserved: *mut std::ffi::c_void, co_init: u32) -> i32;
        fn CoUninitialize();
    }
    const COINIT_APARTMENTTHREADED: u32 = 0x2;
    const COINIT_DISABLE_OLE1DDE: u32 = 0x4;
    const OFN_OVERWRITEPROMPT: u32 = 0x2;
    const OFN_HIDEREADONLY: u32 = 0x4;
    // Keeps the dialog from changing OBS's process-wide working directory
    const OFN_NOCHANGEDIR: u32 = 0x8;
    const OFN_PATHMUSTEXIST: u32 = 0x800;
    const OFN_EXPLORER: u32 = 0x80000;

    let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };

    let mut filter = String::new();
    if !file_filter.is_empty() {
        let (name, patterns) = file_filter.split_once('|').unwrap_or((file_filter, file_filter));
        filter.push_str(name.trim());
        filter.push('\0');
        filter.push_str(&patterns.split_whitespace().collect::<Vec<_>>().join(";"));
        filter.push('\0');
    }
    filter.push_str("All Files\0*.*\0");
    let filter_w = wide(&filter); // trailing NUL from `wide` terminates the list
    let title_w = wide(title);
    // A name typed without an extension gets the filter's ("*.json" → "json")
    let default_ext_w = file_filter
        .split_once('|')
        .and_then(|(_, patterns)| patterns.split_whitespace().next())
        .and_then(|p| p.strip_prefix("*."))
        .map(wide);
    let mut file_buf = vec![0u16; 32768];
    for (slot, c) in file_buf.iter_mut().zip(file_name.encode_utf16().take(32767)) {
        *slot = c;
    }

    let mut ofn: OpenFileNameW = unsafe { std::mem::zeroed() };
    ofn.struct_size = std::mem::size_of::<OpenFileNameW>() as u32;
    ofn.hwnd_owner = crate::app::hwnd();
    ofn.filter = filter_w.as_ptr();
    ofn.filter_index = 1;
    ofn.file = file_buf.as_mut_ptr();
    ofn.max_file = file_buf.len() as u32;
    ofn.title = title_w.as_ptr();
    ofn.def_ext = default_ext_w.as_ref().map_or(std::ptr::null(), |e| e.as_ptr());
    ofn.flags = OFN_EXPLORER | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR | OFN_HIDEREADONLY | OFN_OVERWRITEPROMPT;

    let hr = unsafe { CoInitializeEx(std::ptr::null_mut(), COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
    let picked = unsafe { GetSaveFileNameW(&mut ofn) } != 0;
    if hr >= 0 {
        unsafe { CoUninitialize() };
    }
    if !picked {
        return None;
    }
    let len = file_buf.iter().position(|&c| c == 0).unwrap_or(file_buf.len());
    (len > 0).then(|| std::path::PathBuf::from(String::from_utf16_lossy(&file_buf[..len])))
}
