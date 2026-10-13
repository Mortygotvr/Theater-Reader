//! The "📡 Chat & Alerts" window (the 🎭 Theater menu's item). ⚙ Settings:
//! which chats Theater Reader reads, which of their events, shared chat, the
//! relay, importing the Theater Reader app's settings, and a tester.
//! 🖥 Console: what the relay carries (console, console.html's).
//!
//! Drawn by the window's thread (app).

use parking_lot::Mutex;

use crate::reader::{ChatConfig, Link, Platform, ReaderConfig};

pub const TITLE: &str = "📡 Chat & Alerts — Theater Reader";

/// The 🖥 Console tab is shown (else ⚙ Settings)
static CONSOLE_TAB: Mutex<bool> = Mutex::new(false);
/// The settings being changed (None: as saved)
static DRAFT: Mutex<Option<Draft>> = Mutex::new(None);

struct Draft {
    cfg: ReaderConfig,
    port: String,
    import_path: String,
    note: String,
    test_user: String,
    test_amount: String,
    /// What it was last frame, and when it last changed: it saves itself a
    /// moment after the last change (typing a channel name isn't saved
    /// letter by letter)
    last: (ReaderConfig, String),
    changed_at: std::time::Instant,
}

/// Saved changes apply after this long without another
const SAVE_AFTER: std::time::Duration = std::time::Duration::from_millis(1200);

/// The settings as they'd be saved: chats with no name left out, and the
/// port typed when it is one (else `saved_port`)
fn cleaned(cfg: &ReaderConfig, port: &str, saved_port: u16) -> ReaderConfig {
    let mut cfg = cfg.clone();
    cfg.chats.retain(|c| !c.input.trim().is_empty());
    cfg.relay_port = port.trim().parse::<u16>().ok().filter(|p| *p > 0).unwrap_or(saved_port);
    cfg
}

/// Something it shows changed.
pub fn repaint() {
    if crate::app::is_open() {
        crate::app::repaint();
    }
}

/// The window was closed: changes not saved yet are dropped.
pub fn closed() {
    *DRAFT.lock() = None;
}

/// Its frame (the window thread's update).
pub fn ui(ctx: &egui::Context) {
    egui::CentralPanel::default().show(ctx, |ui| {
        let mut console = *CONSOLE_TAB.lock();
        ui.horizontal(|ui| {
            ui.selectable_value(&mut console, false, egui::RichText::new("⚙ Settings").size(15.0));
            ui.selectable_value(&mut console, true, egui::RichText::new("🖥 Console").size(15.0));
        });
        *CONSOLE_TAB.lock() = console;
        ui.separator();
        if console {
            crate::console::ui(ui);
        } else {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, window_ui);
        }
    });
}

fn link_text(link: &Link) -> (String, egui::Color32) {
    match link {
        Link::Connecting => ("connecting…".into(), egui::Color32::from_rgb(234, 179, 8)),
        Link::Connected => ("connected".into(), egui::Color32::from_rgb(34, 197, 94)),
        Link::Failed(why) => (why.clone(), egui::Color32::from_rgb(239, 68, 68)),
    }
}

