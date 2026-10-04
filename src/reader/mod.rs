//! The chat reader: Theater Reader's chat and alerts (Twitch, Kick and
//! YouTube) inside OBS, with no Python app.
//!
//! It says what Theater Reader said, word for word (the same chat, alert and
//! trigger messages, see normalize), and hands each message to:
//! - other apps, Theater Scene among them: the relay, a websocket on port
//!   41837 like Theater Reader's (relay), which they connect to;
//! - the Control Panel inside OBS: the OBS signal `theater_reader_event`,
//!   carrying the message's JSON (obs);
//! - this plugin's own window: its console and tester (feed).
//!
//! Its settings (which chats, which of their events, the relay) are in
//! reader.json in the plugin's config folder, set in the "📡 Chat & Alerts"
//! window (window). Turned off, it reads nothing and leaves the relay's port
//! to another reader (the Theater Reader app).

pub mod kick;
#[cfg(test)]
mod live;
pub mod normalize;
pub mod relay;
pub mod twitch;
pub mod youtube;

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::log as trust_log;
use normalize::RawEvent;
pub use theater_shared::Platform;
use theater_shared::seen;

/// The relay's port when none is set (Theater Reader's).
pub const DEFAULT_PORT: u16 = 41837;

fn yes() -> bool {
    true
}

fn default_port() -> u16 {
    DEFAULT_PORT
}

/// A chat to listen to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChatConfig {
    pub platform: Platform,
    /// The channel (Twitch, Kick), or a handle or video link (YouTube)
    pub input: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Events turned off or on (absent: on)
    #[serde(default)]
    pub events: BTreeMap<String, bool>,
}

impl ChatConfig {
    pub fn new(platform: Platform) -> Self {
        ChatConfig { platform, input: String::new(), enabled: true, events: BTreeMap::new() }
    }

    pub fn event_on(&self, event: &str) -> bool {
        self.events.get(event).copied().unwrap_or(true)
    }

    /// Who it is in the status list
    pub fn key(&self) -> String {
        format!("{}:{}", self.platform.name(), self.input.trim())
    }
}

/// What shared chat (another channel's chat shown in yours, Twitch) may do.
/// Its chat always reaches the chat sources and the Control Panel.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SharedChat {
    /// Its messages in the chat sources
    #[serde(default)]
    pub overlay: bool,
    /// Its alerts (redeems, subs…) at all
    #[serde(default)]
    pub alerts: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReaderConfig {
    /// It reads the chats and runs the relay (else another reader does)
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub chats: Vec<ChatConfig>,
    #[serde(default)]
    pub shared_chat: SharedChat,
    /// Other computers may connect to the relay (else this one only)
    #[serde(default)]
    pub lan: bool,
    /// The relay's port (Theater Scene and other apps connect to it there)
    #[serde(default = "default_port")]
    pub relay_port: u16,
}

impl Default for ReaderConfig {
    fn default() -> Self {
        ReaderConfig { enabled: false, chats: Vec::new(), shared_chat: SharedChat::default(), lan: false, relay_port: DEFAULT_PORT }
    }
}

fn config_file() -> std::path::PathBuf {
    crate::config_dir().join("reader.json")
}

static CONFIG: Mutex<Option<ReaderConfig>> = Mutex::new(None);

/// The settings (read once from reader.json; the first time, from where
/// Theater Scene kept them when the reader was part of it).
pub fn config() -> ReaderConfig {
    CONFIG
        .lock()
        .get_or_insert_with(|| {
            std::fs::read_to_string(config_file())
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .or_else(|| if cfg!(test) { None } else { legacy_config() })
                .unwrap_or_default()
        })
        .clone()
}

/// Folders Theater Scene may be in (the reader's settings used to be there).
fn theater_scene_dirs() -> Vec<std::path::PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(dir) = std::env::var("THEATER_SCENE_DIR") {
        dirs.push(std::path::PathBuf::from(dir));
    }
    if let Ok(home) = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")) {
        let home = std::path::PathBuf::from(home);
        dirs.push(home.join("repo").join("theater scene(rust)"));
        dirs.push(home.join("repo").join("theater-scene"));
    }
    dirs
}

