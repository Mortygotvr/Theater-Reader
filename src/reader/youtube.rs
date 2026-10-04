//! YouTube: the live chat through YouTube's own web API (InnerTube), no key
//! of the user's, like the C# reference's YouTubeChatReceiver; what it says
//! is Theater Reader's (parsers/youtube.py, from pytchat).
//!
//! The input is a live video's link or id, or a channel (@handle, channel
//! link): its current live stream is found, and found again after it ends.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Duration;

use base64::Engine;
use parking_lot::Mutex;
use serde_json::{Map, Value, json};

use super::{Backoff, Link, Platform, Sink};

const BROWSER: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";
const CLIENT_VERSION: &str = "2.20240401.01.00";
const API_KEY: &str = "AIzaSyAO_FJ2SlqU8Q4STEHLGCilw_Y9_11qcW8";

/// Input → the channel's picture (the chat's avatar)
static AVATARS: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

pub fn channel_avatar(input: &str) -> Option<String> {
    AVATARS.lock().as_ref().and_then(|a| a.get(input).cloned())
}

/// Reads the live chat of `input` until stopped.
pub async fn run(input: String, sink: Sink) {
    let mut backoff = Backoff::new();
    loop {
        sink.link("chat", Link::Connecting);
        match resolve(&input).await {
            Ok(stream) => {
                backoff.reset();
                match poll(&input, stream, &sink).await {
                    Ok(()) => sink.link("chat", Link::Failed("the stream ended: looking for the next one".into())),
                    Err(e) => sink.link("chat", Link::Failed(e)),
                }
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
            Err(e) => {
                sink.link("chat", Link::Failed(format!("{} (again in {} s)", e, backoff.secs().max(30))));
                tokio::time::sleep(Duration::from_secs(backoff.secs().max(30))).await;
                backoff.wait().await;
            }
        }
    }
}

struct Stream {
    video_id: String,
    channel_id: Option<String>,
    api_key: String,
    client_version: String,
    continuation: String,
    /// The continuation replays recent messages first (skip them)
    replays: bool,
}

/// A video id in a link (or the id itself)
pub fn video_id(input: &str) -> Option<String> {
    let input = input.trim();
    let is_id = |s: &str| s.len() == 11 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if is_id(input) {
        return Some(input.to_string());
    }
    for marker in ["v=", "/live/", "/embed/", "youtu.be/", "/v/", "/shorts/"] {
        if let Some(i) = input.find(marker) {
            let id: String = input[i + marker.len()..].chars().take(11).collect();
            if is_id(&id) {
                return Some(id);
            }
        }
    }
    None
}

/// The text after `marker` up to the next quote
fn quoted_after<'a>(html: &'a str, marker: &str) -> Option<&'a str> {
    let start = html.find(marker)? + marker.len();
    let end = html[start..].find('"')?;
    Some(&html[start..start + end])
}

async fn get_page(url: &str) -> Result<String, String> {
    let resp = super::http()
        .get(url)
        .header("User-Agent", BROWSER)
        .header("Accept-Language", "en-US,en;q=0.9")
        // Past the cookie consent page
        .header("Cookie", "CONSENT=YES+cb; SOCS=CAI")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("YouTube said {}", resp.status()));
    }
    resp.text().await.map_err(|e| e.to_string())
}

/// The live video of a channel input, and its watch page
async fn live_page(input: &str) -> Result<(String, String), String> {
    if let Some(id) = video_id(input) {
        let html = get_page(&format!("https://www.youtube.com/watch?v={}", id)).await?;
        return Ok((id, html));
    }
    let base = if input.starts_with("http://") || input.starts_with("https://") {
        input.trim_end_matches('/').to_string()
    } else {
        format!("https://www.youtube.com/{}", input.trim_start_matches('/'))
    };
    let url = if base.ends_with("/live") { base } else { format!("{}/live", base) };
    let html = get_page(&url).await?;
    // Live, /live is the stream's watch page
    let id = quoted_after(&html, "<link rel=\"canonical\" href=\"https://www.youtube.com/watch?v=")
        .map(|s| s.chars().take(11).collect::<String>())
        .filter(|id| video_id(id).is_some())
        .ok_or("not live now")?;
    Ok((id, html))
}

