//! JSON-lines TCP server. Each connection reads one request per line and
//! writes one response per line. Requests are forwarded to the render thread
//! through a channel and answered when the session handles them.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{Receiver, Sender, channel};

pub struct Request {
    pub body: Value,
    pub reply: Sender<Value>,
}

/// Starts listening on `addr` (for example `127.0.0.1:7878`). Returns the
/// channel the render thread drains.
pub fn serve(addr: &str) -> std::io::Result<Receiver<Request>> {
    let listener = TcpListener::bind(addr)?;
    let (tx, rx) = channel();
    std::thread::Builder::new()
        .name("ultrafast-server".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let tx = tx.clone();
                std::thread::spawn(move || connection(stream, tx));
            }
        })?;
    Ok(rx)
}

fn connection(stream: TcpStream, tx: Sender<Request>) {
    let Ok(mut out) = stream.try_clone() else { return };
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { return };
        if line.trim().is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<Value>(&line) {
            Ok(body) => {
                let (rtx, rrx) = channel();
                if tx.send(Request { body, reply: rtx }).is_err() {
                    return;
                }
                rrx.recv()
                    .unwrap_or_else(|_| json!({ "ok": false, "error": "session closed" }))
            }
            Err(e) => json!({ "ok": false, "error": format!("invalid JSON: {e}") }),
        };
        if writeln!(out, "{resp}").is_err() {
            return;
        }
    }
}

/// Sends one request to a running session and returns its response.
pub fn send(addr: &str, request: &Value) -> std::io::Result<Value> {
    let mut stream = TcpStream::connect(addr)?;
    writeln!(stream, "{request}")?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    serde_json::from_str(&line).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}
