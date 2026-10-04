//! The Chat & Alerts window's 🖥 Console: every message the relay carries
//! (this reader's, or the Theater Reader app's while it's off), as
//! console.html showed them: chat, alerts, the trigger feed, system
//! messages, their JSON, saved to a file on demand.
//!
//! It listens from the first time the window opens (feed), on a thread of
//! its own so nothing is missed while the window isn't drawn.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use serde_json::Value;

/// Lines kept, the oldest dropped first
const KEEP: usize = 2000;
/// Lines drawn (the newest)
const SHOWN: usize = 400;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Chat,
    Alert,
    /// The trigger feed ("moderation": what the Control Panel matches)
    Trigger,
    System,
    Unknown,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Chat => "CHAT",
            Kind::Alert => "ALERT",
            Kind::Trigger => "TRIGGER",
            Kind::System => "SYS",
            Kind::Unknown => "UNK",
        }
    }

    fn color(self) -> egui::Color32 {
        match self {
            Kind::Chat => egui::Color32::from_rgb(0x00, 0x4d, 0x40),
            Kind::Alert => egui::Color32::from_rgb(0xe6, 0x51, 0x00),
            Kind::Trigger => egui::Color32::from_rgb(0x4a, 0x14, 0x8c),
            Kind::System | Kind::Unknown => egui::Color32::from_rgb(0x42, 0x42, 0x42),
        }
    }
}

struct Entry {
    /// Local time, as shown
    time: String,
    /// UTC, for the saved log
    stamp: String,
    kind: Kind,
    text: String,
    /// Shown in green after the text
    trigger: Option<String>,
    json: Option<Value>,
    /// Its JSON, laid out (the first time it's shown)
    pretty: std::sync::OnceLock<String>,
}

struct Filters {
    chat: bool,
    alerts: bool,
    system: bool,
    raw: bool,
}

static LOG: Mutex<VecDeque<Entry>> = Mutex::new(VecDeque::new());
static FILTERS: Mutex<Filters> = Mutex::new(Filters { chat: false, alerts: true, system: false, raw: false });
static STARTED: AtomicBool = AtomicBool::new(false);
static NOTE: Mutex<String> = Mutex::new(String::new());

/// Now, local time "HH:MM:SS.mmm" (UTC where there's no local clock to ask)
pub fn local_time_ms() -> String {
    #[cfg(target_os = "windows")]
    {
        #[repr(C)]
        #[derive(Default)]
        struct SystemTime {
            year: u16,
            month: u16,
            day_of_week: u16,
            day: u16,
            hour: u16,
            minute: u16,
            second: u16,
            milliseconds: u16,
        }
        unsafe extern "system" {
            fn GetLocalTime(time: *mut SystemTime);
        }
        let mut t = SystemTime::default();
        unsafe { GetLocalTime(&mut t) };
        format!("{:02}:{:02}:{:02}.{:03}", t.hour, t.minute, t.second, t.milliseconds)
    }
    #[cfg(not(target_os = "windows"))]
    {
        crate::reader::utc_now()[11..23].to_string()
    }
}

/// Now, local time "HH:MM:SS"
fn local_time() -> String {
    local_time_ms()[..8].to_string()
}

fn push(kind: Kind, text: String, trigger: Option<String>, json: Option<Value>) {
    let entry = Entry {
        time: local_time(),
        stamp: crate::reader::utc_now(),
        kind,
        text,
        trigger,
        json,
        pretty: std::sync::OnceLock::new(),
    };
    let mut log = LOG.lock();
    log.push_back(entry);
    while log.len() > KEEP {
        log.pop_front();
    }
    drop(log);
    crate::window::repaint();
}

/// A system line (connections coming and going), once the console listens.
pub fn system(text: &str) {
    if STARTED.load(Ordering::SeqCst) {
        push(Kind::System, text.to_string(), None, None);
    }
}

/// Listens from now on (the window opened).
pub fn start() {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let feed = crate::feed::subscribe();
    push(Kind::System, format!("Console started: {}", source_text()), None, None);
    let _ = std::thread::Builder::new().name("theater-reader-console".into()).spawn(move || {
        while let Ok(message) = feed.recv() {
            add(&message);
        }
    });
}

/// Who the messages come from
fn source_text() -> String {
    let port = crate::reader::config().relay_port;
    if crate::reader::running() {
        if crate::reader::relay::hosting() {
            format!("Theater Reader reads the chats (relay on port {})", port)
        } else {
            "Theater Reader reads the chats (its relay isn't up)".to_string()
        }
    } else if crate::feed::connected() {
        format!("off: listening to the reader on port {}", port)
    } else {
        format!("off: waiting for a reader on port {}…", port)
    }
}