async fn resolve(input: &str) -> Result<Stream, String> {
    let (video_id, html) = live_page(input).await?;
    if !html.contains("\"liveChatRenderer\"") {
        return Err("no live chat on this video (not live?)".into());
    }
    let api_key = quoted_after(&html, "\"INNERTUBE_API_KEY\":\"").unwrap_or(API_KEY).to_string();
    let client_version = quoted_after(&html, "\"INNERTUBE_CLIENT_VERSION\":\"").unwrap_or(CLIENT_VERSION).to_string();
    let channel_id = html
        .find("\"videoDetails\"")
        .and_then(|i| quoted_after(&html[i..], "\"channelId\":\""))
        .or_else(|| quoted_after(&html, "\"externalChannelId\":\""))
        .filter(|c| c.len() == 24)
        .map(str::to_string);
    if let Some(avatar) = html
        .find("\"videoOwnerRenderer\":{\"thumbnail\":{\"thumbnails\":[{\"url\":\"")
        .and_then(|i| quoted_after(&html[i..], "\"url\":\""))
    {
        let avatar = avatar.replace("\\u0026", "&");
        // The 88 px picture
        let avatar = match avatar.find("=s").and_then(|i| avatar[i + 2..].find('-').map(|j| (i, i + 2 + j))) {
            Some((a, b)) if avatar[a + 2..b].chars().all(|c| c.is_ascii_digit()) => format!("{}=s88{}", &avatar[..a], &avatar[b..]),
            _ => avatar,
        };
        AVATARS.lock().get_or_insert_with(HashMap::new).insert(input.to_string(), avatar);
    }
    // Every message ("Live chat", not "Top chat"): made like pytchat's
    let (continuation, replays) = match &channel_id {
        Some(channel) => (continuation_token(&video_id, channel, 3, super::unix_secs()), false),
        None => {
            let i = html.find("\"liveChatRenderer\"").unwrap_or(0);
            let token = quoted_after(&html[i..], "\"continuation\":\"").ok_or("no live chat token")?;
            (token.to_string(), true)
        }
    };
    Ok(Stream { video_id, channel_id, api_key, client_version, continuation, replays })
}

