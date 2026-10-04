//! The messages the window shows (its console, its tester's 🎯 Catch next):
//! the reader's own while it reads (publish), else those of the reader on the
//! relay's port (the Theater Reader app), which it listens to.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, TrySendError};
use futures_util::StreamExt;
use parking_lot::Mutex;

/// Messages a listener hasn't taken yet, at most
const BACKLOG: usize = 256;

static SUBSCRIBERS: Mutex<Vec<Sender<Arc<str>>>> = Mutex::new(Vec::new());
static STARTED: AtomicBool = AtomicBool::new(false);
static CONNECTED: AtomicBool = AtomicBool::new(false);

/// Whether it's listening to another reader's relay.
pub fn connected() -> bool {
    CONNECTED.load(Ordering::Relaxed)
}

/// Every message from now on (its JSON text, as the relay sends it).
pub fn subscribe() -> Receiver<Arc<str>> {
    let (tx, rx) = crossbeam_channel::bounded(BACKLOG);
    SUBSCRIBERS.lock().push(tx);
    listen_once();
    rx
}

/// Hands `message` to every listener.
pub fn publish(message: &str) {
    let message: Arc<str> = Arc::from(message);
    SUBSCRIBERS.lock().retain(|tx| !matches!(tx.try_send(message.clone()), Err(TrySendError::Disconnected(_))));
}

/// While the reader is off: another reader's relay, on the port set.
fn listen_once() {
    if cfg!(test) || STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    crate::runtime().spawn(async move {
        let mut said_waiting = false;
        loop {
            // Reading itself: its messages come by publish(), and the relay is its own
            if crate::reader::running() {
                tokio::time::sleep(Duration::from_secs(3)).await;
                continue;
            }
            let uri = format!("ws://127.0.0.1:{}", crate::reader::config().relay_port);
            match tokio_tungstenite::connect_async(&uri).await {
                Ok((stream, _)) => {
                    crate::log(&format!("[Reader] 📡 Listening to the reader at {}", uri));
                    CONNECTED.store(true, Ordering::Relaxed);
                    said_waiting = false;
                    let (_, mut read) = stream.split();
                    while let Some(message) = read.next().await {
                        if crate::reader::running() {
                            break;
                        }
                        match message {
                            Ok(tokio_tungstenite::tungstenite::Message::Text(text)) => publish(text.as_str()),
                            Ok(tokio_tungstenite::tungstenite::Message::Close(_)) | Err(_) => break,
                            _ => {}
                        }
                    }
                    CONNECTED.store(false, Ordering::Relaxed);
                }
                Err(e) => {
                    if !said_waiting {
                        crate::log(&format!("[Reader] 📡 No reader at {} yet ({}): trying every few seconds", uri, e));
                        said_waiting = true;
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn published_messages_reach_every_listener() {
        let a = subscribe();
        let b = subscribe();
        publish(r#"{"type":"chat","message":"feed hi"}"#);
        assert!(a.try_iter().any(|m| m.contains("feed hi")));
        assert!(b.try_iter().any(|m| m.contains("feed hi")));
        drop(b);
        publish("second");
        assert!(a.try_iter().any(|m| &*m == "second"));
    }
}
