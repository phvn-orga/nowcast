//! A local HTTP server for tests: it answers each request with what its
//! handler returns, and keeps every request, its head then its body, in
//! lowercase.

use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

type Answer = (u16, Vec<(&'static str, &'static str)>, String);

pub struct Server {
    port: u16,
    requests: Arc<Mutex<Vec<String>>>,
}

impl Server {
    pub async fn start(handler: impl Fn(&str) -> Answer + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests: Arc<Mutex<Vec<String>>> = Arc::default();
        let (kept, handler) = (requests.clone(), Arc::new(handler));
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let (kept, handler) = (kept.clone(), handler.clone());
                tokio::spawn(async move {
                    let mut head = Vec::new();
                    let mut byte = [0u8; 1];
                    while !head.ends_with(b"\r\n\r\n") {
                        match socket.read(&mut byte).await {
                            Ok(1) => head.push(byte[0]),
                            _ => return,
                        }
                    }
                    let mut request = String::from_utf8_lossy(&head).to_lowercase();
                    let mut body = vec![0u8; content_length(&request)];
                    if socket.read_exact(&mut body).await.is_err() {
                        return;
                    }
                    request.push_str(&String::from_utf8_lossy(&body).to_lowercase());
                    let (status, headers, body) = handler(&request);
                    kept.lock().unwrap().push(request);
                    let mut answer = format!(
                        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n",
                        body.len()
                    );
                    for (name, value) in headers {
                        answer.push_str(&format!("{name}: {value}\r\n"));
                    }
                    answer.push_str("\r\n");
                    answer.push_str(&body);
                    let _ = socket.write_all(answer.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        Self { port, requests }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    pub fn base(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

/// The length of the body a lowercased request head announces.
fn content_length(head: &str) -> usize {
    head.lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|n| n.trim().parse().ok())
        .unwrap_or(0)
}
