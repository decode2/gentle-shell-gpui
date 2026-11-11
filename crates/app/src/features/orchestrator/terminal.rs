//! Embedded terminal: the daemon's PTY websocket bridge
//! (`GET /sessions/{id}/terminal`), attached to the session worktree.

use std::sync::{
    Arc, Mutex,
    mpsc::{self, Receiver},
};
use std::time::Duration;

/// Events from the PTY side: output bytes to render, or the shell closing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalEvent {
    Output(Vec<u8>),
    Closed,
}

/// Connects to the session's embedded terminal through the daemon's
/// websocket. Keystrokes go back over the same socket; `events` yields
/// output bursts and the close signal.
pub struct TerminalHandle {
    socket: Arc<
        Mutex<tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>>,
    >,
    events: Receiver<TerminalEvent>,
}

impl TerminalHandle {
    /// Connects to the session's terminal and starts the reader thread.
    /// The socket is the write half; events is the read half.
    /// The returned handle owns the socket for writes.
    pub fn connect(session: &str) -> Result<Self, String> {
        let url = format!(
            "ws://127.0.0.1:8094/sessions/{}/terminal",
            percent_encode(session)
        );
        let (socket, _) =
            tungstenite::connect(&url).map_err(|e| format!("terminal ws connect failed: {e}"))?;
        let (tx, rx) = mpsc::channel();
        let socket = Arc::new(Mutex::new(socket));
        let handle = Self {
            socket: socket.clone(),
            events: rx,
        };
        std::thread::spawn(move || Self::socket_reader(socket, tx));
        Ok(handle)
    }

    /// Sends one keystroke (or paste chunk) to the shell.
    pub fn send(&mut self, data: &[u8]) -> Result<(), String> {
        self.socket
            .lock()
            .map_err(|e| format!("terminal socket poisoned: {e}"))?
            .send(tungstenite::Message::Binary(
                tungstenite::Bytes::copy_from_slice(data),
            ))
            .map_err(|e| format!("terminal write failed: {e}"))
    }

    /// The terminal's event stream (output bursts and the close signal).
    pub fn events(&self) -> &Receiver<TerminalEvent> {
        &self.events
    }

    fn socket_reader(
        socket: Arc<
            Mutex<tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>>,
        >,
        tx: mpsc::Sender<TerminalEvent>,
    ) {
        loop {
            let message = {
                let mut guard = match socket.lock() {
                    Ok(guard) => guard,
                    Err(_) => {
                        let _ = tx.send(TerminalEvent::Closed);
                        return;
                    }
                };
                guard.read()
            };
            match message {
                Ok(tungstenite::Message::Binary(bytes)) => {
                    let _ = tx.send(TerminalEvent::Output(bytes.to_vec()));
                }
                Ok(tungstenite::Message::Text(text)) => {
                    let _ = tx.send(TerminalEvent::Output(text.as_bytes().to_vec()));
                }
                Ok(tungstenite::Message::Close(_)) => {
                    let _ = tx.send(TerminalEvent::Closed);
                    return;
                }
                Ok(_) => {}
                Err(_) => {
                    let _ = tx.send(TerminalEvent::Closed);
                    return;
                }
            }
        }
    }
}

/// Percent-encodes a session id for the daemon's URL path.
fn percent_encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' || b == b'~' {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// No-op guard so clippy does not complain about the unused helper import.
#[allow(dead_code)]
const _: Duration = Duration::ZERO;
