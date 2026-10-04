//! Kick: the channel's ids from Kick's site, then its chat and events over
//! Pusher (Kick's public app key), like Theater Reader's parsers/kick.py.

use std::collections::HashMap;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde_json::{Map, Value, json};
use tokio_tungstenite::tungstenite::Message;

use super::{Backoff, Link, Platform, Sink};

const PUSHER: &str = "wss://ws-us2.pusher.com/app/32cbd69e4b950bf97679?protocol=7&client=js&version=8.4.0&flash=false";
const BROWSER: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";

/// Channel name → its picture
static AVATARS: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

/// The channel's picture (the chat's avatar), once its ids were read
pub fn channel_avatar(channel: &str) -> Option<String> {
    AVATARS.lock().as_ref().and_then(|a| a.get(&channel.to_lowercase()).cloned())
}

/// Reads the chat and events of `channel` until stopped.
pub async fn run(channel: String, sink: Sink) {
    let mut backoff = Backoff::new();
    loop {
        sink.link("chat", Link::Connecting);
        match once(&channel, &sink, &mut backoff).await {
            Ok(()) => sink.link("chat", Link::Failed("disconnected, reconnecting".into())),
            Err(e) => sink.link("chat", Link::Failed(format!("{} (again in {} s)", e, backoff.secs()))),
        }
        backoff.wait().await;
    }
}

/// The channel page's JSON, through Windows' curl first: Kick's Cloudflare
/// turns other programs away more often (Theater Reader did the same)
async fn channel_json(url: String) -> Option<Value> {
    #[cfg(target_os = "windows")]
    {
        let u = url.clone();
        let got = tokio::task::spawn_blocking(move || {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            std::process::Command::new("curl.exe")
                .args(["-s", "--max-time", "10", "-H"])
                .arg(format!("User-Agent: {}", BROWSER))
                .args(["-H", "Accept-Language: en-US,en;q=0.9", "-H", "Referer: https://kick.com/", &u])
                .creation_flags(CREATE_NO_WINDOW)
                .output()
                .ok()
                .filter(|o| o.status.success())
                .and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok())
        })
        .await
        .ok()
        .flatten();
        if got.as_ref().is_some_and(|v| v.is_object()) {
            return got;
        }
    }
    let resp = super::http()
        .get(&url)
        .header("Accept", "application/json")
        .header("Accept-Language", "en-US,en;q=0.9")
        .header("Referer", "https://kick.com/")
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json::<Value>().await.ok().filter(|v| v.is_object())
}

/// (chatroom id, channel id)
async fn ids(channel: &str) -> Result<(u64, u64), String> {
    let name = channel.to_lowercase();
    for url in [format!("https://kick.com/api/v1/channels/{}", name), format!("https://kick.com/api/v2/channels/{}", name)] {
        let Some(v) = channel_json(url).await else { continue };
        if let Some(pic) = v["user"]["profile_pic"].as_str() {
            AVATARS.lock().get_or_insert_with(HashMap::new).insert(name.clone(), pic.to_string());
        }
        if let (Some(chat), Some(chan)) = (v["chatroom"]["id"].as_u64(), v["id"].as_u64()) {
            return Ok((chat, chan));
        }
    }
    Err(format!("couldn't read Kick channel '{}' (is the name right?)", channel))
}

