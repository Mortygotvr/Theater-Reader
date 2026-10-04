//! Twitch: the chat (IRC over a websocket, anonymously), and channel points
//! and hype trains (Twitch's "hermes" pubsub, with the web player's public
//! client id), like Theater Reader's parsers/twitch.py.

use std::collections::HashMap;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde_json::{Map, Value, json};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use super::{Backoff, Link, Platform, Sink};

const CLIENT_ID: &str = "kimne78kx3ncx6brgo4mv6wki5h1ko";
const GQL: &str = "https://gql.twitch.tv/gql";

/// Global badges: set → version → picture
static BADGES: Mutex<Option<HashMap<String, HashMap<String, String>>>> = Mutex::new(None);
/// Channel id → its picture
static AVATARS: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);
/// Reward id → title
static REWARDS: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

/// Reads the chat and the channel points of `channel` until stopped.
pub async fn run(channel: String, sink: Sink) {
    let channel = channel.trim().trim_start_matches('#').to_string();
    tokio::join!(run_chat(channel.clone(), sink.clone()), run_points(channel, sink));
}

// ---------------------------------------------------------------------------
// GQL

async fn gql(body: Value) -> Result<Value, String> {
    let resp = super::http()
        .post(GQL)
        .header("Client-Id", CLIENT_ID)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("GQL said {}", resp.status()));
    }
    resp.json().await.map_err(|e| e.to_string())
}

/// Twitch's global badges' pictures (once).
pub async fn load_badges() {
    if BADGES.lock().is_some() {
        return;
    }
    let Ok(v) = gql(json!({ "query": "query GetGlobalBadges { badges { setID version imageURL } }" })).await else { return };
    let mut db: HashMap<String, HashMap<String, String>> = HashMap::new();
    for b in v["data"]["badges"].as_array().into_iter().flatten() {
        let (Some(set), Some(version), Some(url)) = (b["setID"].as_str(), b["version"].as_str(), b["imageURL"].as_str()) else {
            continue;
        };
        // The 2x picture
        let url = url.strip_suffix("/1").map(|u| format!("{}/2", u)).unwrap_or_else(|| url.to_string());
        db.entry(set.to_string()).or_default().insert(version.to_string(), url);
    }
    *BADGES.lock() = Some(db);
}

async fn channel_id(login: &str) -> Result<String, String> {
    let v = gql(json!({
        "query": "query($login: String!) { user(login: $login) { id } }",
        "variables": { "login": login.to_lowercase() },
    }))
    .await?;
    v["data"]["user"]["id"].as_str().map(str::to_string).ok_or_else(|| format!("no Twitch channel is called '{}'", login))
}

/// A channel's picture (the chat's avatar)
pub async fn room_avatar(id: &str) -> Option<String> {
    if let Some(url) = AVATARS.lock().as_ref().and_then(|a| a.get(id).cloned()) {
        return Some(url);
    }
    let v = gql(json!({
        "query": "query($id: ID!) { user(id: $id) { login profileImageURL(width: 70) } }",
        "variables": { "id": id },
    }))
    .await
    .ok()?;
    let url = v["data"]["user"]["profileImageURL"].as_str()?.to_string();
    AVATARS.lock().get_or_insert_with(HashMap::new).insert(id.to_string(), url.clone());
    Some(url)
}

/// The channel's rewards (id → title), which are also its titles for the
/// trigger builder.
async fn load_rewards(channel_id: &str, channel: &str) -> Vec<String> {
    let Ok(v) = gql(json!({
        "query": "query GetChannelRewards($channelID: ID!) { channel(id: $channelID) { communityPointsSettings { customRewards { id title } } } }",
        "variables": { "channelID": channel_id },
    }))
    .await
    else {
        return Vec::new();
    };
    let mut titles = Vec::new();
    let mut rewards = REWARDS.lock();
    let rewards = rewards.get_or_insert_with(HashMap::new);
    for r in v["data"]["channel"]["communityPointsSettings"]["customRewards"].as_array().into_iter().flatten() {
        if let (Some(id), Some(title)) = (r["id"].as_str(), r["title"].as_str()) {
            rewards.insert(id.to_string(), title.to_string());
            titles.push(title.to_string());
        }
    }
    theater_shared::seen::add(Platform::Twitch, channel, &titles);
    titles
}

