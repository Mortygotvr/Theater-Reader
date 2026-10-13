//! What Theater Reader made of an event (main.py's queue loop): its trigger
//! ("Platform channel event detail"), then its messages: the chat line or
//! the alert, and the trigger feed ("moderation").

use serde_json::{Map, Value, json};

use super::{Platform, SharedChat};

/// An event as a chat connection read it (Theater Reader's queue item:
/// parser, channel, event, trigger, data).
#[derive(Clone, Debug)]
pub struct RawEvent {
    pub platform: Platform,
    /// The chat's input (channel name, or YouTube's link)
    pub channel: String,
    /// e.g. "Twitch redeem (pubsub)", "Kick chat", "paid_message"
    pub event: String,
    /// The parser's own trigger, e.g. "Twitch redeem Hydrate"
    pub trigger: String,
    pub data: Map<String, Value>,
}

pub struct Options {
    /// The chat hasn't turned this event off
    pub event_on: bool,
    pub shared: SharedChat,
}

/// Theater Reader's chat events
const CHAT_EVENTS: [&str; 4] = ["Twitch chat", "Kick chat", "chat_message", "Twitch whisper"];

pub fn is_chat(event: &str) -> bool {
    CHAT_EVENTS.contains(&event)
}

fn text<'a>(data: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    data.get(key).and_then(|v| v.as_str())
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Number(n)) => n.as_f64().is_some_and(|n| n != 0.0),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        _ => false,
    }
}

/// `trigger` without `prefix` (Python's replace, for the one place it is)
fn after(trigger: &str, prefix: &str) -> String {
    trigger.strip_prefix(prefix).unwrap_or(&trigger.replace(prefix, "")).trim().to_string()
}

/// The event's word(s) in triggers, and what follows them
pub fn event_words(platform: Platform, event: &str, trigger: &str, message: &str) -> (String, String) {
    match platform {
        Platform::Twitch => {
            let base = event.strip_prefix("Twitch ").unwrap_or(event).replace(" (irc)", "").replace(" (pubsub)", "");
            let extra = match base.as_str() {
                "cheer" => after(trigger, "Twitch cheer "),
                "redeem" => after(trigger, "Twitch redeem "),
                "hype train" => after(trigger, "Twitch hype train "),
                _ => String::new(),
            };
            (base, extra)
        }
        Platform::Kick => {
            let base = event.strip_prefix("Kick ").unwrap_or(event).to_string();
            match base.as_str() {
                "redeem" => (base, after(trigger, "Kick redeem ")),
                "raid start" => ("raid".to_string(), String::new()),
                _ => (base, String::new()),
            }
        }
        Platform::YouTube => match event {
            "paid_message" => ("super chat".to_string(), after(trigger, "Super Chat: ")),
            "sticker_message" => ("super sticker".to_string(), after(trigger, "Super Sticker: ")),
            "member_message" => ("member".to_string(), String::new()),
            "chat_message" => ("chat".to_string(), message.to_string()),
            _ => (event.to_string(), String::new()),
        },
    }
}

/// The event's trigger: "Platform channel event detail"
pub fn trigger_of(event: &RawEvent) -> String {
    let message = text(&event.data, "message").unwrap_or("");
    let channel = if event.channel.is_empty() || event.channel == "Unknown" { "System" } else { &event.channel };
    let channel = if truthy(event.data.get("is_shared_chat")) { "ST" } else { channel };
    let (base, extra) = event_words(event.platform, &event.event, &event.trigger, message);
    let mut trigger = format!("{} {} {}", event.platform.name(), channel, base);
    if !extra.is_empty() {
        trigger.push(' ');
        trigger.push_str(&extra);
    }
    trigger
}

