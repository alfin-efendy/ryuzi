//! Host adapter for `ryuzi:websocket/websocket@0.1.0`: a host-owned TLS
//! WebSocket a component drives via `connect`/`send`/`poll`/`state`/`close`.
//!
//! The host owns every raw socket; the component only ever sees an opaque
//! per-instance `u64` handle. This is the only outbound streaming-network
//! surface a component gets, and — like `ryuzi:http` — it is gated strictly:
//!
//! - **`wss` only.** The connect URL must be `wss://` (TLS). Plain `ws://` is
//!   rejected as `invalid-request` in production; a `ws://` loopback address is
//!   accepted ONLY under `#[cfg(test)]` (see [`is_test_loopback`]) so the local
//!   echo-server tests need no TLS certificate. The production path can never
//!   reach a plaintext socket.
//! - **Manifest allowlist.** The connect host must be covered by the bundle's
//!   `permissions.network` allowlist, checked with the SAME matcher
//!   `ryuzi:http` uses ([`super::http::host_is_allowed`]) — a wildcard or bare
//!   host, never re-implemented here. Not allowed → `rejected`. Because the
//!   component re-calls `connect` to reconnect, every reconnect is re-checked.
//! - **Per-instance caps.** At most [`MAX_WS_HANDLES_PER_INSTANCE`] concurrent
//!   sockets; a single frame is capped at [`MAX_WS_FRAME_BYTES`]; the inbound
//!   buffer holds at most [`MAX_WS_INBOUND_BUFFER`] frames (on overflow the
//!   connection is marked disconnected and further frames are dropped). Each
//!   cap breach surfaces as `limit-exceeded`, never a host crash.
//!
//! # Lifecycle
//! The registry lives on the component instance's `CapabilityState` (see
//! `runtime.rs`). Each [`WsConn`] owns its write half and a background reader
//! task; [`WsConn`]'s `Drop` aborts that task and drops the socket, so when the
//! instance (and thus its `CapabilityState`/`WsRegistry`) is dropped on a
//! supervisor stop/restart, no socket and no reader task outlives it.
//!
//! # Async-from-sync bridge
//! The generated `Host` trait is synchronous, but opening/sending on a socket
//! is async. Exactly like `capabilities::http`, the blocking work is bridged
//! through a captured `tokio::runtime::Handle` (`rt.block_on(...)`); the reader
//! task is spawned onto the same handle. The thin `impl websocket_iface::Host`
//! that maps WIT types to these calls lives in `runtime.rs` alongside the
//! `http`/`oauth` adapters.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::runtime::Handle;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderName, HeaderValue};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{Connector, MaybeTlsStream, WebSocketStream};

use super::http::host_is_allowed;

/// Maximum number of concurrent WebSocket handles a single component instance
/// may hold open at once. A `connect` past this cap fails with
/// `limit-exceeded` rather than opening another socket.
pub const MAX_WS_HANDLES_PER_INSTANCE: usize = 4;

/// Maximum size (bytes) of a single outbound frame's payload. A larger `send`
/// is rejected with `limit-exceeded` before anything is written to the wire.
pub const MAX_WS_FRAME_BYTES: usize = 1_048_576;

/// Maximum number of inbound frames buffered per handle between `poll`s. When
/// the reader task would exceed this, the connection is marked disconnected
/// and the overflowing (and any subsequent) frame is dropped — a slow guest
/// that never drains can never drive unbounded host memory growth.
pub const MAX_WS_INBOUND_BUFFER: usize = 1024;

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;
type WsSink = SplitSink<WsStream, Message>;
type WsSource = SplitStream<WsStream>;

/// An adapter-local error, mapped to the generated WIT `websocket::WsError` by
/// the runtime's `Host` trait impl. Kept independent of the generated bindings
/// so the registry logic and its tests do not depend on `wit_bindings`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsErr {
    /// Malformed URL, non-`wss` scheme, or an unknown/bad handle.
    InvalidRequest(String),
    /// The connect host is not in the bundle's network allowlist.
    Rejected,
    /// The connection dropped (peer closed, socket error, or a `send` on a
    /// closed socket).
    Disconnected,
    /// Too many handles, a frame over [`MAX_WS_FRAME_BYTES`], or the inbound
    /// buffer is full.
    LimitExceeded(String),
    /// Any other failure (handshake/TLS/IO error while opening).
    Failed(String),
}