async fn reward_title(channel_id: &str, channel: &str, reward_id: &str) -> Option<String> {
    let known = || REWARDS.lock().as_ref().and_then(|r| r.get(reward_id).cloned());
    if let Some(title) = known() {
        return Some(title);
    }
    load_rewards(channel_id, channel).await;
    known()
}

// ---------------------------------------------------------------------------
// The chat (IRC)

async fn run_chat(channel: String, sink: Sink) {
    let mut backoff = Backoff::new();
    loop {
        sink.link("chat", Link::Connecting);
        match chat_once(&channel, &sink, &mut backoff).await {
            Ok(()) => sink.link("chat", Link::Failed("disconnected, reconnecting".into())),
            Err(e) => sink.link("chat", Link::Failed(format!("{} (again in {} s)", e, backoff.secs()))),
        }
        backoff.wait().await;
    }
}

async fn chat_once(channel: &str, sink: &Sink, backoff: &mut Backoff) -> Result<(), String> {
    let (ws, _) = tokio_tungstenite::connect_async("wss://irc-ws.chat.twitch.tv:443").await.map_err(|e| e.to_string())?;
    let (mut write, mut read) = ws.split();
    for line in [
        "CAP REQ :twitch.tv/tags twitch.tv/commands".to_string(),
        "PASS SCHMOOPIIE".to_string(),
        format!("NICK justinfan{}", 10000 + super::random_u64() % 80000),
        format!("JOIN #{}", channel.to_lowercase()),
    ] {
        write.send(Message::text(line)).await.map_err(|e| e.to_string())?;
    }
    loop {
        // Twitch pings every 5 minutes: nothing for longer is a dead line
        let next = tokio::time::timeout(Duration::from_secs(400), read.next()).await.map_err(|_| "nothing heard for too long")?;
        let text = match next {
            Some(Ok(Message::Text(t))) => t.to_string(),
            Some(Ok(Message::Close(_))) | None => return Ok(()),
            Some(Ok(_)) => continue,
            Some(Err(e)) => return Err(e.to_string()),
        };
        for line in text.split("\r\n").filter(|l| !l.trim().is_empty()) {
            if line.starts_with("PING") {
                write.send(Message::text("PONG :tmi.twitch.tv")).await.map_err(|e| e.to_string())?;
                continue;
            }
            if line.contains(" RECONNECT") && !line.contains("PRIVMSG") {
                return Ok(());
            }
            if line.contains(" 366 ") {
                // Joined
                sink.link("chat", Link::Connected);
                backoff.reset();
            }
            let badges = BADGES.lock().clone().unwrap_or_default();
            if let Some((event, trigger, data)) = parse_irc(line, &badges) {
                sink.send(Platform::Twitch, channel, &event, trigger, data);
            }
        }
    }
}