async fn poll(input: &str, mut stream: Stream, sink: &Sink) -> Result<(), String> {
    let mut seen: VecDeque<String> = VecDeque::new();
    let mut seen_set: HashSet<String> = HashSet::new();
    let mut connected = false;
    let mut failures = 0;
    loop {
        let url = format!("https://www.youtube.com/youtubei/v1/live_chat/get_live_chat?key={}&prettyPrint=false", stream.api_key);
        let body = json!({
            "context": { "client": { "clientName": "WEB", "clientVersion": stream.client_version, "hl": "en", "gl": "US" } },
            "continuation": stream.continuation,
        });
        let resp = super::http()
            .post(&url)
            .header("User-Agent", BROWSER)
            .header("Origin", "https://www.youtube.com")
            .header("Referer", format!("https://www.youtube.com/watch?v={}", stream.video_id))
            .header("X-YouTube-Client-Name", "1")
            .header("X-YouTube-Client-Version", stream.client_version.clone())
            .json(&body)
            .send()
            .await;
        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                failures += 1;
                if failures > 5 {
                    return Err(e.to_string());
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };
        if !resp.status().is_success() {
            // An old token: a new one
            failures += 1;
            match (&stream.channel_id, failures) {
                (Some(channel), 1..=3) => stream.continuation = continuation_token(&stream.video_id, channel, 1, super::unix_secs()),
                _ => return Err(format!("YouTube said {}", resp.status())),
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }
        failures = 0;
        let v: Value = resp.json().await.map_err(|e| e.to_string())?;
        let Some(live) = v.pointer("/continuationContents/liveChatContinuation") else { return Ok(()) };
        if !connected {
            sink.link("chat", Link::Connected);
            connected = true;
        }
        let events = parse_actions(live);
        if stream.replays {
            stream.replays = false;
        } else {
            for (id, event, trigger, data) in events {
                if !id.is_empty() {
                    if !seen_set.insert(id.clone()) {
                        continue;
                    }
                    seen.push_back(id);
                    if seen.len() > 2000 {
                        if let Some(old) = seen.pop_front() {
                            seen_set.remove(&old);
                        }
                    }
                }
                sink.send(Platform::YouTube, input, &event, trigger, data);
            }
        }
        let (next, wait) = next_continuation(live).ok_or("the chat ended")?;
        stream.continuation = next;
        tokio::time::sleep(Duration::from_millis(wait.clamp(1000, 10000))).await;
    }
}

/// The next token and how long to wait for it (ms)
pub fn next_continuation(live: &Value) -> Option<(String, u64)> {
    let first = live.get("continuations")?.as_array()?.first()?;
    for kind in ["invalidationContinuationData", "timedContinuationData", "reloadContinuationData"] {
        if let Some(c) = first.get(kind) {
            let token = c.get("continuation")?.as_str()?.to_string();
            let wait = c.get("timeoutMs").and_then(|t| t.as_u64()).unwrap_or(2000);
            return Some((token, wait));
        }
    }
    None
}

fn simple_text(v: &Value) -> Option<String> {
    if let Some(s) = v.get("simpleText").and_then(|s| s.as_str()) {
        return Some(s.to_string());
    }
    let runs = v.get("runs")?.as_array()?;
    Some(runs.iter().filter_map(|r| r.get("text").and_then(|t| t.as_str())).collect())
}

/// Message runs as text, emojis as [yt_emoji:URL] (pytchat's messageEx)
fn runs_text(v: Option<&Value>) -> String {
    let Some(runs) = v.and_then(|m| m.get("runs")).and_then(|r| r.as_array()) else {
        return v.and_then(simple_text).unwrap_or_default();
    };
    let mut out = String::new();
    for run in runs {
        if let Some(text) = run.get("text").and_then(|t| t.as_str()) {
            out.push_str(text);
        } else if let Some(emoji) = run.get("emoji") {
            let url = emoji.pointer("/image/thumbnails").and_then(|t| t.as_array()).and_then(|t| t.last()).and_then(|t| t.get("url")).and_then(|u| u.as_str());
            match url {
                Some(url) if !url.ends_with(".svg") => out.push_str(&format!("[yt_emoji:{}]", url)),
                _ => {
                    let custom = emoji.get("isCustomEmoji").and_then(|c| c.as_bool()).unwrap_or(false);
                    let text = if custom { None } else { emoji.get("emojiId").and_then(|e| e.as_str()) };
                    let text = text.or_else(|| emoji.pointer("/shortcuts/0").and_then(|s| s.as_str())).unwrap_or("");
                    out.push_str(text);
                }
            }
        }
    }
    out
}

fn photo(r: &Value) -> String {
    r.pointer("/authorPhoto/thumbnails")
        .and_then(|t| t.as_array())
        .and_then(|t| t.last())
        .and_then(|t| t.get("url"))
        .and_then(|u| u.as_str())
        .unwrap_or("")
        .to_string()
}

/// The chat's new messages: (id, event, trigger, data), as Theater Reader's
pub fn parse_actions(live: &Value) -> Vec<(String, String, String, Map<String, Value>)> {
    let mut out = Vec::new();
    for action in live.get("actions").and_then(|a| a.as_array()).into_iter().flatten() {
        let Some(item) = action.pointer("/addChatItemAction/item") else { continue };
        let Some((kind, r)) = item.as_object().and_then(|o| o.iter().next()) else { continue };
        let id = r.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
        let author = r.get("authorName").and_then(simple_text).unwrap_or_default();
        let entry = match kind.as_str() {
            "liveChatTextMessageRenderer" => {
                let (mut member_badge, mut owner, mut moderator, mut verified, mut member) = (String::new(), false, false, false, false);
                for b in r.get("authorBadges").and_then(|b| b.as_array()).into_iter().flatten() {
                    let b = &b["liveChatAuthorBadgeRenderer"];
                    if let Some(url) = b.pointer("/customThumbnail/thumbnails/0/url").and_then(|u| u.as_str()) {
                        member = true;
                        member_badge = url.to_string();
                    }
                    match b.pointer("/icon/iconType").and_then(|i| i.as_str()) {
                        Some("OWNER") => owner = true,
                        Some("MODERATOR") => moderator = true,
                        Some("VERIFIED") => verified = true,
                        _ => {}
                    }
                }
                let data = json!({
                    "username": author,
                    "author": author,
                    "user_id": r.get("authorExternalChannelId").and_then(|c| c.as_str()).unwrap_or(""),
                    "message": runs_text(r.get("message")),
                    "badges": member_badge,
                    "image": photo(r),
                    "id": id,
                    "subscriber": member,
                    "mod": owner || moderator,
                    "vip": verified,
                });
                ("chat_message", "YouTube Chat".to_string(), data)
            }
            "liveChatPaidMessageRenderer" => {
                let amount = r.get("purchaseAmountText").and_then(simple_text).unwrap_or_default();
                let data = json!({ "author": author, "amount": amount, "message": runs_text(r.get("message")), "image": photo(r) });
                ("paid_message", format!("Super Chat: {}", amount), data)
            }
            "liveChatPaidStickerRenderer" => {
                let amount = r.get("purchaseAmountText").and_then(simple_text).unwrap_or_default();
                let data = json!({ "author": author, "amount": amount, "message": "", "image": photo(r) });
                ("sticker_message", format!("Super Sticker: {}", amount), data)
            }
            "liveChatMembershipItemRenderer" => ("member_message", "New Member!".to_string(), json!({ "author": author })),
            _ => continue,
        };
        let (event, trigger, data) = entry;
        out.push((id, event.to_string(), trigger, data.as_object().cloned().unwrap_or_default()));
    }
    out
}

// ---------------------------------------------------------------------------
// The live chat token (pytchat's paramgen, as the C# reference has it)

fn varint(mut n: u64, out: &mut Vec<u8>) {
    while n >> 7 != 0 {
        out.push((n & 0x7f) as u8 | 0x80);
        n >>= 7;
    }
    out.push(n as u8);
}

fn number(field: u64, n: u64) -> Vec<u8> {
    let mut out = Vec::new();
    varint(field << 3, &mut out);
    varint(n, &mut out);
    out
}

fn bytes(field: u64, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    varint((field << 3) | 2, &mut out);
    varint(data.len() as u64, &mut out);
    out.extend_from_slice(data);
    out
}

fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.concat()
}

