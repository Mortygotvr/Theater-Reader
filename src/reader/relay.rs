//! The relay: a websocket on port 41837, where Theater Reader's was, for
//! other apps (Theater Scene, overlays, bots, a Control Panel outside OBS).
//! Every message the reader sends reaches each app connected; what an app
//! sends reaches the others. Theater Reader's alert tester
//! (test_sammi_trigger) works as it did.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;
use tokio_tungstenite::tungstenite::Message;

use crate::log as trust_log;

static CLIENTS: Mutex<Vec<(u64, UnboundedSender<Message>)>> = Mutex::new(Vec::new());
static CLIENT_TASKS: Mutex<Vec<tokio::task::AbortHandle>> = Mutex::new(Vec::new());
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static HOSTING: AtomicBool = AtomicBool::new(false);
static PROBLEM: Mutex<Option<String>> = Mutex::new(None);

/// The relay is up.
pub fn hosting() -> bool {
    HOSTING.load(Ordering::SeqCst)
}

/// Why it isn't (the port is taken…)
pub fn problem() -> Option<String> {
    PROBLEM.lock().clone()
}

/// How many apps are connected.
pub fn clients() -> usize {
    CLIENTS.lock().len()
}

/// To every app connected.
pub fn broadcast(text: &str) {
    send_all(text, None);
}

fn send_all(text: &str, except: Option<u64>) {
    let message = Message::text(text.to_string());
    CLIENTS.lock().retain(|(id, tx)| Some(*id) == except || tx.send(message.clone()).is_ok());
}

/// The reader stopped (its tasks were aborted): the apps are let go.
pub fn stopped() {
    for task in CLIENT_TASKS.lock().drain(..) {
        task.abort();
    }
    CLIENTS.lock().clear();
    HOSTING.store(false, Ordering::SeqCst);
    *PROBLEM.lock() = None;
}

/// Serves on `port` (this computer only, or the network too) until aborted.
pub async fn serve(port: u16, lan: bool) {
    let host = if lan { "0.0.0.0" } else { "127.0.0.1" };
    let listener = loop {
        match tokio::net::TcpListener::bind((host, port)).await {
            Ok(l) => break l,
            Err(e) => {
                let why = if e.kind() == std::io::ErrorKind::AddrInUse {
                    format!("port {} is taken (is Theater Reader still running? close it)", port)
                } else {
                    format!("port {}: {}", port, e)
                };
                if PROBLEM.lock().as_deref() != Some(why.as_str()) {
                    trust_log(&format!("[Reader] ⚠️ The relay can't start: {}; trying again every 10 s", why));
                    crate::console::system(&format!("⚠ The relay can't start: {}", why));
                    *PROBLEM.lock() = Some(why);
                    crate::window::repaint();
                }
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
        }
    };
    HOSTING.store(true, Ordering::SeqCst);
    *PROBLEM.lock() = None;
    crate::window::repaint();
    trust_log(&format!("[Reader] 📡 Relay on ws://{}:{}", host, port));
    crate::console::system(&format!("📡 Relay on ws://{}:{}", host, port));
    loop {
        let Ok((stream, _)) = listener.accept().await else { continue };
        let task = tokio::spawn(client(stream));
        CLIENT_TASKS.lock().push(task.abort_handle());
        CLIENT_TASKS.lock().retain(|t| !t.is_finished());
    }
}

async fn client(stream: tokio::net::TcpStream) {
    let Ok(ws) = tokio_tungstenite::accept_async(stream).await else { return };
    let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
    let (mut write, mut read) = ws.split();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Message>();
    CLIENTS.lock().push((id, tx));
    crate::console::system(&format!("An app connected to the relay ({} now)", clients()));
    let writing = async {
        while let Some(message) = rx.recv().await {
            if write.send(message).await.is_err() {
                break;
            }
        }
    };
    let reading = async {
        while let Some(Ok(message)) = read.next().await {
            match message {
                Message::Text(text) => from_app(id, text.as_str()),
                Message::Close(_) => break,
                _ => {}
            }
        }
    };
    tokio::select! {
        _ = writing => {}
        _ = reading => {}
    }
    CLIENTS.lock().retain(|(i, _)| *i != id);
    crate::console::system(&format!("An app left the relay ({} now)", clients()));
    crate::window::repaint();
}

/// What an app sent
fn from_app(id: u64, text: &str) {
    let Ok(v) = serde_json::from_str::<Value>(text) else { return };
    let kind = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
    let s = |key: &str| v.get(key).and_then(|x| x.as_str()).map(str::to_string);
    match kind {
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
        // The rest (Theater Scene's tests, test_trigger and test_object,
        // among them) reaches every other app, Theater Scene included
        _ => send_all(text, Some(id)),
    }
}