fn window_ui(ui: &mut egui::Ui) {
    let mut draft_lock = DRAFT.lock();
    let draft = draft_lock.get_or_insert_with(|| {
        let cfg = crate::reader::config();
        let port = cfg.relay_port.to_string();
        Draft {
            cfg: cfg.clone(),
            port: port.clone(),
            import_path: crate::reader::reader_config_candidates().first().map(|p| p.display().to_string()).unwrap_or_default(),
            note: String::new(),
            test_user: "TestUser".into(),
            test_amount: "500".into(),
            last: (cfg, port),
            changed_at: std::time::Instant::now(),
        }
    });
    let saved = crate::reader::config();
    let port_saved = saved.relay_port.to_string();
    // (A port that isn't one is left as it was)
    let to_save = cleaned(&draft.cfg, &draft.port, saved.relay_port);
    let dirty = to_save != saved;

    ui.heading("📡 Chat & Alerts");
    // Where it stands, as saved
    let green = egui::Color32::from_rgb(34, 197, 94);
    let yellow = egui::Color32::from_rgb(245, 158, 11);
    if saved.enabled && crate::reader::running() {
        let n = saved.chats.iter().filter(|c| c.enabled && !c.input.trim().is_empty()).count();
        ui.colored_label(green, format!("● Connected to {} chat(s)", n));
    } else if !saved.chats.is_empty() {
        ui.colored_label(yellow, "⚠ Not connected to these chats: Theater Reader is turned off. Choose \"Theater Reader connects to the chats below\".");
    }
    ui.label("Chat and alerts come from:");
    ui.radio_value(&mut draft.cfg.enabled, true, egui::RichText::new("Theater Reader connects to the chats below").strong());
    ui.radio_value(&mut draft.cfg.enabled, false, "Another reader (the Theater Reader app): this one stays off");
    ui.weak(if draft.cfg.enabled {
        "They go to Theater Scene's triggers and sources and to other apps (the relay, port below), and to the Control Panel in OBS. Close the Theater Reader app."
    } else {
        "It reads nothing and leaves the port below free: Theater Scene and other apps listen to the reader there."
    });
    if saved.enabled {
        match crate::reader::relay::problem() {
            Some(why) => {
                ui.colored_label(egui::Color32::from_rgb(239, 68, 68), format!("⚠ Relay: {}", why));
            }
            None if crate::reader::relay::hosting() => match crate::reader::relay::host() {
                // Another Theater plugin started first (its settings for the port)
                Some(host) => {
                    ui.weak(format!("Relay: hosted by {} · Theater Reader's messages go through it", host));
                }
                None => {
                    ui.weak(format!("Relay: ws://127.0.0.1:{} · {} app(s) connected", port_saved, crate::reader::relay::clients()));
                }
            },
            None => {}
        }
    }
    // Changes save themselves (see the end)
    if dirty {
        ui.colored_label(yellow, "● Saving…");
    } else if !draft.note.is_empty() {
        ui.weak(&draft.note);
    } else {
        ui.weak("Changes apply by themselves.");
    }
    ui.separator();

    // The chats
    ui.strong("Chats");
    let mut remove = None;
    for (i, chat) in draft.cfg.chats.iter_mut().enumerate() {
        chat_ui(ui, i, chat, saved.enabled, &mut remove);
    }
    if let Some(i) = remove {
        draft.cfg.chats.remove(i);
    }
    ui.horizontal(|ui| {
        for p in Platform::ALL {
            if ui.button(format!("➕ {}", p.name())).clicked() {
                // A first chat: reading them is what it's for
                if draft.cfg.chats.is_empty() {
                    draft.cfg.enabled = true;
                }
                draft.cfg.chats.push(ChatConfig::new(p));
            }
        }
    });
    ui.add_space(6.0);

    ui.strong("Shared chat (Twitch)");
    ui.checkbox(&mut draft.cfg.shared_chat.overlay, "Show the other channels' chat in the chat sources");
    ui.checkbox(&mut draft.cfg.shared_chat.alerts, "Their alerts count too (redeems, subs… as channel ST)");
    ui.add_space(6.0);

    ui.strong("Relay (for other apps)");
    ui.horizontal(|ui| {
        ui.label("Port");
        ui.add(egui::TextEdit::singleline(&mut draft.port).desired_width(60.0));
        ui.checkbox(&mut draft.cfg.lan, "Other computers may connect");
    });
    if draft.port.trim() != port_saved {
        ui.weak("Theater Scene listens on the port of its own settings (relay_port in theater_settings.json): change it there too.");
    }
    ui.add_space(6.0);

    ui.separator();

    egui::CollapsingHeader::new("📥 Import from Theater Reader").show(ui, |ui| {
        ui.label("Theater Reader's config.json (its chats, their events and shared chat; not its TTS or moderation):");
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut draft.import_path).desired_width((ui.available_width() - 80.0).max(120.0)));
            if ui.button("Import").clicked() {
                match std::fs::read_to_string(draft.import_path.trim()).map_err(|e| e.to_string()).and_then(|t| crate::reader::import_reader_config(&t)) {
                    Ok(cfg) => {
                        let n = cfg.chats.len();
                        draft.cfg = ReaderConfig { enabled: true, ..cfg };
                        draft.note = format!("Imported {} chat(s).", n);
                    }
                    Err(e) => draft.note = format!("Couldn't import: {}", e),
                }
            }
        });
        for found in crate::reader::reader_config_candidates() {
            if ui.small_button(found.display().to_string()).clicked() {
                draft.import_path = found.display().to_string();
            }
        }
    });

    egui::CollapsingHeader::new("🧪 Test an alert").show(ui, |ui| {
        ui.weak("Sends it to everything listening, like Theater Reader's tester (channel \"testserver\").");
        let pattern = theater_shared::builder::pattern_ui(ui, "reader_tester");
        ui.horizontal(|ui| {
            ui.label("From");
            ui.add(egui::TextEdit::singleline(&mut draft.test_user).desired_width(100.0));
            ui.label("Amount");
            ui.add(egui::TextEdit::singleline(&mut draft.test_amount).desired_width(60.0));
            let platform_chosen = !pattern.starts_with('*');
            let running = crate::reader::running();
            let why = if running { "Choose a platform and an event" } else { "Theater Reader is off: test with the reader in use (the Theater Reader app's tester)" };
            if ui
                .add_enabled(running && !pattern.is_empty() && platform_chosen, egui::Button::new("▶ Send"))
                .on_disabled_hover_text(why)
                .clicked()
            {
                crate::reader::test_alert(&pattern, &draft.test_user, &draft.test_amount);
                draft.note = format!("Sent a test: {}", pattern);
            }
        });
    });

    // Saves itself a moment after the last change
    let now = (draft.cfg.clone(), draft.port.trim().to_string());
    if now != draft.last {
        draft.last = now;
        draft.changed_at = std::time::Instant::now();
    }
    if dirty {
        if draft.changed_at.elapsed() >= SAVE_AFTER {
            draft.cfg.relay_port = to_save.relay_port;
            crate::reader::set_config(to_save);
            draft.note = "Saved.".into();
        } else {
            ui.ctx().request_repaint_after(SAVE_AFTER);
        }
    }
}