fn connected() -> bool {
    if crate::reader::running() { crate::reader::relay::hosting() } else { crate::feed::connected() }
}

/// A relay message, as console.html wrote it
fn add(message: &Arc<str>) {
    let Ok(data) = serde_json::from_str::<Value>(message) else {
        push(Kind::Unknown, format!("Not JSON: {}", message), None, None);
        return;
    };
    let s = |v: &Value, key: &str| v.get(key).and_then(|x| x.as_str()).unwrap_or("").to_string();
    match data.get("type").and_then(|t| t.as_str()).unwrap_or("") {
        "chat" | "whisper" => {
            let text = format!("[{}] {}: {}", s(&data, "source"), s(&data, "username"), s(&data, "message"));
            push(Kind::Chat, text, None, Some(data));
        }
        "alert" => {
            let text = format!("[{}] Event detected ->", s(&data, "source"));
            let trigger = s(&data, "trigger");
            push(Kind::Alert, text, Some(trigger), Some(data));
        }
        "moderation" => {
            let payload = data.get("payload").cloned().unwrap_or(Value::Null);
            let trigger = Some(s(&payload, "trigger")).filter(|t| !t.is_empty()).unwrap_or_else(|| "Unknown".into());
            push(Kind::Trigger, "Trigger feed ->".into(), Some(trigger), Some(payload));
        }
        "reader_state" => {
            let on = data.get("enabled").and_then(|e| e.as_bool()).unwrap_or(false);
            push(Kind::System, format!("Theater Reader {}", if on { "started" } else { "stopped" }), None, None);
        }
        "config_state" | "tts_info" | "status" | "sub_cache" | "vision_status" | "sys_diag" => {
            push(Kind::System, format!("Received internal system sync: {}", s(&data, "type")), None, Some(data));
        }
        other => {
            let other = if other.is_empty() { "(none)".to_string() } else { other.to_string() };
            push(Kind::Unknown, format!("Unknown message type: {}", other), None, Some(data));
        }
    }
}

fn shown(kind: Kind, f: &Filters) -> bool {
    match kind {
        Kind::Chat => f.chat,
        Kind::Alert | Kind::Trigger => f.alerts,
        Kind::System | Kind::Unknown => f.system,
    }
}

/// The log as console.html saved it: [{timestamp, payload}]
fn save() {
    let entries: Vec<Value> = LOG
        .lock()
        .iter()
        .map(|e| serde_json::json!({ "timestamp": e.stamp, "payload": e.json.clone().unwrap_or_else(|| Value::from(e.text.clone())) }))
        .collect();
    if entries.is_empty() {
        *NOTE.lock() = "No logs to save!".into();
        return;
    }
    let name = format!("theater_reader_log_{}.json", crate::reader::utc_now().replace([':', '.'], "-"));
    let _ = std::thread::Builder::new().name("theater-reader-console-save".into()).spawn(move || {
        let Some(path) = crate::dialog::save_file_dialog("Save the console log", "JSON (*.json) | *.json", &name) else { return };
        let written = serde_json::to_string_pretty(&entries).map_err(|e| e.to_string()).and_then(|s| std::fs::write(&path, s).map_err(|e| e.to_string()));
        *NOTE.lock() = match written {
            Ok(()) => format!("Saved {} message(s) to {}", entries.len(), path.display()),
            Err(e) => format!("Couldn't save: {}", e),
        };
        crate::window::repaint();
    });
}

/// The console tab
pub fn ui(ui: &mut egui::Ui) {
    start();
    let mut f = FILTERS.lock();
    ui.horizontal_wrapped(|ui| {
        let color = if connected() { egui::Color32::from_rgb(0x4c, 0xaf, 0x50) } else { egui::Color32::from_rgb(0xd3, 0x2f, 0x2f) };
        let (dot, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
        ui.painter().circle_filled(dot.center(), 6.0, color);
        ui.strong("Event Console");
        ui.weak(source_text());
    });
    ui.horizontal_wrapped(|ui| {
        ui.checkbox(&mut f.chat, "Show chat");
        ui.checkbox(&mut f.alerts, "Show alerts and triggers");
        ui.checkbox(&mut f.system, "Show system events");
        ui.checkbox(&mut f.raw, "Show raw JSON");
        if ui.button("💾 Save log to file").clicked() {
            save();
        }
        if ui.button("🗑 Clear console").clicked() {
            LOG.lock().clear();
            NOTE.lock().clear();
        }
    });
    let note = NOTE.lock().clone();
    if !note.is_empty() {
        ui.weak(note);
    }
    ui.separator();

    let log = LOG.lock();
    let visible: Vec<&Entry> = log.iter().filter(|e| shown(e.kind, &f)).collect();
    let skipped = visible.len().saturating_sub(SHOWN);
    egui::ScrollArea::vertical().auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
        if skipped > 0 {
            ui.weak(format!("({} older line(s) not shown; Save keeps them all)", skipped));
        }
        for e in &visible[skipped..] {
            entry_ui(ui, e, f.raw);
        }
        if visible.is_empty() {
            ui.weak("Nothing yet with these filters.");
        }
    });
}