/// Theater Scene's theater_reader.json and relay_port, copied here once.
fn legacy_config() -> Option<ReaderConfig> {
    for dir in theater_scene_dirs() {
        let Ok(text) = std::fs::read_to_string(dir.join("theater_reader.json")) else { continue };
        let Ok(mut cfg) = serde_json::from_str::<ReaderConfig>(&text) else { continue };
        cfg.relay_port = std::fs::read_to_string(dir.join("theater_settings.json"))
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .and_then(|v| v.get("relay_port").and_then(|p| p.as_u64()))
            .and_then(|p| u16::try_from(p).ok())
            .filter(|p| *p > 0)
            .unwrap_or(DEFAULT_PORT);
        // The trigger lists' channels and titles too
        let seen_file = crate::config_dir().join("seen.json");
        if !seen_file.exists() {
            let _ = std::fs::copy(dir.join("theater_reader_seen.json"), &seen_file);
        }
        save_config(&cfg);
        trust_log(&format!("[Reader] 📥 Took the settings Theater Scene had in {}", dir.display()));
        return Some(cfg);
    }
    None
}

fn save_config(cfg: &ReaderConfig) {
    let _ = std::fs::create_dir_all(crate::config_dir());
    match serde_json::to_string_pretty(cfg) {
        Ok(s) => {
            if let Err(e) = std::fs::write(config_file(), s) {
                trust_log(&format!("[Reader] ⚠️ Could not save {}: {}", config_file().display(), e));
            }
        }
        Err(e) => trust_log(&format!("[Reader] ⚠️ Could not save the settings: {}", e)),
    }
}

/// Saves the settings and starts over with them.
pub fn set_config(cfg: ReaderConfig) {
    save_config(&cfg);
    *CONFIG.lock() = Some(cfg);
    restart();
}

/// Theater Reader's config.json: its chats and shared chat options.
pub fn import_reader_config(text: &str) -> Result<ReaderConfig, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {}", e))?;
    let chats = v.get("chats").and_then(|c| c.as_object()).ok_or("no \"chats\" in it")?;
    let mut cfg = config();
    cfg.chats.clear();
    for chat in chats.values() {
        let platform = match chat.get("parser").and_then(|p| p.as_str()).unwrap_or("") {
            "twitch_parse" => Platform::Twitch,
            "kick_parse" => Platform::Kick,
            "youtube_parse" => Platform::YouTube,
            _ => continue,
        };
        let input = chat.get("input").and_then(|i| i.as_str()).unwrap_or("").trim().to_string();
        if input.is_empty() {
            continue;
        }
        let filters = chat.get("event_filters").or_else(|| chat.get("filters")).and_then(|f| f.as_object());
        let events = filters
            .map(|f| {
                f.iter()
                    .filter(|(k, _)| platform.events().contains(&k.as_str()))
                    .filter_map(|(k, on)| on.as_bool().filter(|on| !on).map(|on| (k.clone(), on)))
                    .collect()
            })
            .unwrap_or_default();
        cfg.chats.push(ChatConfig { platform, input, enabled: true, events });
    }
    if let Some(st) = v.get("st_filters") {
        cfg.shared_chat.overlay = st.get("overlay").and_then(|b| b.as_bool()).unwrap_or(false);
        cfg.shared_chat.alerts = st.get("alerts").and_then(|b| b.as_bool()).unwrap_or(false);
    }
    Ok(cfg)
}

/// Where Theater Reader's config.json may be.
pub fn reader_config_candidates() -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(home) = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")) {
        let home = std::path::PathBuf::from(home);
        out.push(home.join(".config").join("TheaterReader").join("config.json"));
        out.push(home.join("repo").join("Theater-reader").join("config.json"));
    }
    if let Ok(appdata) = std::env::var("LOCALAPPDATA") {
        out.push(std::path::PathBuf::from(appdata).join("Programs").join("Theater Reader").join("config.json"));
    }
    out.into_iter().filter(|p| p.is_file()).collect()
}