fn chat_ui(ui: &mut egui::Ui, i: usize, chat: &mut ChatConfig, running: bool, remove: &mut Option<usize>) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.checkbox(&mut chat.enabled, "");
            ui.strong(chat.platform.name());
            ui.add(egui::TextEdit::singleline(&mut chat.input).hint_text(chat.platform.input_hint()).desired_width(220.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("🗑").on_hover_text("Remove this chat").clicked() {
                    *remove = Some(i);
                }
            });
        });
        let status = if running && chat.enabled { crate::reader::status(&chat.key()) } else { None };
        if status.is_none() {
            let why = if !chat.enabled {
                "○ turned off"
            } else if !running {
                "○ not connected: Theater Reader is turned off (the choice at the top)"
            } else {
                "○ not connected yet: save to start it"
            };
            ui.weak(why);
        }
        {
            if let Some(status) = status {
                ui.horizontal_wrapped(|ui| {
                    for (name, link) in &status.links {
                        let (text, color) = link_text(link);
                        ui.colored_label(color, format!("● {}: {}", name, text));
                    }
                    if status.events > 0 {
                        let ago = status.last_event.map(|t| t.elapsed().as_secs()).unwrap_or(0);
                        ui.weak(format!("· {} event(s), last {} s ago", status.events, ago));
                    }
                });
            }
        }
        egui::CollapsingHeader::new("Events").id_salt(("reader_chat_events", i)).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                for event in chat.platform.events() {
                    let mut on = chat.event_on(event);
                    if ui.checkbox(&mut on, *event).changed() {
                        if on {
                            chat.events.remove(*event);
                        } else {
                            chat.events.insert(event.to_string(), false);
                        }
                    }
                }
            });
        });
    });
}