/// A single WebSocket frame, adapter-local mirror of the WIT `ws-frame`. A
/// text frame carries UTF-8 bytes; a binary frame carries arbitrary bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WsFrame {
    pub data: Vec<u8>,
    pub is_text: bool,
}

/// A request header the component asks the host to set on the opening
/// handshake (adapter-local mirror of the WIT `ws-header`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WsHeader {
    pub name: String,
    pub value: String,
}

/// Adapter-local mirror of the WIT `ws-state`. `connect` blocks until the
/// handshake completes, so a live handle reports `Open`; a dropped peer/socket
/// reports `Closed`. (`Connecting`/`Closing` are part of the contract but the
/// current adapter transitions through them synchronously.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WsConnState {
    Connecting,
    Open,
    Closing,
    Closed,
}

/// The shared inbound buffer a connection's reader task fills and `poll`/
/// `state` drain. `disconnected` is set by the reader task when the peer
/// closes or the socket errors, and by [`Inbox::push`] on buffer overflow.
struct Inbox {
    frames: VecDeque<WsFrame>,
    disconnected: bool,
}

impl Inbox {
    fn new() -> Self {
        Self {
            frames: VecDeque::new(),
            disconnected: false,
        }
    }

    /// Buffers `frame`, returning `true` if it was accepted. On overflow past
    /// [`MAX_WS_INBOUND_BUFFER`] the buffer is left untouched, the connection
    /// is marked `disconnected`, and `false` is returned so the reader task
    /// stops (drop-and-disconnect overflow policy).
    fn push(&mut self, frame: WsFrame) -> bool {
        if self.frames.len() >= MAX_WS_INBOUND_BUFFER {
            self.disconnected = true;
            return false;
        }
        self.frames.push_back(frame);
        true
    }
}

/// One host-owned connection: the write half (driven synchronously via
/// `rt.block_on`), the background reader task, and the shared inbound buffer.
struct WsConn {
    write: WsSink,
    reader: tokio::task::JoinHandle<()>,
    inbox: Arc<Mutex<Inbox>>,
}

impl Drop for WsConn {
    fn drop(&mut self) {
        // Abort the reader task (dropping its read half) and let `write` drop
        // with `self` (dropping the write half); once both halves are gone the
        // underlying socket is closed. No socket or task outlives the handle.
        self.reader.abort();
    }
}

/// Per-instance registry of live WebSocket connections plus a monotonic handle
/// counter. Owned by `CapabilityState`; dropping it drops every [`WsConn`],
/// which aborts each reader task and closes each socket.
#[derive(Default)]
pub struct WsRegistry {
    conns: HashMap<u64, WsConn>,
    next_handle: u64,
}