async fn once(channel: &str, sink: &Sink, backoff: &mut Backoff) -> Result<(), String> {
    let (chat_id, chan_id) = ids(channel).await?;
    let (ws, _) = tokio_tungstenite::connect_async(PUSHER).await.map_err(|e| e.to_string())?;
    let (mut write, mut read) = ws.split();
    for topic in [
        format!("chatrooms.{}.v2", chat_id),
        format!("chatroom_{}", chat_id),
        format!("chatrooms.{}", chat_id),
        format!("channel.{}", chan_id),
        format!("channel_{}", chan_id),
    ] {
        let subscribe = json!({ "event": "pusher:subscribe", "data": { "auth": "", "channel": topic } });
        write.send(Message::text(subscribe.to_string())).await.map_err(|e| e.to_string())?;
    }
    sink.link("chat", Link::Connected);
    backoff.reset();
    loop {
        // Pusher pings after 2 minutes of quiet
        let next = tokio::time::timeout(Duration::from_secs(200), read.next()).await.map_err(|_| "nothing heard for too long")?;
        let text = match next {
            Some(Ok(Message::Text(t))) => t.to_string(),
            Some(Ok(Message::Close(_))) | None => return Ok(()),
            Some(Ok(_)) => continue,
            Some(Err(e)) => return Err(e.to_string()),
        };
        if text.contains("\"pusher:ping\"") {
            write.send(Message::text(r#"{"event":"pusher:pong","data":{}}"#)).await.map_err(|e| e.to_string())?;
            continue;
        }
        if text.contains("pusher:error") || text.contains("subscription_error") {
            crate::log(&format!("[Reader] Kick '{}': {}", channel, text));
            continue;
        }
        if let Some((event, trigger, data)) = parse_frame(&text) {
            sink.send(Platform::Kick, channel, &event, trigger, data);
        }
    }
}

/// Events Theater Reader knew but made nothing of: "Kick other"
const OTHER: [&str; 8] = [
    "PinnedMessageEvent",
    "HostStartedEvent",
    "HostEndedEvent",
    "PollStartedEvent",
    "PollEndedEvent",
    "ChatClearedEvent",
    "EmoteCreatedEvent",
    "EmoteDeletedEvent",
];

fn text_of(v: &Value) -> Option<String> {
    v.as_str().map(str::to_string)
}

/// A Pusher frame as Theater Reader's parse_frame: (event, trigger, data)
pub fn parse_frame(frame: &str) -> Option<(String, String, Map<String, Value>)> {
    let d: Value = serde_json::from_str(frame).ok()?;
    let full_name = d["event"].as_str()?;
    if full_name.starts_with("pusher") {
        return None;
    }
    let name = full_name.rsplit('\\').next().unwrap_or(full_name);
    let mut payload = match &d["data"] {
        Value::String(s) => serde_json::from_str::<Value>(s).unwrap_or(json!({ "raw": s })),
        Value::Object(_) => d["data"].clone(),
        Value::Null => json!({}),
        other => json!({ "raw": other }),
    };
    if !payload.is_object() {
        payload = json!({ "raw": payload });
    }
    let p = payload.as_object_mut()?;
    let username_from = |p: &Map<String, Value>, key: &str| p.get(key).and_then(text_of).unwrap_or_else(|| "Unknown".into());
    let set_user = |p: &mut Map<String, Value>, name: String| {
        p.insert("username".into(), Value::from(name.clone()));
        p.insert("user".into(), Value::from(name));
    };

    let (event, title) = match name {
        "ChatMessageEvent" => {
            let sender = p.get("sender").cloned().unwrap_or(json!({}));
            set_user(p, sender["username"].as_str().unwrap_or("Unknown").to_string());
            let content = p.get("content").and_then(text_of).unwrap_or_default();
            p.insert("message".into(), Value::from(content));
            p.insert("color".into(), Value::from(sender["identity"]["color"].as_str().unwrap_or("#CCCCCC")));
            let (mut badges, mut sub, mut moderator, mut vip) = (String::new(), false, false, false);
            for b in sender["identity"]["badges"].as_array().into_iter().flatten() {
                let kind = match b {
                    Value::Object(o) if o.get("active").and_then(|a| a.as_bool()).unwrap_or(true) => {
                        o.get("type").and_then(|t| t.as_str()).unwrap_or("")
                    }
                    Value::String(s) => s.as_str(),
                    _ => "",
                };
                if kind.contains("broadcaster") {
                    badges.push_str("[BROADCASTER]");
                    moderator = true;
                } else if kind.contains("moderator") {
                    badges.push_str("[MOD]");
                    moderator = true;
                } else if kind.contains("subscriber") {
                    badges.push_str("[SUB]");
                    sub = true;
                } else if kind.contains("vip") {
                    badges.push_str("[VIP]");
                    vip = true;
                } else if kind.contains("founder") {
                    badges.push_str("[FOUNDER]");
                    sub = true;
                } else if kind.contains("og") {
                    badges.push_str("[OG]");
                }
            }
            p.insert("badges".into(), Value::from(badges));
            p.insert("subscriber".into(), Value::from(sub));
            p.insert("mod".into(), Value::from(moderator));
            p.insert("vip".into(), Value::from(vip));
            ("Kick chat", String::new())
        }
        "RewardRedeemedEvent" => {
            let user = username_from(p, "username");
            set_user(p, user);
            let input = p.get("user_input").and_then(text_of).unwrap_or_default();
            p.insert("message".into(), Value::from(input));
            let title = p
                .get("reward_title")
                .and_then(text_of)
                .filter(|t| !t.is_empty())
                .or_else(|| p.get("reward").and_then(|r| r.get("title")).and_then(text_of))
                .or_else(|| p.get("title").and_then(text_of))
                .unwrap_or_else(|| "Unknown".into());
            ("Kick redeem", title)
        }
        "FollowEvent" => {
            let user = username_from(p, "username");
            set_user(p, user);
            ("Kick follow", String::new())
        }
        // Kick's own name for a follow: the count changed, and who did it
        "FollowersUpdated" => {
            if p.get("followed").and_then(|f| f.as_bool()) != Some(true) || p.get("username").and_then(|u| u.as_str()).is_none() {
                return None;
            }
            let user = username_from(p, "username");
            set_user(p, user);
            ("Kick follow", String::new())
        }
        "SubscriptionEvent" => {
            let user = username_from(p, "username");
            set_user(p, user);
            ("Kick sub", String::new())
        }
        "GiftedSubscriptionEvent" | "GiftedSubscriptionsEvent" => {
            let user = username_from(p, "gifter_username");
            set_user(p, user);
            ("Kick gift sub", String::new())
        }
        "RaidStartedEvent" | "StreamHostEvent" => {
            let user = username_from(p, "host_username");
            set_user(p, user);
            ("Kick raid start", String::new())
        }
        "RaidEndedEvent" => ("Kick raid end", String::new()),
        "UserBannedEvent" | "UserTimedOutEvent" => {
            let user = p
                .get("user")
                .and_then(|u| u.get("username"))
                .and_then(text_of)
                .or_else(|| p.get("username").and_then(text_of))
                .unwrap_or_else(|| "Unknown".into());
            set_user(p, user);
            (if name == "UserBannedEvent" { "Kick ban" } else { "Kick timeout" }, String::new())
        }
        "StreamStartedEvent" => ("Kick stream start", String::new()),
        "StreamEndedEvent" => ("Kick stream end", String::new()),
        n if OTHER.contains(&n) => ("Kick other", String::new()),
        _ => return None,
    };
    let trigger = match event {
        "Kick redeem" => format!("Kick redeem {}", title),
        "Kick other" => format!("Kick other ({})", name),
        e => e.to_string(),
    };
    Some((event.to_string(), trigger, payload.as_object()?.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(event: &str, data: Value) -> String {
        json!({ "event": format!("App\\Events\\{}", event), "data": data.to_string(), "channel": "chatrooms.1.v2" }).to_string()
    }

    #[test]
    fn a_chat_message() {
        let f = frame(
            "ChatMessageEvent",
            json!({ "content": "hi [emote:37226:KEKW]", "sender": { "username": "Mo", "identity": { "color": "#75FD46", "badges": [ { "type": "moderator", "text": "Moderator" }, { "type": "subscriber", "text": "Subscriber", "count": 3 } ] } } }),
        );
        let (event, trigger, data) = parse_frame(&f).unwrap();
        assert_eq!((event.as_str(), trigger.as_str()), ("Kick chat", "Kick chat"));
        assert_eq!(data["username"], "Mo");
        assert_eq!(data["message"], "hi [emote:37226:KEKW]");
        assert_eq!(data["badges"], "[MOD][SUB]");
        assert_eq!(data["color"], "#75FD46");
        assert_eq!(data["mod"], true);
        assert_eq!(data["subscriber"], true);
    }

    #[test]
    fn redeems_subs_gifts_raids_and_bans() {
        let (event, trigger, data) = parse_frame(&frame("RewardRedeemedEvent", json!({ "reward_title": "Hydrate", "username": "v", "user_input": "now" }))).unwrap();
        assert_eq!((event.as_str(), trigger.as_str()), ("Kick redeem", "Kick redeem Hydrate"));
        assert_eq!(data["message"], "now");
        let (_, trigger, _) = parse_frame(&frame("RewardRedeemedEvent", json!({ "reward": { "title": "Stretch" } }))).unwrap();
        assert_eq!(trigger, "Kick redeem Stretch");

        let (event, _, data) = parse_frame(&frame("GiftedSubscriptionsEvent", json!({ "gifter_username": "Santa", "gifted_usernames": ["a", "b"] }))).unwrap();
        assert_eq!(event, "Kick gift sub");
        assert_eq!(data["username"], "Santa");

        let (event, _, data) = parse_frame(&frame("StreamHostEvent", json!({ "host_username": "Raider", "number_viewers": 12 }))).unwrap();
        assert_eq!(event, "Kick raid start");
        assert_eq!(data["username"], "Raider");

        let (event, _, data) = parse_frame(&frame("UserBannedEvent", json!({ "user": { "username": "Bad" } }))).unwrap();
        assert_eq!(event, "Kick ban");
        assert_eq!(data["username"], "Bad");

        let (event, trigger, _) = parse_frame(&frame("PinnedMessageEvent", json!({}))).unwrap();
        assert_eq!((event.as_str(), trigger.as_str()), ("Kick other", "Kick other (PinnedMessageEvent)"));

        let (event, _, data) = parse_frame(&frame("FollowersUpdated", json!({ "followersCount": 10, "followed": true, "username": "Fan" }))).unwrap();
        assert_eq!(event, "Kick follow");
        assert_eq!(data["username"], "Fan");
        assert!(parse_frame(&frame("FollowersUpdated", json!({ "followersCount": 9, "followed": false }))).is_none());
    }

    #[test]
    fn pushers_own_frames_and_unknown_events_are_nothing() {
        assert!(parse_frame(r#"{"event":"pusher:connection_established","data":"{}"}"#).is_none());
        assert!(parse_frame(r#"{"event":"pusher_internal:subscription_succeeded","data":"{}","channel":"x"}"#).is_none());
        assert!(parse_frame(&frame("ReactionCreatedEvent", json!({}))).is_none());
        assert!(parse_frame("not json").is_none());
    }
}
