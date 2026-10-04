# Theater Reader

An OBS Studio plugin that reads **Twitch, Kick and YouTube** chat and alerts itself, without a separate app or browser, and hands them on the way the Theater Reader app did:

- **The relay:** a websocket on `ws://127.0.0.1:41837` that [Theater Scene](https://github.com/Mortygotvr/Theater-Scene) and other apps connect to.
- **For the Control Panel inside OBS:** the OBS signal `theater_reader_event`, and the procedures `theater_reader_state`, `theater_reader_seen` and `theater_reader_test`.

It's optional. Theater Scene works the same with the Theater Reader app, or anything else serving the relay. Turned off in its settings, it reads nothing and leaves the port free for another reader.

## Installing a release

Extract the zip into `C:\ProgramData\obs-studio\plugins`, so you get `plugins\theater-reader\bin` and `plugins\theater-reader\data`, then restart OBS (30 or newer).

## Using it

**🎭 Theater → 📡 Chat & Alerts…** opens its window:
- **⚙ Settings:** which chats to read, which of their events, shared chat, and the relay port. You can also import the Theater Reader app's `config.json`.
- **🧪 Tester:** sends a test alert to everything listening.
- **🖥 Console:** every message the relay carries.

Settings are saved in `%APPDATA%\obs-studio\plugin_config\theater-reader\`.

## Building (Windows)

You need Rust and OBS Studio 30 or newer.

1. Clone [Theater-Shared](https://github.com/Mortygotvr/Theater-Shared) next to this repo, so `../Theater-Shared` exists.
2. `cargo build --release`.
3. Close OBS, then run `.\deploy.ps1`. Its menu item uses Theater Scene's Qt shim (`trust_qt.dll`). By default it's taken from a Theater Scene build next to this repo; pass `-QtShim <path>` otherwise. Without the shim, the item goes in OBS's Tools menu.

## License

GPL-2.0-or-later (see [LICENSE](LICENSE)), like OBS Studio.
