//! Against the real Twitch, Kick and YouTube (network): run by hand with
//! `cargo test --lib reader::live -- --ignored --nocapture`.

use std::time::Duration;

use super::*;

fn runtime() -> tokio::runtime::Runtime {
    let _ = rustls::crypto::ring::default_provider().install_default();
    tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap()
}

/// A sink of the current generation, and what reaches it
fn sink(chat: &str) -> (Sink, UnboundedReceiver<RawEvent>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    (Sink { tx, chat: chat.to_string(), generation: GENERATION.load(Ordering::SeqCst) }, rx)
}

async fn collect(rx: &mut UnboundedReceiver<RawEvent>, secs: u64) -> Vec<RawEvent> {
    let mut out = Vec::new();
    let end = tokio::time::Instant::now() + Duration::from_secs(secs);
    while let Ok(Some(e)) = tokio::time::timeout_at(end, rx.recv()).await {
        out.push(e);
    }
    out
}

#[test]
#[ignore]
fn twitch_live() {
    let rt = runtime();
    rt.block_on(async {
        twitch::load_badges().await;
        let channel = std::env::var("TWITCH_CHANNEL").unwrap_or_else(|_| "kaicenat".into());
        let (sink, mut rx) = sink("Twitch:live");
        let task = tokio::spawn(twitch::run(channel.clone(), sink));
        let events = collect(&mut rx, 25).await;
        task.abort();
        println!("{} event(s) from {}", events.len(), channel);
        for e in events.iter().take(8) {
            let options = normalize::Options { event_on: true, shared: SharedChat::default() };
            let avatar = chat_avatar(e).await;
            for m in normalize::messages(e, &options, "live", avatar.as_deref()) {
                let text = m.to_string();
                println!("{}", &text[..text.len().min(400)]);
            }
        }
        let status = status("Twitch:live").unwrap();
        println!("links: {:?}", status.links);
        assert_eq!(status.links.get("chat"), Some(&Link::Connected));
        assert_eq!(status.links.get("points"), Some(&Link::Connected));
    });
}

#[test]
#[ignore]
fn kick_live() {
    let rt = runtime();
    rt.block_on(async {
        let channel = std::env::var("KICK_CHANNEL").unwrap_or_else(|_| "xqc".into());
        let (sink, mut rx) = sink("Kick:live");
        let task = tokio::spawn(kick::run(channel.clone(), sink));
        let events = collect(&mut rx, 25).await;
        task.abort();
        println!("{} event(s) from {}; avatar {:?}", events.len(), channel, kick::channel_avatar(&channel));
        for e in events.iter().take(5) {
            let options = normalize::Options { event_on: true, shared: SharedChat::default() };
            for m in normalize::messages(e, &options, "live", kick::channel_avatar(&e.channel).as_deref()) {
                let text = m.to_string();
                println!("{}", &text[..text.len().min(400)]);
            }
        }
        println!("links: {:?}", status("Kick:live").map(|s| s.links));
        assert_eq!(status("Kick:live").unwrap().links.get("chat"), Some(&Link::Connected));
    });
}

#[test]
#[ignore]
fn youtube_live() {
    let rt = runtime();
    rt.block_on(async {
        let input = std::env::var("YOUTUBE_INPUT").unwrap_or_else(|_| "@LofiGirl".into());
        let (sink, mut rx) = sink("YouTube:live");
        let task = tokio::spawn(youtube::run(input.clone(), sink));
        let events = collect(&mut rx, 40).await;
        task.abort();
        println!("{} event(s) from {}; avatar {:?}", events.len(), input, youtube::channel_avatar(&input));
        for e in events.iter().take(5) {
            let options = normalize::Options { event_on: true, shared: SharedChat::default() };
            for m in normalize::messages(e, &options, "live", youtube::channel_avatar(&e.channel).as_deref()) {
                let text = m.to_string();
                println!("{}", &text[..text.len().min(400)]);
            }
        }
        println!("links: {:?}", status("YouTube:live").map(|s| s.links));
        assert_eq!(status("YouTube:live").unwrap().links.get("chat"), Some(&Link::Connected));
    });
}