fn entry_ui(ui: &mut egui::Ui, e: &Entry, raw: bool) {
    let mono = egui::TextStyle::Monospace.resolve(ui.style());
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        ui.label(egui::RichText::new(format!("[{}]", e.time)).font(mono.clone()).color(egui::Color32::from_gray(0x88)));
        ui.label(
            egui::RichText::new(format!(" {:^7} ", e.kind.label()))
                .font(mono.clone())
                .strong()
                .color(egui::Color32::WHITE)
                .background_color(e.kind.color()),
        );
        ui.label(egui::RichText::new(&e.text).font(mono.clone()));
        if let Some(trigger) = &e.trigger {
            ui.label(egui::RichText::new(trigger).font(mono.clone()).strong().color(egui::Color32::from_rgb(0x53, 0xfc, 0x18)));
        }
    });
    if raw {
        if let Some(json) = e.json.as_ref().filter(|j| j.as_object().is_some_and(|o| !o.is_empty())) {
            let pretty = e.pretty.get_or_init(|| serde_json::to_string_pretty(json).unwrap_or_default());
            egui::Frame::new()
                .fill(egui::Color32::from_rgb(0x18, 0x18, 0x18))
                .stroke(egui::Stroke::new(1.0, egui::Color32::from_gray(0x33)))
                .corner_radius(4.0)
                .inner_margin(8.0)
                .show(ui, |ui| {
                    ui.label(egui::RichText::new(pretty).font(mono).size(11.0).color(egui::Color32::from_gray(0x88)));
                });
        }
    }
    ui.add_space(2.0);
    ui.separator();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The newest line saying `marker` (other tests publish too)
    fn line(marker: &str) -> (Kind, String, Option<String>) {
        let log = LOG.lock();
        let e = log
            .iter()
            .rev()
            .find(|e| e.text.contains(marker) || e.trigger.as_deref().is_some_and(|t| t.contains(marker)))
            .unwrap_or_else(|| panic!("no line with {marker}"));
        (e.kind, e.text.clone(), e.trigger.clone())
    }

    #[test]
    fn relay_messages_become_console_lines() {
        add(&Arc::from(r#"{"type":"chat","source":"Twitch","username":"Mo","message":"console hi"}"#));
        let (kind, text, _) = line("console hi");
        assert!(kind == Kind::Chat && text == "[Twitch] Mo: console hi", "{text}");

        add(&Arc::from(r#"{"type":"alert","source":"TwitchParser","event":"Twitch cheer","trigger":"Twitch consolecheck cheer 500 bits","data":{}}"#));
        let (kind, text, _) = line("consolecheck cheer");
        assert!(kind == Kind::Alert && text == "[TwitchParser] Event detected ->");

        add(&Arc::from(r#"{"type":"moderation","payload":{"trigger":"Twitch consolecheck sub","username":"a"}}"#));
        let (kind, text, _) = line("consolecheck sub");
        assert!(kind == Kind::Trigger && text == "Trigger feed ->");

        add(&Arc::from(r#"{"type":"console_check_type"}"#));
        assert!(line("console_check_type").0 == Kind::Unknown);
        add(&Arc::from("not json (console check)"));
        assert!(line("console check)").0 == Kind::Unknown);
    }

    #[test]
    fn the_tab_draws_with_raw_json() {
        add(&Arc::from(r#"{"type":"alert","source":"KickParser","event":"Kick follow","trigger":"Kick m follow","data":{"username":"x"}}"#));
        FILTERS.lock().raw = true;
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| super::ui(ui));
        });
        FILTERS.lock().raw = false;
        let log = LOG.lock();
        let drawn = log.iter().rev().find(|e| e.trigger.as_deref() == Some("Kick m follow")).unwrap();
        assert!(drawn.pretty.get().is_some_and(|p| p.contains("\"username\": \"x\"")), "its JSON was laid out");
    }
}