/// IRC tags' escapes
fn unescape_tag(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some(':') => out.push(';'),
            Some('s') => out.push(' '),
            Some('\\') => out.push('\\'),
            Some('r') => out.push('\r'),
            Some('n') => out.push('\n'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

fn parse_tags(s: &str) -> HashMap<String, String> {
    s.split(';')
        .map(|part| match part.split_once('=') {
            Some((k, v)) => (k.to_string(), unescape_tag(v)),
            None => (part.to_string(), String::new()),
        })
        .collect()
}

/// A chat line's badges: pictures where Twitch has them, else [SUB] and such
pub fn badges_text(raw: &str, db: &HashMap<String, HashMap<String, String>>) -> String {
    let mut out = String::new();
    for item in raw.split(',') {
        let Some((key, version)) = item.split_once('/') else { continue };
        let url = db
            .get(key)
            .and_then(|v| v.get(version))
            .cloned()
            .or_else(|| matches!(key, "broadcaster" | "moderator").then(|| format!("https://static-cdn.jtvnw.net/badges/v1/{}/2", version)));
        match url {
            Some(url) => out.push_str(&format!("[badge:{}:{}]", url, key)),
            None => {
                let name = match key {
                    "subscriber" => "SUB".to_string(),
                    "broadcaster" => "BROADCASTER".to_string(),
                    "moderator" => "MOD".to_string(),
                    "vip" => "VIP".to_string(),
                    "founder" => "FOUNDER".to_string(),
                    other => other.to_uppercase(),
                };
                out.push_str(&format!("[{}]", name));
            }
        }
    }
    out
}

/// Emotes as [twitch_emote:ID] (the tag counts characters, not bytes)
pub fn emotes_text(text: &str, tag: &str) -> String {
    if tag.is_empty() || text.is_empty() {
        return text.to_string();
    }
    let mut places: Vec<(usize, usize, &str)> = Vec::new();
    for group in tag.split('/') {
        let Some((id, positions)) = group.split_once(':') else { continue };
        for pos in positions.split(',') {
            let Some((a, b)) = pos.split_once('-') else { continue };
            let (Ok(a), Ok(b)) = (a.parse::<usize>(), b.parse::<usize>()) else { return text.to_string() };
            places.push((a, b + 1, id));
        }
    }
    places.sort_by(|x, y| y.0.cmp(&x.0));
    let mut chars: Vec<char> = text.chars().collect();
    for (start, end, id) in places {
        if start > end || end > chars.len() {
            continue;
        }
        chars.splice(start..end, format!("[twitch_emote:{}]", id).chars());
    }
    chars.into_iter().collect()
}

/// A chat line as Theater Reader's parse_frame: (event, trigger, data)
pub fn parse_irc(line: &str, badge_db: &HashMap<String, HashMap<String, String>>) -> Option<(String, String, Map<String, Value>)> {
    let mut rest = line.trim();
    if rest.is_empty() || rest.starts_with("PING") {
        return None;
    }
    let mut tags = HashMap::new();
    if let Some(r) = rest.strip_prefix('@') {
        let (t, after) = r.split_once(' ')?;
        tags = parse_tags(t);
        rest = after.trim_start();
    }
    let mut prefix = None;
    if let Some(r) = rest.strip_prefix(':') {
        let (p, after) = r.split_once(' ').unwrap_or((r, ""));
        prefix = Some(p);
        rest = after.trim_start();
    }
    let (head, text) = match rest.find(" :") {
        Some(i) => (rest[..i].trim(), Some(&rest[i + 2..])),
        None => (rest, None),
    };
    let parts: Vec<&str> = head.split_whitespace().collect();
    let command = *parts.first()?;
    let tag = |k: &str| tags.get(k).map(String::as_str).filter(|v| !v.is_empty());

    let room_id = tag("room-id");
    let source_room_id = tag("source-room-id");
    let shared = matches!((room_id, source_room_id), (Some(a), Some(b)) if a != b);
    let mut base = Map::new();
    base.insert("room_id".into(), room_id.map(Value::from).unwrap_or(Value::Null));
    base.insert("source_room_id".into(), source_room_id.map(Value::from).unwrap_or(Value::Null));
    base.insert("is_shared_chat".into(), Value::from(shared));
    base.insert("user_id".into(), tag("user-id").map(Value::from).unwrap_or(Value::Null));
    let with = |mut data: Map<String, Value>| {
        data.extend(base.clone());
        data
    };

    match command {
        "PRIVMSG" => {
            let raw_badges = tags.get("badges").map(String::as_str).unwrap_or("");
            let badges = badges_text(raw_badges, badge_db);
            let user = tag("display-name")
                .map(str::to_string)
                .or_else(|| prefix.map(|p| p.split('!').next().unwrap_or(p).to_string()))
                .unwrap_or_else(|| "Unknown".into());
            let message = emotes_text(text.unwrap_or(""), tags.get("emotes").map(String::as_str).unwrap_or(""));
            let subscriber = raw_badges.contains("subscriber") || raw_badges.contains("founder") || tag("subscriber") == Some("1");
            let is_mod = tag("mod") == Some("1") || raw_badges.contains("moderator") || raw_badges.contains("broadcaster");
            let is_vip = raw_badges.contains("vip");
            let common = |extra: Value| {
                let mut data = extra.as_object().cloned().unwrap_or_default();
                data.insert("username".into(), Value::from(user.clone()));
                data.insert("user".into(), Value::from(user.clone()));
                data.insert("message".into(), Value::from(message.clone()));
                data.insert("badges_html".into(), Value::from(badges.clone()));
                data.insert("subscriber".into(), Value::from(subscriber));
                data.insert("mod".into(), Value::from(is_mod));
                data.insert("vip".into(), Value::from(is_vip));
                with(data)
            };
            if let Some(reward) = tag("custom-reward-id") {
                let short: String = reward.chars().take(8).collect();
                return Some(("Twitch redeem (irc)".into(), format!("Twitch redeem {}...", short), common(json!({ "reward_id": reward }))));
            }
            if let Some(bits) = tag("bits") {
                return Some(("Twitch cheer".into(), format!("Twitch cheer {} bits", bits), common(json!({ "bits": bits }))));
            }
            let color = tag("color").unwrap_or("#FFFFFF");
            Some(("Twitch chat".into(), "Twitch chat".into(), common(json!({ "color": color }))))
        }
        "USERNOTICE" => {
            let mut kind = tag("msg-id").unwrap_or("");
            if kind == "sharedchatnotice" {
                kind = tag("source-msg-id").unwrap_or(kind);
            }
            let username = tag("display-name").or(tag("login")).map(Value::from).unwrap_or(Value::Null);
            if matches!(kind, "sub" | "resub" | "subgift" | "anonsubgift" | "giftpaidupgrade" | "submysterygift") {
                let data = json!({
                    "username": username,
                    "type": kind,
                    "plan": tag("msg-param-sub-plan").unwrap_or(""),
                    "message": text.map(Value::from).unwrap_or(Value::Null),
                });
                return Some(("Twitch sub".into(), "Twitch sub".into(), with(data.as_object().cloned().unwrap_or_default())));
            }
            if kind == "raid" {
                let raider = tag("msg-param-login").map(Value::from).unwrap_or(username);
                let data = json!({ "username": raider, "viewers": tag("msg-param-viewerCount").unwrap_or("0") });
                return Some(("Twitch raid".into(), "Twitch raid".into(), with(data.as_object().cloned().unwrap_or_default())));
            }
            other(command, line, with(Map::new()))
        }
        "CLEARCHAT" => {
            let target = text.unwrap_or("").to_string();
            let target = if target.is_empty() { parts.get(1).copied().unwrap_or("").to_string() } else { target };
            if let Some(duration) = tags.get("ban-duration") {
                let data = json!({ "username": target, "duration": duration });
                return Some(("Twitch timeout".into(), "Twitch timeout".into(), with(data.as_object().cloned().unwrap_or_default())));
            }
            Some(("Twitch ban".into(), "Twitch ban".into(), with(json!({ "username": target }).as_object().cloned().unwrap_or_default())))
        }
        _ => other(command, line, with(Map::new())),
    }
}

fn other(command: &str, line: &str, mut data: Map<String, Value>) -> Option<(String, String, Map<String, Value>)> {
    const NOISY: [&str; 17] = [
        "JOIN", "PART", "PING", "PONG", "CAP", "HOSTTARGET", "RECONNECT", "353", "366", "001", "002", "003", "004", "375", "372",
        "376", "USERSTATE",
    ];
    if NOISY.contains(&command) || command == "ROOMSTATE" || command == "GLOBALUSERSTATE" {
        return None;
    }
    data.insert("command".into(), Value::from(command));
    data.insert("raw".into(), Value::from(line));
    Some(("Twitch other".into(), format!("Twitch other ({})", command), data))
}

// ---------------------------------------------------------------------------
// Channel points and hype trains (hermes)

async fn run_points(channel: String, sink: Sink) {
    let mut backoff = Backoff::new();
    let id = loop {
        sink.link("points", Link::Connecting);
        match channel_id(&channel).await {
            Ok(id) => break id,
            Err(e) => sink.link("points", Link::Failed(format!("{} (again in {} s)", e, backoff.secs()))),
        }
        backoff.wait().await;
    };
    load_rewards(&id, &channel).await;
    backoff.reset();
    loop {
        sink.link("points", Link::Connecting);
        match points_once(&channel, &id, &sink, &mut backoff).await {
            Ok(()) => sink.link("points", Link::Failed("disconnected, reconnecting".into())),
            Err(e) => sink.link("points", Link::Failed(format!("{} (again in {} s)", e, backoff.secs()))),
        }
        backoff.wait().await;
    }
}

fn nonce() -> String {
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-";
    (0..22).map(|_| CHARS[(super::random_u64() % CHARS.len() as u64) as usize] as char).collect()
}

async fn points_once(channel: &str, id: &str, sink: &Sink, backoff: &mut Backoff) -> Result<(), String> {
    let mut request = format!("wss://hermes.twitch.tv/v1?clientId={}", CLIENT_ID).into_client_request().map_err(|e| e.to_string())?;
    request.headers_mut().insert("Origin", "https://www.twitch.tv".parse().map_err(|_| "header")?);
    let (ws, _) = tokio_tungstenite::connect_async(request).await.map_err(|e| e.to_string())?;
    let (mut write, mut read) = ws.split();
    let topics = [
        format!("community-points-channel-v1.{}", id),
        format!("video-playback-by-id.{}", id),
        format!("raid.{}", id),
        format!("polls.{}", id),
        format!("predictions-channel-v1.{}", id),
        format!("hype-train-events-v1.{}", id),
    ];
    for topic in topics {
        let subscribe = json!({
            "type": "subscribe",
            "id": nonce(),
            "subscribe": { "id": nonce(), "type": "pubsub", "pubsub": { "topic": topic } },
            "timestamp": super::utc_now(),
        });
        write.send(Message::text(subscribe.to_string())).await.map_err(|e| e.to_string())?;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    sink.link("points", Link::Connected);
    backoff.reset();
    let mut keepalive = Duration::from_secs(60);
    loop {
        let next = tokio::time::timeout(keepalive + Duration::from_secs(30), read.next()).await.map_err(|_| "nothing heard for too long")?;
        let text = match next {
            Some(Ok(Message::Text(t))) => t.to_string(),
            Some(Ok(Message::Close(_))) | None => return Ok(()),
            Some(Ok(_)) => continue,
            Some(Err(e)) => return Err(e.to_string()),
        };
        let Ok(frame) = serde_json::from_str::<Value>(&text) else { continue };
        match frame["type"].as_str().unwrap_or("") {
            "welcome" => {
                if let Some(secs) = frame["welcome"]["keepaliveSec"].as_u64() {
                    keepalive = Duration::from_secs(secs.max(10));
                }
                continue;
            }
            "reconnect" => return Ok(()),
            "PING" => {
                write.send(Message::text(r#"{"type":"PONG"}"#)).await.map_err(|e| e.to_string())?;
                continue;
            }
            _ => {}
        }
        if let Some((event, mut trigger, mut data)) = parse_hermes(&frame) {
            if event == "Twitch redeem (pubsub)" && data.get("reward_title").and_then(|t| t.as_str()) == Some("Unknown") {
                if let Some(reward) = data.get("reward_id").and_then(|r| r.as_str()).map(str::to_string) {
                    if let Some(title) = reward_title(id, channel, &reward).await {
                        trigger = format!("Twitch redeem {}", title);
                        data.insert("reward_title".into(), Value::from(title));
                    }
                }
            }
            sink.send(Platform::Twitch, channel, &event, trigger, data);
        }
    }
}

/// The first object of type `kind` anywhere in `v` (JSON in strings too): its
/// data, or itself
fn find_event(v: &Value, kind: &str) -> Option<Value> {
    match v {
        Value::Object(o) => {
            if o.get("type").and_then(|t| t.as_str()) == Some(kind) {
                return Some(o.get("data").filter(|d| !d.is_null()).cloned().unwrap_or_else(|| v.clone()));
            }
            o.values().find_map(|child| find_event(child, kind))
        }
        Value::Array(a) => a.iter().find_map(|child| find_event(child, kind)),
        Value::String(s) => {
            let s = s.trim();
            if s.starts_with('{') && s.ends_with('}') {
                serde_json::from_str::<Value>(s).ok().and_then(|inner| find_event(&inner, kind))
            } else {
                None
            }
        }
        _ => None,
    }
}

fn int_of(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::Object(o) => o.get("value").and_then(|x| x.as_i64()),
        n => n.as_i64(),
    }
}

/// A hermes frame as Theater Reader's parse_frame: a redeem or a hype train
pub fn parse_hermes(frame: &Value) -> Option<(String, String, Map<String, Value>)> {
    if let Some(found) = find_event(frame, "reward-redeemed") {
        let redemption = found.get("redemption").filter(|r| r.is_object()).unwrap_or(&found);
        let user = redemption.get("user").cloned().unwrap_or(json!({}));
        let reward = redemption.get("reward").cloned().unwrap_or(json!({}));
        let image = reward["image"]["url_4x"]
            .as_str()
            .or_else(|| reward["default_image"]["url_4x"].as_str())
            .or_else(|| reward["defaultImage"]["url_4x"].as_str());
        let title = reward["title"].as_str().unwrap_or("Unknown").to_string();
        let message = redemption["user_input"].as_str().or_else(|| redemption["userInput"].as_str()).unwrap_or("");
        let data = json!({
            "reward_id": reward["id"],
            "reward_title": title,
            "cost": reward["cost"],
            "user_id": user["id"],
            "username": user["login"],
            "display_name": user["display_name"].as_str().or_else(|| user["displayName"].as_str()),
            "message": message,
            "image": image,
        });
        return Some(("Twitch redeem (pubsub)".into(), format!("Twitch redeem {}", title), data.as_object().cloned()?));
    }
    let train = find_event(frame, "hype-train-progression")
        .or_else(|| find_event(frame, "hype-train-start"))
        .or_else(|| find_event(frame, "hype-train-end"))?;
    let mut sources = vec![&train, frame];
    if let Some(progress) = train.get("progress").filter(|p| p.is_object()) {
        sources.push(progress);
        if let Some(level) = progress.get("level").filter(|l| l.is_object()) {
            sources.push(level);
        }
    }
    if let Some(level) = train.get("level").filter(|l| l.is_object()) {
        sources.push(level);
    }
    let first = |key: &str| sources.iter().filter_map(|s| int_of(s.get(key))).find(|n| *n != 0).unwrap_or(0);
    let (level, progress, goal) = (first("level"), first("progress"), first("goal"));
    let total = train.get("total").cloned().unwrap_or(Value::from(0));
    let data = json!({ "level": level, "progress": progress, "goal": goal, "total": total });
    Some(("Twitch hype train".into(), format!("Twitch hype train {}", level), data.as_object().cloned()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_badges() -> HashMap<String, HashMap<String, String>> {
        HashMap::new()
    }

    #[test]
    fn a_chat_line() {
        let line = r"@badge-info=subscriber/12;badges=moderator/1,subscriber/12;color=#9146FF;display-name=Mo;emotes=25:6-10;mod=1;room-id=111;user-id=222;subscriber=1 :mo!mo@mo.tmi.twitch.tv PRIVMSG #mortygotvr :hello Kappa there";
        let (event, trigger, data) = parse_irc(line, &no_badges()).unwrap();
        assert_eq!(event, "Twitch chat");
        assert_eq!(trigger, "Twitch chat");
        assert_eq!(data["username"], "Mo");
        assert_eq!(data["message"], "hello [twitch_emote:25] there");
        assert_eq!(data["badges_html"], "[badge:https://static-cdn.jtvnw.net/badges/v1/1/2:moderator][SUB]");
        assert_eq!(data["color"], "#9146FF");
        assert_eq!(data["subscriber"], true);
        assert_eq!(data["mod"], true);
        assert_eq!(data["room_id"], "111");
        assert_eq!(data["is_shared_chat"], false);
    }

    #[test]
    fn emotes_count_characters() {
        // "é" is two bytes, one character
        assert_eq!(emotes_text("é Kappa", "25:2-6"), "é [twitch_emote:25]");
        assert_eq!(emotes_text("Kappa Kappa", "25:0-4,6-10"), "[twitch_emote:25] [twitch_emote:25]");
        assert_eq!(emotes_text("short", "25:0-40"), "short", "out of range: left");
    }

    #[test]
    fn cheers_redeems_subs_raids_and_bans() {
        let cheer = r"@badges=;bits=500;display-name=Bob;room-id=1 :bob!bob@x PRIVMSG #m :cheer500 go";
        let (event, trigger, data) = parse_irc(cheer, &no_badges()).unwrap();
        assert_eq!((event.as_str(), trigger.as_str()), ("Twitch cheer", "Twitch cheer 500 bits"));
        assert_eq!(data["bits"], "500");

        let redeem = r"@custom-reward-id=1a2b3c4d-5e6f;display-name=Bob :bob!bob@x PRIVMSG #m :my input";
        let (event, trigger, _) = parse_irc(redeem, &no_badges()).unwrap();
        assert_eq!((event.as_str(), trigger.as_str()), ("Twitch redeem (irc)", "Twitch redeem 1a2b3c4d..."));

        let sub = r"@msg-id=resub;display-name=Ann;msg-param-sub-plan=1000;room-id=1 :tmi.twitch.tv USERNOTICE #m :great stream";
        let (event, _, data) = parse_irc(sub, &no_badges()).unwrap();
        assert_eq!(event, "Twitch sub");
        assert_eq!(data["type"], "resub");
        assert_eq!(data["plan"], "1000");
        assert_eq!(data["message"], "great stream");

        let raid = r"@msg-id=raid;display-name=Raider;msg-param-login=raider;msg-param-viewerCount=42 :tmi.twitch.tv USERNOTICE #m";
        let (event, _, data) = parse_irc(raid, &no_badges()).unwrap();
        assert_eq!(event, "Twitch raid");
        assert_eq!(data["username"], "raider");
        assert_eq!(data["viewers"], "42");

        let timeout = r"@ban-duration=600;room-id=1 :tmi.twitch.tv CLEARCHAT #m :baduser";
        let (event, _, data) = parse_irc(timeout, &no_badges()).unwrap();
        assert_eq!(event, "Twitch timeout");
        assert_eq!(data["username"], "baduser");
        assert_eq!(data["duration"], "600");

        assert!(parse_irc(":tmi.twitch.tv 366 justinfan1 #m :End of /NAMES list", &no_badges()).is_none());
        assert!(parse_irc("@emote-only=0 :tmi.twitch.tv ROOMSTATE #m", &no_badges()).is_none());
        let (event, trigger, _) = parse_irc("@msg-id=x :tmi.twitch.tv NOTICE #m :hi", &no_badges()).unwrap();
        assert_eq!((event.as_str(), trigger.as_str()), ("Twitch other", "Twitch other (NOTICE)"));
    }

    #[test]
    fn shared_chat_is_marked() {
        let line = r"@display-name=A;room-id=1;source-room-id=2 :a!a@a PRIVMSG #m :hi";
        let (_, _, data) = parse_irc(line, &no_badges()).unwrap();
        assert_eq!(data["is_shared_chat"], true);
    }

    #[test]
    fn tags_unescape() {
        let line = r"@display-name=A;system-msg=hello\sthere\:\\ok;room-id=1 :a!a@a PRIVMSG #m :x";
        let tags = parse_tags(&line[1..line.find(' ').unwrap()]);
        assert_eq!(tags["system-msg"], r"hello there;\ok");
    }

    #[test]
    fn a_points_redeem_from_hermes() {
        let inner = json!({
            "type": "reward-redeemed",
            "data": { "redemption": {
                "user": { "id": "9", "login": "viewer", "display_name": "Viewer" },
                "reward": { "id": "r1", "title": "Hydrate", "cost": 100, "default_image": { "url_4x": "https://x/4.png" } },
                "user_input": "drink"
            } }
        });
        let frame = json!({
            "type": "notification",
            "notification": { "type": "pubsub", "pubsub": inner.to_string() }
        });
        let (event, trigger, data) = parse_hermes(&frame).unwrap();
        assert_eq!(event, "Twitch redeem (pubsub)");
        assert_eq!(trigger, "Twitch redeem Hydrate");
        assert_eq!(data["reward_title"], "Hydrate");
        assert_eq!(data["cost"], 100);
        assert_eq!(data["username"], "viewer");
        assert_eq!(data["display_name"], "Viewer");
        assert_eq!(data["message"], "drink");
        assert_eq!(data["image"], "https://x/4.png");
    }

    #[test]
    fn a_hype_train_from_hermes() {
        let inner = json!({ "type": "hype-train-progression", "data": { "progress": { "level": { "value": 3, "goal": 1000 }, "value": 250, "goal": 1000, "total": 2250 } } });
        let frame = json!({ "type": "notification", "notification": { "pubsub": inner.to_string() } });
        let (event, trigger, data) = parse_hermes(&frame).unwrap();
        assert_eq!(event, "Twitch hype train");
        assert_eq!(trigger, "Twitch hype train 3");
        assert_eq!(data["level"], 3);
        assert_eq!(data["progress"], 250);
        assert_eq!(data["goal"], 1000);
        assert!(parse_hermes(&json!({ "type": "welcome" })).is_none());
    }
}
