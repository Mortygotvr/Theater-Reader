//! The relay, on port 41837 where Theater Reader's was, for other apps
//! (Theater Scene, overlays, bots, a Control Panel outside OBS): Theater
//! Shared's, which Theater Reader and the Theater API plugins share. Whichever
//! starts first hosts it and the others send through it, so each works with
//! or without the others. Theater Reader's alert tester (test_sammi_trigger)
//! works as it did, whoever hosts.

use serde_json::Value;
use theater_shared::relay::{self, Role};

/// The relay is up (served here, or by another Theater plugin, or joined).
pub fn hosting() -> bool {
    relay::status().up
}

/// Why it isn't (the port is taken…)
pub fn problem() -> Option<String> {
    relay::status().problem
}

/// How many apps are connected (when served here).
pub fn clients() -> usize {
    relay::status().apps
}

/// Who serves it, when not Theater Reader ("Theater API · Twitch")
pub fn host() -> Option<String> {
    match relay::status().role {
        Role::Host | Role::None => None,
        role => Some(relay::role_name(&role)),
    }
}

/// Takes Theater Reader's role in the relay (the first time it reads chats:
/// turned off, it leaves the port to another reader).
pub fn start(port: u16, lan: bool) {
    relay::start("Theater Reader", port, lan, crate::runtime().handle(), crate::log, Some(from_app));
}

/// To every app connected, and the Control Panel.
pub fn broadcast(text: &str) {
    relay::send(text);
}

/// What an app sent
fn from_app(text: &str) {
    let Ok(v) = serde_json::from_str::<Value>(text) else { return };
    let s = |key: &str| v.get(key).and_then(|x| x.as_str()).map(str::to_string);
    match v.get("type").and_then(|t| t.as_str()).unwrap_or("") {
        // Theater Reader's alert tester
        "test_sammi_trigger" => {
            let amount = v.get("var").map(|x| x.as_str().map(str::to_string).unwrap_or_else(|| x.to_string())).unwrap_or_else(|| "500".into());
            super::test_alert(&s("trigger").unwrap_or_else(|| "Test Trigger".into()), &s("user").unwrap_or_default(), &amount);
        }
        // A trigger card's Test: its first pattern, wildcards filled
        "execute_trigger" => {
            let events = v.pointer("/payload/events").and_then(|e| e.as_str()).unwrap_or("");
            let Some(pattern) = events.split(',').map(str::trim).find(|p| !p.is_empty()) else { return };
            let fill = v.pointer("/data/_matches/0").map(|m| m.as_str().map(str::to_string).unwrap_or_else(|| m.to_string()));
            super::test_alert(pattern, "", &fill.unwrap_or_else(|| "TestVal".into()));
        }
        // The rest reached the other apps already
        _ => {}
    }
}