/// The event's messages, in the order Theater Reader sent them: the chat line
/// or the alert (unless shared chat's are off), then the trigger feed. None
/// when the chat turned the event off.
pub fn messages(event: &RawEvent, options: &Options, msg_id: &str, avatar: Option<&str>) -> Vec<Value> {
    if !options.event_on {
        return Vec::new();
    }
    let mut data = event.data.clone();
    let msg_text = text(&data, "message").unwrap_or("").to_string();
    let username = text(&data, "username").or_else(|| text(&data, "author")).unwrap_or("");
    let username = username.strip_prefix('@').unwrap_or(username).to_string();
    let trigger = trigger_of(event);
    if !data.contains_key("subscriber") {
        data.insert("subscriber".into(), Value::from("unknown"));
    }

    let shared = truthy(data.get("is_shared_chat"));
    let chat = is_chat(&event.event);
    if shared && !chat && !options.shared.alerts {
        return Vec::new();
    }

    let mut out = Vec::new();
    if !(shared && !options.shared.overlay) {
        if chat {
            let mut line = json!({
                "type": if event.event.to_lowercase().contains("whisper") { "whisper" } else { "chat" },
                "trigger": event.event,
                "msg_id": msg_id,
                // Theater Reader's parser name, capitalized: "Youtube"
                "source": match event.platform { Platform::YouTube => "Youtube", p => p.name() },
                "message": msg_text,
                "username": text(&data, "username").or_else(|| text(&data, "author")).unwrap_or("Unknown"),
            });
            match event.platform {
                Platform::Twitch => {
                    line["badges"] = Value::from(text(&data, "badges_html").unwrap_or(""));
                    line["color"] = Value::from(text(&data, "color").unwrap_or(""));
                }
                Platform::Kick => {
                    line["badges"] = Value::from(text(&data, "badges").unwrap_or(""));
                    if let Some(color) = text(&data, "color") {
                        line["color"] = Value::from(color);
                    }
                }
                Platform::YouTube => {}
            }
            if let Some(avatar) = avatar.filter(|a| !a.is_empty()) {
                line["avatar"] = Value::from(avatar);
            }
            out.push(line);
        } else {
            // With the shared alert (theater_shared::alert), for scripts and the Control Panel
            out.push(theater_shared::alert::with_alert(json!({
                "type": "alert",
                "source": event.platform.parser_name(),
                "event": event.event,
                "trigger": trigger,
                "data": data,
            })));
        }
    }
    out.push(json!({
        "type": "moderation",
        "payload": {
            "trigger": trigger,
            "msg_id": msg_id,
            "platform": event.platform.name().to_lowercase(),
            "username": username,
            "message": msg_text,
            "customData": data,
            "is_suspicious": false,
            "chat_moderate_enabled": false,
        }
    }));
    out
}