impl WsRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open a TLS WebSocket to an allowlisted `wss://` `url`, returning an
    /// opaque handle. Enforces (in order) the per-instance handle cap, the
    /// `wss`-only scheme rule, and the network allowlist before any socket is
    /// opened. Spawns a bounded reader task that buffers inbound frames.
    ///
    /// Runs the async open on `rt` via `block_on`, so it MUST be called from a
    /// blocking (non-async) context — exactly how the host invokes it inside
    /// `spawn_blocking` (see the module doc).
    pub fn connect(
        &mut self,
        allowlist: &[String],
        rt: &Handle,
        url: &str,
        headers: Vec<WsHeader>,
    ) -> Result<u64, WsErr> {
        if self.conns.len() >= MAX_WS_HANDLES_PER_INSTANCE {
            return Err(WsErr::LimitExceeded(format!(
                "too many open websocket handles (max {MAX_WS_HANDLES_PER_INSTANCE})"
            )));
        }

        let parsed =
            url::Url::parse(url).map_err(|error| WsErr::InvalidRequest(error.to_string()))?;
        let host = parsed
            .host_str()
            .ok_or_else(|| WsErr::InvalidRequest("url has no host".to_string()))?
            .to_string();

        // Scheme gate FIRST (before the allowlist): production is `wss` only.
        // A `ws://` loopback address is allowed only under `#[cfg(test)]`.
        let scheme = parsed.scheme();
        let plaintext = match scheme {
            "wss" => false,
            "ws" if is_test_loopback(&host) => true,
            _ => {
                return Err(WsErr::InvalidRequest(format!(
                    "scheme `{scheme}` is not supported; websocket requires wss"
                )))
            }
        };

        // Same allowlist matcher as `ryuzi:http` — never re-implemented here.
        if !host_is_allowed(allowlist, &host) {
            return Err(WsErr::Rejected);
        }

        let inbox = Arc::new(Mutex::new(Inbox::new()));
        let (sink, source) = rt.block_on(open_stream(url, headers, plaintext))?;
        let reader = rt.spawn(reader_loop(source, inbox.clone()));

        let handle = self.next_handle;
        self.next_handle += 1;
        self.conns.insert(
            handle,
            WsConn {
                write: sink,
                reader,
                inbox,
            },
        );
        Ok(handle)
    }

    /// Send one frame on `handle`. Rejects an unknown handle
    /// (`invalid-request`), a payload over [`MAX_WS_FRAME_BYTES`]
    /// (`limit-exceeded`), or a send on an already-dropped connection
    /// (`disconnected`); a text frame whose bytes are not valid UTF-8 is
    /// `invalid-request`. A write error marks the connection disconnected.
    pub fn send(&mut self, rt: &Handle, handle: u64, frame: WsFrame) -> Result<(), WsErr> {
        let conn = self
            .conns
            .get_mut(&handle)
            .ok_or_else(|| WsErr::InvalidRequest("unknown websocket handle".to_string()))?;

        if frame.data.len() > MAX_WS_FRAME_BYTES {
            return Err(WsErr::LimitExceeded(format!(
                "frame of {} bytes exceeds the {MAX_WS_FRAME_BYTES}-byte limit",
                frame.data.len()
            )));
        }
        if conn
            .inbox
            .lock()
            .expect("ws inbox mutex poisoned")
            .disconnected
        {
            return Err(WsErr::Disconnected);
        }

        let message = if frame.is_text {
            let text = String::from_utf8(frame.data)
                .map_err(|_| WsErr::InvalidRequest("text frame is not valid utf-8".to_string()))?;
            Message::text(text)
        } else {
            Message::binary(frame.data)
        };

        match rt.block_on(conn.write.send(message)) {
            Ok(()) => Ok(()),
            Err(_) => {
                // A failed write means the socket is broken — surface it as a
                // disconnect so the guest reconnects, and mark it so.
                conn.inbox
                    .lock()
                    .expect("ws inbox mutex poisoned")
                    .disconnected = true;
                Err(WsErr::Disconnected)
            }
        }
    }

    /// Drain and return every inbound frame buffered since the last `poll`
    /// (order-preserving, non-blocking). Empty when idle. If the buffer is
    /// empty AND the connection has dropped, returns `disconnected` so the
    /// guest can detect the drop; buffered frames are always delivered first.
    /// An unknown handle is `invalid-request`.
    pub fn poll(&mut self, handle: u64) -> Result<Vec<WsFrame>, WsErr> {
        let conn = self
            .conns
            .get(&handle)
            .ok_or_else(|| WsErr::InvalidRequest("unknown websocket handle".to_string()))?;
        let mut inbox = conn.inbox.lock().expect("ws inbox mutex poisoned");
        let frames: Vec<WsFrame> = inbox.frames.drain(..).collect();
        if frames.is_empty() && inbox.disconnected {
            return Err(WsErr::Disconnected);
        }
        Ok(frames)
    }

    /// Report the connection state for `handle`: `closed` once the peer/socket
    /// has dropped, otherwise `open`. An unknown handle is `invalid-request`.
    pub fn state(&mut self, handle: u64) -> Result<WsConnState, WsErr> {
        let conn = self
            .conns
            .get(&handle)
            .ok_or_else(|| WsErr::InvalidRequest("unknown websocket handle".to_string()))?;
        if conn
            .inbox
            .lock()
            .expect("ws inbox mutex poisoned")
            .disconnected
        {
            Ok(WsConnState::Closed)
        } else {
            Ok(WsConnState::Open)
        }
    }

    /// Close the connection and release `handle`. Removing the [`WsConn`] runs
    /// its `Drop` (abort reader task + drop the socket). An unknown handle is
    /// `invalid-request`.
    pub fn close(&mut self, handle: u64) -> Result<(), WsErr> {
        match self.conns.remove(&handle) {
            Some(_conn) => Ok(()),
            None => Err(WsErr::InvalidRequest(
                "unknown websocket handle".to_string(),
            )),
        }
    }
}