/// The token's header: which video, whose
fn token_header(video_id: &str, channel_id: &str) -> String {
    let header = cat(&[
        bytes(1, &cat(&[bytes(3, &bytes(1, video_id.as_bytes())), bytes(5, &cat(&[bytes(1, channel_id.as_bytes()), bytes(2, video_id.as_bytes())]))])),
        bytes(3, &bytes(48687757, &bytes(1, video_id.as_bytes()))),
        number(4, 1),
    ]);
    base64::engine::general_purpose::URL_SAFE.encode(header)
}

/// A token for every message of the stream from `past_secs` ago
pub fn continuation_token(video_id: &str, channel_id: &str, past_secs: u64, now: u64) -> String {
    let now = now as f64;
    let rand = |lo: f64, hi: f64| lo + (super::random_u64() % 1_000_000) as f64 / 1_000_000.0 * (hi - lo);
    let micros = |t: f64| (t * 1_000_000.0) as u64;
    let ts1 = micros(now - rand(0.0, 3.0));
    let ts2 = micros(now - rand(0.01, 0.99));
    let ts3 = micros(now - past_secs as f64 + rand(0.0, 1.0));
    let ts4 = micros(now - rand(600.0, 3600.0));
    let ts5 = micros(now - rand(0.01, 0.99));
    let chat_type = 1; // every message (4: top chat)

    let header_b64 = token_header(video_id, channel_id);
    let body = cat(&[
        number(1, 0),
        number(2, 0),
        number(3, 0),
        number(4, 0),
        bytes(7, &[]),
        number(8, 0),
        bytes(9, &[]),
        number(10, ts2),
        number(11, 3),
        number(15, 0),
    ]);
    let entity = cat(&[
        bytes(3, header_b64.as_bytes()),
        number(5, ts1),
        number(6, 0),
        number(7, 0),
        number(8, 1),
        bytes(9, &body),
        number(10, ts3),
        number(11, ts4),
        number(13, chat_type),
        bytes(16, &number(1, chat_type)),
        number(17, 0),
        bytes(19, &number(1, 0)),
        number(20, ts5),
    ]);
    let token = bytes(119693434, &entity);
    base64::engine::general_purpose::URL_SAFE.encode(token).replace('=', "%3D")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_ids_from_links() {
        assert_eq!(video_id("dQw4w9WgXcQ").as_deref(), Some("dQw4w9WgXcQ"));
        assert_eq!(video_id("https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=1").as_deref(), Some("dQw4w9WgXcQ"));
        assert_eq!(video_id("https://youtu.be/dQw4w9WgXcQ").as_deref(), Some("dQw4w9WgXcQ"));
        assert_eq!(video_id("https://www.youtube.com/live/dQw4w9WgXcQ?si=x").as_deref(), Some("dQw4w9WgXcQ"));
        assert_eq!(video_id("@mortygotvr"), None);
        assert_eq!(video_id("https://www.youtube.com/@mortygotvr"), None);
    }

    #[test]
    fn chat_super_chats_stickers_and_members() {
        let live = json!({ "actions": [
            { "addChatItemAction": { "item": { "liveChatTextMessageRenderer": {
                "id": "m1",
                "message": { "runs": [ { "text": "hi " }, { "emoji": { "emojiId": "UCx/abc", "isCustomEmoji": true, "shortcuts": [":wave:"], "image": { "thumbnails": [ { "url": "https://yt3.ggpht.com/e=w24" }, { "url": "https://yt3.ggpht.com/e=w48" } ] } } } ] },
                "authorName": { "simpleText": "Ann" },
                "authorPhoto": { "thumbnails": [ { "url": "https://yt4.ggpht.com/a=s32" }, { "url": "https://yt4.ggpht.com/a=s64" } ] },
                "authorExternalChannelId": "UC123",
                "authorBadges": [
                    { "liveChatAuthorBadgeRenderer": { "icon": { "iconType": "MODERATOR" } } },
                    { "liveChatAuthorBadgeRenderer": { "customThumbnail": { "thumbnails": [ { "url": "https://yt3.ggpht.com/badge" } ] } } }
                ]
            } } } },
            { "addChatItemAction": { "item": { "liveChatPaidMessageRenderer": {
                "id": "m2", "authorName": { "simpleText": "Bob" }, "purchaseAmountText": { "simpleText": "$5.00" },
                "message": { "runs": [ { "text": "thanks" } ] }
            } } } },
            { "addChatItemAction": { "item": { "liveChatPaidStickerRenderer": {
                "id": "m3", "authorName": { "simpleText": "Cy" }, "purchaseAmountText": { "simpleText": "CA$2.00" }
            } } } },
            { "addChatItemAction": { "item": { "liveChatMembershipItemRenderer": {
                "id": "m4", "authorName": { "simpleText": "Di" }, "headerSubtext": { "runs": [ { "text": "Welcome" } ] }
            } } } },
            { "addLiveChatTickerItemAction": { "item": {} } }
        ] });
        let out = parse_actions(&live);
        assert_eq!(out.len(), 4);
        let (id, event, trigger, data) = &out[0];
        assert_eq!((id.as_str(), event.as_str(), trigger.as_str()), ("m1", "chat_message", "YouTube Chat"));
        assert_eq!(data["message"], "hi [yt_emoji:https://yt3.ggpht.com/e=w48]");
        assert_eq!(data["username"], "Ann");
        assert_eq!(data["image"], "https://yt4.ggpht.com/a=s64");
        assert_eq!(data["badges"], "https://yt3.ggpht.com/badge");
        assert_eq!(data["subscriber"], true);
        assert_eq!(data["mod"], true);
        assert_eq!(data["vip"], false);
        assert_eq!((out[1].1.as_str(), out[1].2.as_str()), ("paid_message", "Super Chat: $5.00"));
        assert_eq!(out[1].3["message"], "thanks");
        assert_eq!((out[2].1.as_str(), out[2].2.as_str()), ("sticker_message", "Super Sticker: CA$2.00"));
        assert_eq!((out[3].1.as_str(), out[3].2.as_str()), ("member_message", "New Member!"));
        assert_eq!(out[3].3["author"], "Di");
    }

    #[test]
    fn the_next_token() {
        let live = json!({ "continuations": [ { "invalidationContinuationData": { "continuation": "abc", "timeoutMs": 5000 } } ] });
        assert_eq!(next_continuation(&live), Some(("abc".to_string(), 5000)));
        let live = json!({ "continuations": [ { "timedContinuationData": { "continuation": "t" } } ] });
        assert_eq!(next_continuation(&live), Some(("t".to_string(), 2000)));
        assert_eq!(next_continuation(&json!({})), None);
    }

    #[test]
    fn a_token_is_url_safe_base64_of_the_video() {
        let token = continuation_token("dQw4w9WgXcQ", "UCuAXFkgsw1L7xaCfnd5JJOw", 3, 1_700_000_000);
        assert!(token.chars().all(|c| c.is_ascii_alphanumeric() || "-_%".contains(c)), "{token}");
        let raw = base64::engine::general_purpose::URL_SAFE.decode(token.replace("%3D", "=")).unwrap();
        // Field 119693434, length-delimited
        let mut tag = Vec::new();
        varint((119693434 << 3) | 2, &mut tag);
        assert!(raw.starts_with(&tag));
        // The header (base64 inside) names the video and the channel
        let header = token_header("dQw4w9WgXcQ", "UCuAXFkgsw1L7xaCfnd5JJOw");
        assert!(raw.windows(header.len()).any(|w| w == header.as_bytes()));
        let decoded = base64::engine::general_purpose::URL_SAFE.decode(&header).unwrap();
        let decoded = String::from_utf8_lossy(&decoded);
        assert!(decoded.contains("dQw4w9WgXcQ") && decoded.contains("UCuAXFkgsw1L7xaCfnd5JJOw"));
    }
}