/// Theater Reader's alert tester (test_sammi_trigger): the trigger feed's
/// message, then the alert (or the chat line, for a chat trigger), from
/// "Test UI", on the "testserver" channel.
pub fn test_messages(trigger: &str, user: &str, amount: &str, now: u64) -> Vec<Value> {
    let trigger = trigger.trim();
    if trigger.is_empty() {
        return Vec::new();
    }
    let lower = trigger.to_lowercase();
    let platform = ["Twitch", "Kick", "YouTube"].into_iter().find(|p| lower.starts_with(&format!("{} ", p.to_lowercase())));
    let trigger = match platform {
        Some(p) if !lower.contains("testserver") => {
            let rest = trigger[p.len()..].trim();
            // "Twitch * redeem X" (a pattern) and "Twitch redeem X" both test "Twitch testserver redeem X"
            let rest = rest.strip_prefix("* ").unwrap_or(rest);
            format!("{} testserver {}", p, rest)
        }
        None if !lower.contains("testserver") => format!("testserver {}", trigger),
        _ => trigger.to_string(),
    };
    // A pattern's other wildcards (an amount's) are the amount
    let trigger = if amount.trim().is_empty() { trigger } else { trigger.replace('*', amount.trim()) };
    let user = if user.trim().is_empty() { "TestUser" } else { user.trim() };
    let msg_id = format!("test-uuid-{}", now);
    let data = json!({
        "is_test": true,
        "username": user,
        "badges_html": "",
        "color": "#FF5733",
        "amount": amount,
        "tier": amount,
        "months": amount,
        "gifted": false,
    });
    let feed = json!({
        "type": "moderation",
        "payload": {
            "trigger": trigger,
            "msg_id": msg_id,
            "platform": platform.unwrap_or("Test"),
            "username": user,
            "message": amount,
            "customData": data,
        }
    });
    let shown = if trigger.to_lowercase().contains("chat") {
        json!({
            "type": "chat",
            "trigger": trigger,
            "msg_id": msg_id,
            "source": "Test UI",
            "message": amount,
            "username": user,
        })
    } else {
        theater_shared::alert::with_alert(json!({
            "type": "alert",
            "source": "Test UI",
            "event": trigger,
            "trigger": trigger,
            "data": data,
        }))
    };
    vec![feed, shown]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(platform: Platform, channel: &str, event: &str, trigger: &str, data: Value) -> RawEvent {
        RawEvent {
            platform,
            channel: channel.into(),
            event: event.into(),
            trigger: trigger.into(),
            data: data.as_object().cloned().unwrap(),
        }
    }

    fn on() -> Options {
        Options { event_on: true, shared: SharedChat::default() }
    }

    #[test]
    fn triggers_are_theater_readers() {
        let cases = [
            (Platform::Twitch, "Twitch redeem (pubsub)", "Twitch redeem Hydrate", "Twitch mortygotvr redeem Hydrate"),
            (Platform::Twitch, "Twitch redeem (irc)", "Twitch redeem 1a2b3c4d...", "Twitch mortygotvr redeem 1a2b3c4d..."),
            (Platform::Twitch, "Twitch cheer", "Twitch cheer 500 bits", "Twitch mortygotvr cheer 500 bits"),
            (Platform::Twitch, "Twitch hype train", "Twitch hype train 3", "Twitch mortygotvr hype train 3"),
            (Platform::Twitch, "Twitch sub", "Twitch sub", "Twitch mortygotvr sub"),
            (Platform::Twitch, "Twitch other", "Twitch other (NOTICE)", "Twitch mortygotvr other"),
            (Platform::Kick, "Kick redeem", "Kick redeem Hydrate", "Kick mortygotvr redeem Hydrate"),
            (Platform::Kick, "Kick raid start", "Kick raid start", "Kick mortygotvr raid"),
            (Platform::Kick, "Kick gift sub", "Kick gift sub", "Kick mortygotvr gift sub"),
            (Platform::Kick, "Kick other", "Kick other (PinnedMessageEvent)", "Kick mortygotvr other"),
        ];
        for (platform, event, parser_trigger, want) in cases {
            let e = raw(platform, "mortygotvr", event, parser_trigger, json!({}));
            assert_eq!(trigger_of(&e), want);
        }
        let yt = "https://www.youtube.com/watch?v=abcdefghijk";
        let e = raw(Platform::YouTube, yt, "paid_message", "Super Chat: $5.00", json!({"author": "A"}));
        assert_eq!(trigger_of(&e), format!("YouTube {} super chat $5.00", yt));
        let e = raw(Platform::YouTube, yt, "chat_message", "YouTube Chat", json!({"message": "hi there"}));
        assert_eq!(trigger_of(&e), format!("YouTube {} chat hi there", yt));
        let e = raw(Platform::YouTube, yt, "member_message", "New Member!", json!({}));
        assert_eq!(trigger_of(&e), format!("YouTube {} member", yt));
        // Shared chat's channel is ST
        let e = raw(Platform::Twitch, "mortygotvr", "Twitch sub", "Twitch sub", json!({"is_shared_chat": true}));
        assert_eq!(trigger_of(&e), "Twitch ST sub");
    }

    #[test]
    fn an_alert_then_the_trigger_feed() {
        let e = raw(
            Platform::Twitch,
            "mortygotvr",
            "Twitch redeem (pubsub)",
            "Twitch redeem Hydrate",
            json!({"reward_title": "Hydrate", "username": "viewer1", "message": ""}),
        );
        let out = messages(&e, &on(), "id-1", None);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["type"], "alert");
        assert_eq!(out[0]["source"], "TwitchParser");
        assert_eq!(out[0]["event"], "Twitch redeem (pubsub)");
        assert_eq!(out[0]["trigger"], "Twitch mortygotvr redeem Hydrate");
        assert_eq!(out[0]["data"]["subscriber"], "unknown");
        // The shared alert, beside the platform's data
        assert_eq!(out[0]["alert"]["kind"], "redeem");
        assert_eq!(out[0]["alert"]["title"], "Hydrate");
        assert_eq!(out[0]["alert"]["user"], "viewer1");
        assert_eq!(out[0]["alert"]["channel"], "mortygotvr");
        assert_eq!(out[1]["type"], "moderation");
        assert_eq!(out[1]["payload"]["trigger"], "Twitch mortygotvr redeem Hydrate");
        assert_eq!(out[1]["payload"]["platform"], "twitch");
        assert_eq!(out[1]["payload"]["username"], "viewer1");
        assert_eq!(out[1]["payload"]["msg_id"], "id-1");
        assert_eq!(out[1]["payload"]["customData"]["reward_title"], "Hydrate");
    }

    #[test]
    fn a_chat_line_like_theater_readers() {
        let e = raw(
            Platform::Twitch,
            "mortygotvr",
            "Twitch chat",
            "Twitch chat",
            json!({"username": "Mo", "message": "hi [twitch_emote:25]", "badges_html": "[MOD]", "color": "#9146FF", "subscriber": false}),
        );
        let out = messages(&e, &on(), "id-2", Some("https://static-cdn.jtvnw.net/a.png"));
        assert_eq!(out[0], json!({
            "type": "chat", "trigger": "Twitch chat", "msg_id": "id-2", "source": "Twitch",
            "message": "hi [twitch_emote:25]", "username": "Mo", "badges": "[MOD]", "color": "#9146FF",
            "avatar": "https://static-cdn.jtvnw.net/a.png"
        }));
        assert_eq!(out[1]["payload"]["trigger"], "Twitch mortygotvr chat");
        let yt = raw(Platform::YouTube, "@me", "chat_message", "YouTube Chat", json!({"username": "A", "author": "A", "message": "yo"}));
        assert_eq!(messages(&yt, &on(), "id", None)[0]["source"], "Youtube");
    }

    #[test]
    fn events_turned_off_and_shared_chat() {
        let e = raw(Platform::Twitch, "m", "Twitch sub", "Twitch sub", json!({}));
        assert!(messages(&e, &Options { event_on: false, shared: SharedChat::default() }, "x", None).is_empty());

        let shared_sub = raw(Platform::Twitch, "m", "Twitch sub", "Twitch sub", json!({"is_shared_chat": true}));
        assert!(messages(&shared_sub, &on(), "x", None).is_empty(), "shared chat's alerts are off");
        let with_alerts = Options { event_on: true, shared: SharedChat { overlay: false, alerts: true } };
        let out = messages(&shared_sub, &with_alerts, "x", None);
        assert_eq!(out.len(), 1, "no alert shown (overlay off), the trigger feed only");
        assert_eq!(out[0]["type"], "moderation");

        let shared_chat = raw(Platform::Twitch, "m", "Twitch chat", "Twitch chat", json!({"is_shared_chat": true, "message": "x"}));
        let out = messages(&shared_chat, &on(), "x", None);
        assert_eq!(out.len(), 1, "its chat isn't shown");
        let shown = Options { event_on: true, shared: SharedChat { overlay: true, alerts: false } };
        assert_eq!(messages(&shared_chat, &shown, "x", None)[0]["type"], "chat");
    }

    #[test]
    fn the_tester_says_what_theater_readers_did() {
        let out = test_messages("Twitch * redeem Hydrate", "Tester", "500", 1700000000);
        assert_eq!(out[0]["type"], "moderation");
        assert_eq!(out[0]["payload"]["trigger"], "Twitch testserver redeem Hydrate");
        assert_eq!(out[0]["payload"]["platform"], "Twitch");
        assert_eq!(out[1]["type"], "alert");
        assert_eq!(out[1]["source"], "Test UI");
        assert_eq!(out[1]["event"], "Twitch testserver redeem Hydrate");
        assert_eq!(out[1]["data"]["amount"], "500");
        let out = test_messages("Twitch * cheer * bits", "", "100", 1);
        assert_eq!(out[1]["trigger"], "Twitch testserver cheer 100 bits");
        let out = test_messages("Twitch cheer", "", "100", 1);
        assert_eq!(out[1]["trigger"], "Twitch testserver cheer");
        assert_eq!(out[1]["data"]["username"], "TestUser");
        assert_eq!(test_messages("Kick chat", "a", "hello", 1)[1]["type"], "chat");
    }
}