/// Whether `host` is a loopback address a `ws://` (plaintext) connection is
/// allowed to reach. `true` ONLY under `#[cfg(test)]` — the production build
/// always returns `false`, so production can never open a plaintext socket.
fn is_test_loopback(host: &str) -> bool {
    #[cfg(test)]
    {
        matches!(host, "127.0.0.1" | "localhost" | "::1" | "[::1]")
    }
    #[cfg(not(test))]
    {
        let _ = host;
        false
    }
}

/// Open the WebSocket and split it into its write/read halves. A `wss` URL uses
/// an explicit ring-backed rustls connector (never the process-default
/// provider — the workspace is ring-only); a plaintext `ws` URL (test loopback
/// only) uses a plain connect.
async fn open_stream(
    url: &str,
    headers: Vec<WsHeader>,
    plaintext: bool,
) -> Result<(WsSink, WsSource), WsErr> {
    let mut request = url
        .into_client_request()
        .map_err(|error| WsErr::InvalidRequest(error.to_string()))?;
    {
        let request_headers = request.headers_mut();
        for WsHeader { name, value } in &headers {
            let header_name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|error| WsErr::InvalidRequest(error.to_string()))?;
            let header_value = HeaderValue::from_str(value)
                .map_err(|error| WsErr::InvalidRequest(error.to_string()))?;
            request_headers.append(header_name, header_value);
        }
    }

    let stream = if plaintext {
        tokio_tungstenite::connect_async(request)
            .await
            .map_err(|error| WsErr::Failed(error.to_string()))?
            .0
    } else {
        let connector = Connector::Rustls(Arc::new(rustls_client_config()));
        tokio_tungstenite::connect_async_tls_with_config(request, None, false, Some(connector))
            .await
            .map_err(|error| WsErr::Failed(error.to_string()))?
            .0
    };
    Ok(stream.split())
}

/// A rustls client config pinned to the ring crypto provider with the webpki
/// root store. Built explicitly (not via `ClientConfig::builder()`) so the
/// provider is never resolved from the ambiguous process default — the
/// workspace compiles rustls ring-only (no aws-lc-rs; see the Cargo notes).
fn rustls_client_config() -> rustls::ClientConfig {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .expect("the ring provider supports rustls' default protocol versions")
        .with_root_certificates(roots)
        .with_no_client_auth()
}