// ---------------------------------------------------------------------------
// How each chat is doing

#[derive(Clone, Debug, PartialEq)]
pub enum Link {
    Connecting,
    Connected,
    /// Why not (it tries again)
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct ChatStatus {
    /// Each connection of the chat (Twitch has two: chat, and points)
    pub links: BTreeMap<&'static str, Link>,
    pub last_event: Option<std::time::Instant>,
    pub events: u64,
}

static STATUS: Mutex<Option<HashMap<String, ChatStatus>>> = Mutex::new(None);

pub(crate) fn set_link(chat: &str, link: &'static str, state: Link) {
    let mut status = STATUS.lock();
    let entry = status.get_or_insert_with(HashMap::new).entry(chat.to_string()).or_insert_with(|| ChatStatus {
        links: BTreeMap::new(),
        last_event: None,
        events: 0,
    });
    if entry.links.get(link) == Some(&state) {
        return;
    }
    let said = match &state {
        Link::Connected => Some(format!("🟢 {} ({}) connected", chat, link)),
        Link::Failed(why) => Some(format!("🔴 {} ({}): {}", chat, link, why)),
        Link::Connecting => None,
    };
    entry.links.insert(link, state);
    drop(status);
    if let Some(said) = said {
        trust_log(&format!("[Reader] {}", said));
        crate::console::system(&said);
    }
    repaint_window();
}

fn count_event(chat: &str) {
    if let Some(entry) = STATUS.lock().get_or_insert_with(HashMap::new).get_mut(chat) {
        entry.last_event = Some(std::time::Instant::now());
        entry.events += 1;
    }
}

pub fn status(chat: &str) -> Option<ChatStatus> {
    STATUS.lock().as_ref().and_then(|s| s.get(chat).cloned())
}

fn repaint_window() {
    crate::window::repaint();
}

// ---------------------------------------------------------------------------
// Running

/// The running chats' tasks, and the pipeline's
static TASKS: Mutex<Vec<tokio::task::JoinHandle<()>>> = Mutex::new(Vec::new());
static EVENTS: Mutex<Option<UnboundedSender<RawEvent>>> = Mutex::new(None);
/// Which start this is: a stopped chat's late events are dropped
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Whether it reads the chats now.
pub fn running() -> bool {
    EVENTS.lock().is_some()
}

/// Starts reading (when OBS loads the plugin).
pub fn start() {
    // One TLS for Twitch, Kick and YouTube (ring); a second one would make
    // rustls ask which
    let _ = rustls::crypto::ring::default_provider().install_default();
    restart();
}

/// Stops what runs and starts what the settings say.
pub fn restart() {
    stop();
    let cfg = config();
    if !cfg.enabled {
        trust_log(&format!("[Reader] 💤 Turned off: another reader may use port {}", cfg.relay_port));
        return;
    }
    let runtime = crate::runtime();
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *EVENTS.lock() = Some(tx.clone());
    *STATUS.lock() = Some(HashMap::new());

    let mut tasks = TASKS.lock();
    tasks.push(runtime.spawn(pipeline(rx, cfg.clone(), generation)));
    tasks.push(runtime.spawn(relay::serve(cfg.relay_port, cfg.lan)));
    tasks.push(runtime.spawn(twitch::load_badges()));
    for chat in cfg.chats.iter().filter(|c| c.enabled && !c.input.trim().is_empty()) {
        let sink = Sink { tx: tx.clone(), chat: chat.key(), generation };
        let input = chat.input.trim().to_string();
        set_link(&chat.key(), "chat", Link::Connecting);
        tasks.push(match chat.platform {
            Platform::Twitch => runtime.spawn(twitch::run(input, sink)),
            Platform::Kick => runtime.spawn(kick::run(input, sink)),
            Platform::YouTube => runtime.spawn(youtube::run(input, sink)),
        });
    }
    let n = cfg.chats.iter().filter(|c| c.enabled).count();
    trust_log(&format!("[Reader] 📡 Reading {} chat(s) in OBS; the relay is on port {}", n, cfg.relay_port));
    crate::obs::emit_reader_event(&serde_json::json!({ "type": "reader_state", "enabled": true }).to_string());
}

/// Stops every chat and the relay.
pub fn stop() {
    let was_running = EVENTS.lock().take().is_some();
    for task in TASKS.lock().drain(..) {
        task.abort();
    }
    relay::stopped();
    if was_running {
        crate::obs::emit_reader_event(&serde_json::json!({ "type": "reader_state", "enabled": false }).to_string());
    }
}

/// Where a chat's connections send what they read.
#[derive(Clone)]
pub struct Sink {
    tx: UnboundedSender<RawEvent>,
    /// The chat (ChatConfig::key)
    pub chat: String,
    generation: u64,
}

impl Sink {
    pub fn send(&self, platform: Platform, channel: &str, event: &str, trigger: String, data: Map<String, Value>) {
        if GENERATION.load(Ordering::SeqCst) != self.generation {
            return;
        }
        count_event(&self.chat);
        let _ = self.tx.send(RawEvent {
            platform,
            channel: channel.to_string(),
            event: event.to_string(),
            trigger,
            data,
        });
    }