/// Background task: read frames off `source` into the shared `inbox` until the
/// peer closes, the socket errors, or the inbound buffer overflows. Control
/// frames (ping/pong/raw) are ignored; a close frame or stream end marks the
/// connection disconnected.
async fn reader_loop(mut source: WsSource, inbox: Arc<Mutex<Inbox>>) {
    loop {
        match source.next().await {
            Some(Ok(Message::Text(text))) => {
                let frame = WsFrame {
                    data: text.as_bytes().to_vec(),
                    is_text: true,
                };
                if !inbox.lock().expect("ws inbox mutex poisoned").push(frame) {
                    break;
                }
            }
            Some(Ok(Message::Binary(bytes))) => {
                let frame = WsFrame {
                    data: bytes.to_vec(),
                    is_text: false,
                };
                if !inbox.lock().expect("ws inbox mutex poisoned").push(frame) {
                    break;
                }
            }
            Some(Ok(Message::Close(_))) => {
                inbox.lock().expect("ws inbox mutex poisoned").disconnected = true;
                break;
            }
            // Ping/Pong/raw frames carry no application payload here.
            Some(Ok(_)) => {}
            Some(Err(_)) | None => {
                inbox.lock().expect("ws inbox mutex poisoned").disconnected = true;
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::net::TcpListener;

    /// Spawn a loopback `tokio-tungstenite` echo server, returning its bound
    /// port. Each accepted connection echoes text/binary frames back until the
    /// client closes. Mirrors the axum `spawn_server` pattern in `github_e2e`.
    async fn spawn_echo_server() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback listener should bind");
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let ws = match tokio_tungstenite::accept_async(stream).await {
                        Ok(ws) => ws,
                        Err(_) => return,
                    };
                    let (mut write, mut read) = ws.split();
                    while let Some(Ok(message)) = read.next().await {
                        if message.is_close() {
                            break;
                        }
                        if (message.is_text() || message.is_binary())
                            && write.send(message).await.is_err()
                        {
                            break;
                        }
                    }
                });
            }
        });
        port
    }

    /// Run `f` on a blocking thread with the current runtime handle — the
    /// registry's `block_on`-bearing methods must not be called from an async
    /// context, exactly as the host runs them inside `spawn_blocking`.
    async fn on_blocking<F, R>(f: F) -> R
    where
        F: FnOnce(Handle) -> R + Send + 'static,
        R: Send + 'static,
    {
        let rt = Handle::current();
        tokio::task::spawn_blocking(move || f(rt))
            .await
            .expect("blocking task should not panic")
    }

    /// Poll `handle` until at least one frame arrives, or panic after ~4s.
    fn poll_until_frame(reg: &mut WsRegistry, handle: u64) -> Vec<WsFrame> {
        for _ in 0..200 {
            let frames = reg.poll(handle).expect("poll on a live handle");
            if !frames.is_empty() {
                return frames;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("no frame arrived within the timeout");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn websocket_connect_send_poll_roundtrip_on_allowlisted_host() {
        let port = spawn_echo_server().await;
        on_blocking(move |rt| {
            let mut reg = WsRegistry::new();
            let url = format!("ws://127.0.0.1:{port}/");
            let handle = reg
                .connect(&["127.0.0.1".to_string()], &rt, &url, vec![])
                .expect("connect to an allowlisted host");
            reg.send(
                &rt,
                handle,
                WsFrame {
                    data: b"hello ryuzi".to_vec(),
                    is_text: true,
                },
            )
            .expect("send a text frame");

            let frames = poll_until_frame(&mut reg, handle);
            assert_eq!(frames.len(), 1);
            assert_eq!(frames[0].data, b"hello ryuzi");
            assert!(frames[0].is_text);
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn connect_to_a_non_allowlisted_host_is_rejected() {
        let port = spawn_echo_server().await;
        on_blocking(move |rt| {
            let mut reg = WsRegistry::new();
            let url = format!("ws://127.0.0.1:{port}/");
            // 127.0.0.1 is a valid test loopback scheme-wise, but it is NOT in
            // the allowlist, so the connect must be rejected.
            let err = reg
                .connect(&["example.com".to_string()], &rt, &url, vec![])
                .expect_err("a non-allowlisted host must be rejected");
            assert_eq!(err, WsErr::Rejected);
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn connect_with_a_non_loopback_ws_scheme_is_invalid_request() {
        on_blocking(|rt| {
            let mut reg = WsRegistry::new();
            // The host is allowlisted, but a non-loopback `ws://` (plaintext)
            // scheme is rejected as invalid-request before any connect — the
            // production path is strictly `wss`.
            let err = reg
                .connect(
                    &["example.com".to_string()],
                    &rt,
                    "ws://example.com/socket",
                    vec![],
                )
                .expect_err("a non-loopback ws:// must be invalid-request");
            assert!(matches!(err, WsErr::InvalidRequest(_)), "got {err:?}");
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_frame_larger_than_the_max_is_limit_exceeded() {
        let port = spawn_echo_server().await;
        on_blocking(move |rt| {
            let mut reg = WsRegistry::new();
            let url = format!("ws://127.0.0.1:{port}/");
            let handle = reg
                .connect(&["127.0.0.1".to_string()], &rt, &url, vec![])
                .expect("connect");
            let oversized = vec![b'x'; MAX_WS_FRAME_BYTES + 1];
            let err = reg
                .send(
                    &rt,
                    handle,
                    WsFrame {
                        data: oversized,
                        is_text: false,
                    },
                )
                .expect_err("an oversized frame must be rejected");
            assert!(matches!(err, WsErr::LimitExceeded(_)), "got {err:?}");
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn opening_a_handle_past_the_cap_is_limit_exceeded() {
        let port = spawn_echo_server().await;
        on_blocking(move |rt| {
            let mut reg = WsRegistry::new();
            let url = format!("ws://127.0.0.1:{port}/");
            let allow = vec!["127.0.0.1".to_string()];
            for _ in 0..MAX_WS_HANDLES_PER_INSTANCE {
                reg.connect(&allow, &rt, &url, vec![])
                    .expect("connect within the handle cap");
            }
            let err = reg
                .connect(&allow, &rt, &url, vec![])
                .expect_err("the handle past the cap must be rejected");
            assert!(matches!(err, WsErr::LimitExceeded(_)), "got {err:?}");
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_server_close_is_observable_via_state_and_poll() {
        // A server that accepts the handshake then immediately drops the
        // socket, so the client's reader observes the disconnect.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            if let Ok((stream, _)) = listener.accept().await {
                let _ws = tokio_tungstenite::accept_async(stream).await;
                // `_ws` drops here -> the socket closes.
            }
        });

        on_blocking(move |rt| {
            let mut reg = WsRegistry::new();
            let url = format!("ws://127.0.0.1:{port}/");
            let handle = reg
                .connect(&["127.0.0.1".to_string()], &rt, &url, vec![])
                .expect("connect");

            let mut closed = false;
            for _ in 0..200 {
                if reg.state(handle).expect("state") == WsConnState::Closed {
                    closed = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            assert!(closed, "state must become Closed after the server closes");

            // Once the buffer is drained and the socket is gone, poll surfaces
            // the disconnect.
            match reg.poll(handle) {
                Ok(frames) => assert!(frames.is_empty(), "no data frames expected"),
                Err(err) => assert_eq!(err, WsErr::Disconnected),
            }
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dropping_the_registry_closes_its_sockets() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            if let Ok((stream, _)) = listener.accept().await {
                if let Ok(ws) = tokio_tungstenite::accept_async(stream).await {
                    let (mut write, mut read) = ws.split();
                    // Serve until the client's socket goes away (stream ends).
                    while let Some(Ok(message)) = read.next().await {
                        if message.is_close() {
                            break;
                        }
                        let _ = write.send(message).await;
                    }
                    let _ = tx.send(());
                }
            }
        });

        let rt = Handle::current();
        let allow = vec!["127.0.0.1".to_string()];
        let url = format!("ws://127.0.0.1:{port}/");
        // Open a connection, then drop the whole registry on the blocking
        // thread — WsConn::Drop must abort the reader and close the socket.
        tokio::task::spawn_blocking(move || {
            let mut reg = WsRegistry::new();
            let _handle = reg.connect(&allow, &rt, &url, vec![]).expect("connect");
            // `reg` drops here.
        })
        .await
        .expect("blocking task");

        tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("the server must observe the dropped socket closing")
            .expect("the close signal channel should not close early");
    }

    #[test]
    fn inbox_push_overflow_marks_disconnected_and_drops() {
        let mut inbox = Inbox::new();
        for i in 0..MAX_WS_INBOUND_BUFFER {
            assert!(
                inbox.push(WsFrame {
                    data: vec![i as u8],
                    is_text: false,
                }),
                "push {i} within the cap must be accepted"
            );
        }
        assert_eq!(inbox.frames.len(), MAX_WS_INBOUND_BUFFER);
        assert!(!inbox.disconnected);

        // The overflowing push is rejected, marks disconnected, and is dropped.
        assert!(!inbox.push(WsFrame {
            data: vec![0],
            is_text: false,
        }));
        assert!(inbox.disconnected);
        assert_eq!(inbox.frames.len(), MAX_WS_INBOUND_BUFFER);
    }
}