    pub fn link(&self, link: &'static str, state: Link) {
        if GENERATION.load(Ordering::SeqCst) == self.generation {
            set_link(&self.chat, link, state);
        }
    }
}

/// One after the other, like Theater Reader's queue: the event's messages,
/// then out.
async fn pipeline(mut rx: UnboundedReceiver<RawEvent>, cfg: ReaderConfig, generation: u64) {
    while let Some(event) = rx.recv().await {
        if GENERATION.load(Ordering::SeqCst) != generation {
            return;
        }
        let chat = cfg.chats.iter().find(|c| c.platform == event.platform && same_channel(&c.input, &event.channel));
        let options = normalize::Options {
            event_on: chat.map(|c| c.event_on(&event.event)).unwrap_or(true),
            shared: cfg.shared_chat.clone(),
        };
        let avatar = if normalize::is_chat(&event.event) { chat_avatar(&event).await } else { None };
        for message in normalize::messages(&event, &options, &new_msg_id(), avatar.as_deref()) {
            seen::note(&message);
            dispatch(&message);
        }
    }
}

/// Theater Reader matched the chat of an event loosely (the input, or either
/// in the other)
fn same_channel(input: &str, channel: &str) -> bool {
    let input = input.trim();
    !input.is_empty() && (input.eq_ignore_ascii_case(channel) || channel.contains(input) || input.contains(channel))
}

/// The picture a chat message shows: the channel's (Theater Reader's way)
async fn chat_avatar(event: &RawEvent) -> Option<String> {
    match event.platform {
        Platform::Twitch => {
            let text = |key: &str| event.data.get(key).and_then(|r| r.as_str()).filter(|r| !r.is_empty());
            let room = text("source_room_id").or_else(|| text("room_id"))?;
            twitch::room_avatar(room).await
        }
        Platform::Kick => kick::channel_avatar(&event.channel),
        Platform::YouTube => youtube::channel_avatar(&event.channel)
            .or_else(|| event.data.get("image").and_then(|i| i.as_str()).map(str::to_string)),
    }
}

/// Hands a message to everything listening: other apps (the relay, Theater
/// Scene's triggers and sources among them), the Control Panel (OBS signal)
/// and this plugin's window.
pub fn dispatch(message: &Value) {
    let text = message.to_string();
    crate::feed::publish(&text);
    relay::broadcast(&text);
    crate::obs::emit_reader_event(&text);
}

/// Theater Reader's alert tester: `trigger` (e.g. "Twitch * redeem Hydrate"
/// or "Twitch cheer"), as if `user` had done it, with `amount`. Its channel
/// is "testserver".
pub fn test_alert(trigger: &str, user: &str, amount: &str) {
    for message in normalize::test_messages(trigger, user, amount, unix_secs()) {
        seen::note(&message);
        dispatch(&message);
    }
}

// ---------------------------------------------------------------------------
// Small things

pub(crate) fn unix_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Now, as "2026-09-30T12:34:56.789Z"
pub(crate) fn utc_now() -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let secs = now.as_secs() as i64;
    let (days, rem) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    // Civil date from days since 1970 (Howard Hinnant's)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z", y, m, d, rem / 3600, rem % 3600 / 60, rem % 60, now.subsec_millis())
}

/// A random number (std's hasher keys are random)
pub(crate) fn random_u64() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(COUNT.fetch_add(1, Ordering::Relaxed));
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
    h.finish()
}

/// A message id (a random UUID, like Theater Reader's)
pub(crate) fn new_msg_id() -> String {
    let (a, b) = (random_u64(), random_u64());
    let bytes: Vec<u8> = a.to_le_bytes().into_iter().chain(b.to_le_bytes()).collect();
    let hex: String = bytes
        .iter()
        .enumerate()
        .map(|(i, byte)| match i {
            6 => (byte & 0x0f) | 0x40,
            8 => (byte & 0x3f) | 0x80,
            _ => *byte,
        })
        .map(|b| format!("{:02x}", b))
        .collect();
    format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

/// Web requests, with a browser's name
pub(crate) fn http() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36")
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap_or_default()
    })
}

/// Waits before trying again: 1 s, then twice as long each time, a minute at most
pub(crate) struct Backoff(u64);

impl Backoff {
    pub fn new() -> Self {
        Backoff(1)
    }
    pub fn reset(&mut self) {
        self.0 = 1;
    }
    pub async fn wait(&mut self) {
        tokio::time::sleep(Duration::from_secs(self.0)).await;
        self.0 = (self.0 * 2).min(60);
    }
    pub fn secs(&self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theater_readers_config_comes_in() {
        let text = r#"{
            "chats": {
                "chat_1": {"parser": "twitch_parse", "input": "mortygotvr", "moderate": false,
                    "event_filters": {"Twitch chat": true, "Twitch other": false, "Twitch roomstate": false}},
                "chat_2": {"parser": "kick_parse", "input": "mortygotvr", "filters": {"Kick follow": false}},
                "chat_3": {"parser": "youtube_parse", "input": ""}
            },
            "st_filters": {"tts": true, "sammi": true, "overlay": true, "alerts": false},
            "gemini_api_key": "secret"
        }"#;
        let cfg = import_reader_config(text).unwrap();
        assert_eq!(cfg.chats.len(), 2, "the empty one is left out");
        let twitch = cfg.chats.iter().find(|c| c.platform == Platform::Twitch).unwrap();
        assert_eq!(twitch.input, "mortygotvr");
        assert!(twitch.event_on("Twitch chat"));
        assert!(!twitch.event_on("Twitch other"));
        assert!(!twitch.events.contains_key("Twitch roomstate"), "only events it has");
        let kick = cfg.chats.iter().find(|c| c.platform == Platform::Kick).unwrap();
        assert!(!kick.event_on("Kick follow"));
        assert!(cfg.shared_chat.overlay && !cfg.shared_chat.alerts);
    }

    #[test]
    fn utc_now_looks_right() {
        let now = utc_now();
        assert_eq!(now.len(), 24);
        assert!(now.starts_with("20") && now.ends_with('Z'), "{now}");
    }

    #[test]
    fn message_ids_look_like_uuids() {
        let id = new_msg_id();
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4");
        assert_ne!(id, new_msg_id());
    }

    #[test]
    fn a_chat_is_found_loosely_like_theater_reader_did() {
        assert!(same_channel("Elcaeses", "elcaeses"));
        assert!(same_channel("https://www.youtube.com/watch?v=abc", "https://www.youtube.com/watch?v=abc"));
        assert!(!same_channel("", "mortygotvr"));
        assert!(!same_channel("havoc", "mortygotvr"));
    }
}
