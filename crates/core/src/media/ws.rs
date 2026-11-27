//! Engine 4: pure async RFC 6455 WebSocket client over `tokio-rustls` (`ring` + `webpki-roots`)
//! and loopback `TcpStream`, supporting full-duplex `WsWriter`/`WsReader`, auto-Pong,
//! fragmented frames, and clean Close shutdown.

use super::{MediaError, MediaErrorCode, crypto, is_loopback_host};
use crate::redaction::CredentialSet;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use bytes::{Buf, Bytes, BytesMut};
use serde::Serialize;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};
use tokio_util::sync::CancellationToken;

const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
const MAX_HANDSHAKE_BYTES: usize = 64 * 1024;
const DEFAULT_MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
const CHANNEL_CAPACITY: usize = 256;

const OP_CONTINUATION: u8 = 0x0;
const OP_TEXT: u8 = 0x1;
const OP_BINARY: u8 = 0x2;
const OP_CLOSE: u8 = 0x8;
const OP_PING: u8 = 0x9;
const OP_PONG: u8 = 0xA;

#[derive(Clone, Debug)]
pub struct WsConnectConfig {
    pub connect_timeout: Duration,
    pub handshake_timeout: Duration,
    pub max_message_bytes: usize,
    pub allow_insecure_ws: bool,
    pub headers: Vec<(String, String)>,
    pub sensitive_headers: Vec<String>,
}

impl Default for WsConnectConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(15),
            handshake_timeout: Duration::from_secs(15),
            max_message_bytes: DEFAULT_MAX_MESSAGE_BYTES,
            allow_insecure_ws: false,
            headers: Vec::new(),
            sensitive_headers: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WsCloseFrame {
    pub code: u16,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WsMessage {
    Text(String),
    Binary(Bytes),
    Pong(Bytes),
    Close(Option<WsCloseFrame>),
}

#[derive(Clone, Debug)]
enum WsOutbound {
    Text(String),
    Binary(Bytes),
    Ping(Bytes),
    Pong(Bytes),
    Close(Option<WsCloseFrame>),
}

#[derive(Clone)]
pub struct WsWriter {
    tx: mpsc::Sender<WsOutbound>,
    close_sent: Arc<AtomicBool>,
}

impl WsWriter {
    pub async fn send_text(&self, text: impl Into<String>) -> Result<(), MediaError> {
        self.tx
            .send(WsOutbound::Text(text.into()))
            .await
            .map_err(|_| MediaError::new(MediaErrorCode::Transport, "websocket writer closed"))
    }

    pub async fn send_json<T: Serialize>(&self, value: &T) -> Result<(), MediaError> {
        let json = serde_json::to_string(value).map_err(|e| {
            MediaError::invalid_request(format!("failed to serialize WS JSON: {e}"))
        })?;
        self.send_text(json).await
    }

    pub async fn send_binary(&self, data: impl Into<Bytes>) -> Result<(), MediaError> {
        self.tx
            .send(WsOutbound::Binary(data.into()))
            .await
            .map_err(|_| MediaError::new(MediaErrorCode::Transport, "websocket writer closed"))
    }

    pub async fn send_ping(&self, data: impl Into<Bytes>) -> Result<(), MediaError> {
        let data = data.into();
        if data.len() > 125 {
            return Err(MediaError::invalid_request(
                "websocket ping payload must be <= 125 bytes",
            ));
        }
        self.tx
            .send(WsOutbound::Ping(data))
            .await
            .map_err(|_| MediaError::new(MediaErrorCode::Transport, "websocket writer closed"))
    }

    pub async fn close(&self, code: u16, reason: impl Into<String>) -> Result<(), MediaError> {
        let reason = reason.into();
        if reason.len() > 123 {
            return Err(MediaError::invalid_request(
                "websocket close reason must be <= 123 bytes",
            ));
        }
        if self.close_sent.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let _ = self
            .tx
            .send(WsOutbound::Close(Some(WsCloseFrame { code, reason })))
            .await;
        Ok(())
    }
}

pub struct WsReader {
    rx: mpsc::Receiver<Result<WsMessage, MediaError>>,
    shutdown: CancellationToken,
}

impl WsReader {
    pub async fn recv(&mut self) -> Option<Result<WsMessage, MediaError>> {
        self.rx.recv().await
    }
}

impl Drop for WsReader {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

enum WsTransport {
    Plain(TcpStream),
    Tls(Box<tokio_rustls::client::TlsStream<TcpStream>>),
}

impl AsyncRead for WsTransport {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_read(cx, buf),
            Self::Tls(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for WsTransport {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_write(cx, buf),
            Self::Tls(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_flush(cx),
            Self::Tls(s) => Pin::new(s.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_shutdown(cx),
            Self::Tls(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }
}

pub async fn connect_ws(
    url_str: &str,
    config: WsConnectConfig,
    keys: &CredentialSet,
    cancel: &CancellationToken,
) -> Result<(WsWriter, WsReader), MediaError> {
    let mut all_secrets: Vec<String> = Vec::new();
    for (name, val) in &config.headers {
        if config
            .sensitive_headers
            .iter()
            .any(|s| s.eq_ignore_ascii_case(name))
        {
            all_secrets.push(val.clone());
            if let Some(tok) = val.strip_prefix("Bearer ") {
                all_secrets.push(tok.to_owned());
            } else if let Some(tok) = val.strip_prefix("Bearer;") {
                all_secrets.push(tok.to_owned());
            }
        }
    }
    let combined_keys = keys.merged_with(all_secrets);

    connect_ws_inner(url_str, config, cancel)
        .await
        .map_err(|e| e.scrubbed(&combined_keys))
}

async fn connect_ws_inner(
    url_str: &str,
    config: WsConnectConfig,
    cancel: &CancellationToken,
) -> Result<(WsWriter, WsReader), MediaError> {
    let url = url::Url::parse(url_str.trim())
        .map_err(|_| MediaError::invalid_request("invalid websocket URL"))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(MediaError::invalid_request(
            "websocket URL must not contain credentials",
        ));
    }
    let is_tls = match url.scheme() {
        "wss" => true,
        "ws" => {
            if !config.allow_insecure_ws && !is_loopback_host(&url) {
                return Err(MediaError::invalid_request(
                    "insecure ws:// is only allowed for loopback hosts",
                ));
            }
            false
        }
        _ => {
            return Err(MediaError::invalid_request(
                "websocket URL scheme must be ws or wss",
            ));
        }
    };
    let host = url
        .host_str()
        .ok_or_else(|| MediaError::invalid_request("websocket URL missing host"))?
        .to_owned();
    let port = url
        .port_or_known_default()
        .ok_or_else(|| MediaError::invalid_request("websocket URL missing port"))?;

    validate_custom_headers(&config.headers)?;

    let connect_addr = format!("{host}:{port}");
    let tcp = tokio::select! {
        _ = cancel.cancelled() => return Err(MediaError::new(MediaErrorCode::Cancelled, "websocket connect cancelled")),
        res = tokio::time::timeout(config.connect_timeout, TcpStream::connect(&connect_addr)) => {
            res.map_err(|_| MediaError::new(MediaErrorCode::Timeout, "websocket TCP connect timed out"))?
                .map_err(|e| MediaError::new(MediaErrorCode::Transport, format!("websocket TCP connect failed: {e}")))?
        }
    };
    let _ = tcp.set_nodelay(true);

    let mut transport = if is_tls {
        let root_store = RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        let tls_config =
            ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .map_err(|e| MediaError::new(MediaErrorCode::Transport, e.to_string()))?
                .with_root_certificates(root_store)
                .with_no_client_auth();
        let connector = TlsConnector::from(Arc::new(tls_config));
        let clean_host = host
            .strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
            .unwrap_or(&host)
            .to_owned();
        let server_name = if let Ok(ip) = clean_host.parse::<IpAddr>() {
            ServerName::IpAddress(ip.into())
        } else {
            ServerName::try_from(clean_host)
                .map_err(|_| MediaError::invalid_request("invalid TLS server name"))?
        };
        let tls_stream = tokio::select! {
            _ = cancel.cancelled() => return Err(MediaError::new(MediaErrorCode::Cancelled, "TLS handshake cancelled")),
            res = tokio::time::timeout(config.handshake_timeout, connector.connect(server_name, tcp)) => {
                res.map_err(|_| MediaError::new(MediaErrorCode::Timeout, "TLS handshake timed out"))?
                    .map_err(|e| MediaError::new(MediaErrorCode::Transport, format!("TLS handshake failed: {e}")))?
            }
        };
        WsTransport::Tls(Box::new(tls_stream))
    } else {
        WsTransport::Plain(tcp)
    };

    // Perform RFC 6455 HTTP/1.1 Upgrade handshake and retain post-header bytes.
    let trailing_bytes =
        perform_upgrade_handshake(&mut transport, &url, &host, port, &config, cancel).await?;

    let (read_half, write_half) = tokio::io::split(transport);
    let (outbound_tx, outbound_rx) = mpsc::channel::<WsOutbound>(CHANNEL_CAPACITY);
    let (inbound_tx, inbound_rx) = mpsc::channel::<Result<WsMessage, MediaError>>(CHANNEL_CAPACITY);
    let shutdown = cancel.child_token();
    let close_sent = Arc::new(AtomicBool::new(false));

    tokio::spawn(ws_writer_loop(write_half, outbound_rx, shutdown.clone()));
    tokio::spawn(ws_reader_loop(
        read_half,
        trailing_bytes,
        outbound_tx.clone(),
        inbound_tx,
        close_sent.clone(),
        config.max_message_bytes,
        shutdown.clone(),
    ));

    Ok((
        WsWriter {
            tx: outbound_tx,
            close_sent,
        },
        WsReader {
            rx: inbound_rx,
            shutdown,
        },
    ))
}

fn validate_custom_headers(headers: &[(String, String)]) -> Result<(), MediaError> {
    for (name, value) in headers {
        if name.is_empty()
            || name
                .bytes()
                .any(|b| matches!(b, b'\r' | b'\n' | b'\0' | b':'))
            || value.bytes().any(|b| matches!(b, b'\r' | b'\n' | b'\0'))
        {
            return Err(MediaError::invalid_request(
                "invalid websocket header name or value",
            ));
        }
        let lower = name.to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "host" | "upgrade" | "connection" | "sec-websocket-key" | "sec-websocket-version"
        ) {
            return Err(MediaError::invalid_request(format!(
                "websocket handshake header '{name}' cannot be overridden"
            )));
        }
        http::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| MediaError::invalid_request("invalid websocket header name"))?;
        http::HeaderValue::from_str(value)
            .map_err(|_| MediaError::invalid_request("invalid websocket header value"))?;
    }
    Ok(())
}

async fn perform_upgrade_handshake(
    transport: &mut WsTransport,
    url: &url::Url,
    host: &str,
    port: u16,
    config: &WsConnectConfig,
    cancel: &CancellationToken,
) -> Result<BytesMut, MediaError> {
    let nonce = uuid::Uuid::new_v4().into_bytes();
    let ws_key = STANDARD.encode(nonce);

    let mut request_target = if url.path().is_empty() {
        "/".to_owned()
    } else {
        url.path().to_owned()
    };
    if let Some(q) = url.query() {
        request_target.push('?');
        request_target.push_str(q);
    }
    let default_port = if url.scheme() == "wss" { 443 } else { 80 };
    let host_header = if port == default_port {
        host.to_owned()
    } else {
        format!("{host}:{port}")
    };

    let mut req_buf = String::new();
    req_buf.push_str(&format!("GET {request_target} HTTP/1.1\r\n"));
    req_buf.push_str(&format!("Host: {host_header}\r\n"));
    req_buf.push_str("Upgrade: websocket\r\n");
    req_buf.push_str("Connection: Upgrade\r\n");
    req_buf.push_str(&format!("Sec-WebSocket-Key: {ws_key}\r\n"));
    req_buf.push_str("Sec-WebSocket-Version: 13\r\n");
    for (name, value) in &config.headers {
        req_buf.push_str(&format!("{name}: {value}\r\n"));
    }
    req_buf.push_str("\r\n");

    transport
        .write_all(req_buf.as_bytes())
        .await
        .map_err(|e| MediaError::new(MediaErrorCode::Transport, e.to_string()))?;
    transport
        .flush()
        .await
        .map_err(|e| MediaError::new(MediaErrorCode::Transport, e.to_string()))?;

    let mut buf = BytesMut::with_capacity(4096);
    let mut temp = [0u8; 4096];
    let header_len = loop {
        let mut headers = [httparse::EMPTY_HEADER; 64];
        let mut resp = httparse::Response::new(&mut headers);
        match resp.parse(&buf) {
            Ok(httparse::Status::Complete(len)) => {
                let code = resp.code.unwrap_or(0);
                if code != 101 {
                    let mapped = if (400..=599).contains(&code) {
                        super::engine2::map_http_error(code, &buf[len.min(buf.len())..])
                    } else {
                        MediaError::new(
                            MediaErrorCode::DecodeError,
                            format!("unexpected websocket handshake HTTP status {code}"),
                        )
                    };
                    return Err(mapped);
                }

                let mut has_upgrade = false;
                let mut has_connection = false;
                let mut accept_header: Option<String> = None;
                for hdr in resp.headers.iter() {
                    let val = std::str::from_utf8(hdr.value).unwrap_or("").trim();
                    if hdr.name.eq_ignore_ascii_case("upgrade")
                        && val.eq_ignore_ascii_case("websocket")
                    {
                        has_upgrade = true;
                    } else if hdr.name.eq_ignore_ascii_case("connection")
                        && val
                            .split(',')
                            .any(|tok| tok.trim().eq_ignore_ascii_case("upgrade"))
                    {
                        has_connection = true;
                    } else if hdr.name.eq_ignore_ascii_case("sec-websocket-accept") {
                        accept_header = Some(val.to_owned());
                    }
                }
                if !has_upgrade || !has_connection {
                    return Err(MediaError::new(
                        MediaErrorCode::DecodeError,
                        "websocket 101 response missing Upgrade or Connection header",
                    ));
                }
                let expected_accept =
                    STANDARD.encode(crypto::sha1_digest(format!("{ws_key}{WS_GUID}").as_bytes()));
                let got_accept = accept_header.unwrap_or_default();
                if !crypto::constant_time_eq(got_accept.as_bytes(), expected_accept.as_bytes()) {
                    return Err(MediaError::new(
                        MediaErrorCode::DecodeError,
                        "invalid Sec-WebSocket-Accept digest",
                    ));
                }
                break len;
            }
            Ok(httparse::Status::Partial) => {}
            Err(e) => {
                return Err(MediaError::new(
                    MediaErrorCode::DecodeError,
                    format!("malformed websocket HTTP upgrade response: {e}"),
                ));
            }
        }

        if buf.len() >= MAX_HANDSHAKE_BYTES {
            return Err(MediaError::new(
                MediaErrorCode::DecodeError,
                "websocket handshake response exceeded 64 KiB",
            ));
        }

        let n = tokio::select! {
            _ = cancel.cancelled() => return Err(MediaError::new(MediaErrorCode::Cancelled, "websocket handshake cancelled")),
            res = tokio::time::timeout(config.handshake_timeout, transport.read(&mut temp)) => {
                res.map_err(|_| MediaError::new(MediaErrorCode::Timeout, "websocket handshake timed out"))?
                    .map_err(|e| MediaError::new(MediaErrorCode::Transport, e.to_string()))?
            }
        };
        if n == 0 {
            return Err(MediaError::new(
                MediaErrorCode::Transport,
                "EOF during websocket handshake",
            ));
        }
        buf.extend_from_slice(&temp[..n]);
    };

    let _ = buf.split_to(header_len);
    Ok(buf)
}

async fn write_outbound_frame(
    writer: &mut tokio::io::WriteHalf<WsTransport>,
    msg: WsOutbound,
) -> Result<bool, ()> {
    let (opcode, payload, is_close) = match msg {
        WsOutbound::Text(s) => (OP_TEXT, s.into_bytes(), false),
        WsOutbound::Binary(b) => (OP_BINARY, b.to_vec(), false),
        WsOutbound::Ping(b) => (OP_PING, b.to_vec(), false),
        WsOutbound::Pong(b) => (OP_PONG, b.to_vec(), false),
        WsOutbound::Close(frame) => {
            let mut data = Vec::new();
            if let Some(f) = frame {
                data.extend_from_slice(&f.code.to_be_bytes());
                data.extend_from_slice(f.reason.as_bytes());
            }
            (OP_CLOSE, data, true)
        }
    };
    let encoded = encode_client_frame(opcode, &payload);
    if writer.write_all(&encoded).await.is_err() {
        return Err(());
    }
    if writer.flush().await.is_err() {
        return Err(());
    }
    Ok(is_close)
}

async fn ws_writer_loop(
    mut writer: tokio::io::WriteHalf<WsTransport>,
    mut rx: mpsc::Receiver<WsOutbound>,
    shutdown: CancellationToken,
) {
    loop {
        let msg = tokio::select! {
            _ = shutdown.cancelled() => {
                // Drain any already-enqueued outbound frames (e.g. echoed OP_CLOSE)
                // before exiting so immediate WsReader::drop cannot drop the Close reply.
                let _ = tokio::time::timeout(Duration::from_millis(500), async {
                    while let Ok(pending) = rx.try_recv() {
                        match write_outbound_frame(&mut writer, pending).await {
                            Ok(true) => {
                                let _ = writer.shutdown().await;
                                break;
                            }
                            Ok(false) => {}
                            Err(()) => break,
                        }
                    }
                })
                .await;
                break;
            }
            item = rx.recv() => match item {
                Some(m) => m,
                None => break,
            }
        };
        match write_outbound_frame(&mut writer, msg).await {
            Ok(true) => {
                let _ = writer.shutdown().await;
                break;
            }
            Ok(false) => {}
            Err(()) => break,
        }
    }
}

fn encode_client_frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(14 + payload.len());
    out.push(0x80 | (opcode & 0x0F));
    let len = payload.len();
    if len <= 125 {
        out.push(0x80 | (len as u8));
    } else if len <= 65535 {
        out.push(0x80 | 126);
        out.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        out.push(0x80 | 127);
        out.extend_from_slice(&(len as u64).to_be_bytes());
    }
    let mask = uuid::Uuid::new_v4().into_bytes();
    let mask_key = [mask[0], mask[1], mask[2], mask[3]];
    out.extend_from_slice(&mask_key);
    for (i, &b) in payload.iter().enumerate() {
        out.push(b ^ mask_key[i & 3]);
    }
    out
}

async fn ws_reader_loop(
    mut reader: tokio::io::ReadHalf<WsTransport>,
    mut buf: BytesMut,
    outbound_tx: mpsc::Sender<WsOutbound>,
    inbound_tx: mpsc::Sender<Result<WsMessage, MediaError>>,
    close_sent: Arc<AtomicBool>,
    max_message_bytes: usize,
    shutdown: CancellationToken,
) {
    let mut frag_opcode: Option<u8> = None;
    let mut frag_buf = BytesMut::new();
    let mut clean_close = false;

    loop {
        let frame = match read_next_frame(&mut reader, &mut buf, max_message_bytes, &shutdown).await
        {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(err) => {
                let _ = inbound_tx.send(Err(err)).await;
                break;
            }
        };

        match frame.opcode {
            OP_PING => {
                if !frame.fin || frame.payload.len() > 125 {
                    let _ = inbound_tx
                        .send(Err(MediaError::new(
                            MediaErrorCode::DecodeError,
                            "invalid websocket ping control frame",
                        )))
                        .await;
                    break;
                }
                let _ = outbound_tx.send(WsOutbound::Pong(frame.payload)).await;
            }
            OP_PONG => {
                if !frame.fin || frame.payload.len() > 125 {
                    let _ = inbound_tx
                        .send(Err(MediaError::new(
                            MediaErrorCode::DecodeError,
                            "invalid websocket pong control frame",
                        )))
                        .await;
                    break;
                }
                if inbound_tx
                    .send(Ok(WsMessage::Pong(frame.payload)))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            OP_CLOSE => {
                if !frame.fin || frame.payload.len() > 125 {
                    let _ = inbound_tx
                        .send(Err(MediaError::new(
                            MediaErrorCode::DecodeError,
                            "invalid websocket close control frame",
                        )))
                        .await;
                    break;
                }
                let close_frame = parse_close_payload(&frame.payload);
                if !close_sent.swap(true, Ordering::SeqCst) {
                    let _ = outbound_tx
                        .send(WsOutbound::Close(close_frame.clone()))
                        .await;
                }
                let _ = inbound_tx.send(Ok(WsMessage::Close(close_frame))).await;
                clean_close = true;
                break;
            }
            OP_TEXT | OP_BINARY => {
                if frag_opcode.is_some() {
                    let _ = inbound_tx
                        .send(Err(MediaError::new(
                            MediaErrorCode::DecodeError,
                            "received new data frame while fragmented message in progress",
                        )))
                        .await;
                    break;
                }
                if frame.fin {
                    if let Err(err) =
                        emit_data_message(frame.opcode, frame.payload, &inbound_tx).await
                    {
                        let _ = inbound_tx.send(Err(err)).await;
                        break;
                    }
                } else {
                    frag_opcode = Some(frame.opcode);
                    frag_buf.clear();
                    frag_buf.extend_from_slice(&frame.payload);
                }
            }
            OP_CONTINUATION => {
                let Some(op) = frag_opcode else {
                    let _ = inbound_tx
                        .send(Err(MediaError::new(
                            MediaErrorCode::DecodeError,
                            "unexpected websocket continuation frame",
                        )))
                        .await;
                    break;
                };
                if frag_buf.len().saturating_add(frame.payload.len()) > max_message_bytes {
                    let _ = inbound_tx
                        .send(Err(MediaError::new(
                            MediaErrorCode::DecodeError,
                            "fragmented websocket message exceeded max_message_bytes",
                        )))
                        .await;
                    break;
                }
                frag_buf.extend_from_slice(&frame.payload);
                if frame.fin {
                    frag_opcode = None;
                    let assembled = frag_buf.split().freeze();
                    if let Err(err) = emit_data_message(op, assembled, &inbound_tx).await {
                        let _ = inbound_tx.send(Err(err)).await;
                        break;
                    }
                }
            }
            other => {
                let _ = inbound_tx
                    .send(Err(MediaError::new(
                        MediaErrorCode::DecodeError,
                        format!("unsupported websocket opcode {other:#x}"),
                    )))
                    .await;
                break;
            }
        }
    }
    if !clean_close {
        shutdown.cancel();
    }
}

async fn emit_data_message(
    opcode: u8,
    payload: Bytes,
    tx: &mpsc::Sender<Result<WsMessage, MediaError>>,
) -> Result<(), MediaError> {
    let msg = if opcode == OP_TEXT {
        let text = String::from_utf8(payload.to_vec()).map_err(|e| {
            MediaError::new(
                MediaErrorCode::DecodeError,
                format!("websocket text frame is not valid UTF-8: {e}"),
            )
        })?;
        WsMessage::Text(text)
    } else {
        WsMessage::Binary(payload)
    };
    let _ = tx.send(Ok(msg)).await;
    Ok(())
}

fn parse_close_payload(payload: &[u8]) -> Option<WsCloseFrame> {
    if payload.len() < 2 {
        return None;
    }
    let code = u16::from_be_bytes([payload[0], payload[1]]);
    let reason = String::from_utf8_lossy(&payload[2..]).into_owned();
    Some(WsCloseFrame { code, reason })
}

struct RawFrame {
    fin: bool,
    opcode: u8,
    payload: Bytes,
}

async fn read_next_frame(
    reader: &mut tokio::io::ReadHalf<WsTransport>,
    buf: &mut BytesMut,
    max_message_bytes: usize,
    shutdown: &CancellationToken,
) -> Result<Option<RawFrame>, MediaError> {
    if !ensure_bytes(reader, buf, 2, shutdown).await? {
        return Ok(None);
    }
    let b0 = buf[0];
    let b1 = buf[1];
    let fin = (b0 & 0x80) != 0;
    let rsv = b0 & 0x70;
    let opcode = b0 & 0x0F;
    let masked = (b1 & 0x80) != 0;
    let len7 = b1 & 0x7F;

    if rsv != 0 {
        return Err(MediaError::new(
            MediaErrorCode::DecodeError,
            "websocket server frame set non-zero RSV bits",
        ));
    }
    if masked {
        return Err(MediaError::new(
            MediaErrorCode::DecodeError,
            "websocket server frame must not be masked",
        ));
    }

    let mut header_size = 2usize;
    let payload_len: usize = match len7 {
        0..=125 => usize::from(len7),
        126 => {
            header_size += 2;
            if !ensure_bytes(reader, buf, header_size, shutdown).await? {
                return Err(MediaError::new(
                    MediaErrorCode::Transport,
                    "unexpected EOF reading 16-bit WS frame length",
                ));
            }
            usize::from(u16::from_be_bytes([buf[2], buf[3]]))
        }
        _ => {
            header_size += 8;
            if !ensure_bytes(reader, buf, header_size, shutdown).await? {
                return Err(MediaError::new(
                    MediaErrorCode::Transport,
                    "unexpected EOF reading 64-bit WS frame length",
                ));
            }
            let len64 = u64::from_be_bytes(buf[2..10].try_into().unwrap());
            usize::try_from(len64).map_err(|_| {
                MediaError::new(
                    MediaErrorCode::DecodeError,
                    "64-bit WS frame length exceeds platform usize",
                )
            })?
        }
    };

    if payload_len > max_message_bytes {
        return Err(MediaError::new(
            MediaErrorCode::DecodeError,
            format!("websocket frame length {payload_len} exceeds limit {max_message_bytes}"),
        ));
    }

    buf.advance(header_size);
    if !ensure_bytes(reader, buf, payload_len, shutdown).await? {
        return Err(MediaError::new(
            MediaErrorCode::Transport,
            "unexpected EOF reading WS frame payload",
        ));
    }
    let payload = buf.split_to(payload_len).freeze();
    Ok(Some(RawFrame {
        fin,
        opcode,
        payload,
    }))
}

async fn ensure_bytes(
    reader: &mut tokio::io::ReadHalf<WsTransport>,
    buf: &mut BytesMut,
    needed: usize,
    shutdown: &CancellationToken,
) -> Result<bool, MediaError> {
    let mut temp = [0u8; 8192];
    while buf.len() < needed {
        let n = tokio::select! {
            _ = shutdown.cancelled() => return Ok(false),
            res = reader.read(&mut temp) => {
                res.map_err(|e| MediaError::new(MediaErrorCode::Transport, e.to_string()))?
            }
        };
        if n == 0 {
            return Ok(false);
        }
        buf.extend_from_slice(&temp[..n]);
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_server_frame(fin: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.push(if fin { 0x80 } else { 0x00 } | (opcode & 0x0F));
        let len = payload.len();
        if len <= 125 {
            out.push(len as u8);
        } else if len <= 65535 {
            out.push(126);
            out.extend_from_slice(&(len as u16).to_be_bytes());
        } else {
            out.push(127);
            out.extend_from_slice(&(len as u64).to_be_bytes());
        }
        out.extend_from_slice(payload);
        out
    }

    async fn read_client_frame(stream: &mut TcpStream) -> (bool, u8, Vec<u8>) {
        let mut hdr = [0u8; 2];
        stream.read_exact(&mut hdr).await.unwrap();
        let fin = (hdr[0] & 0x80) != 0;
        let opcode = hdr[0] & 0x0F;
        let masked = (hdr[1] & 0x80) != 0;
        assert!(masked, "client frames must be masked");
        let len7 = hdr[1] & 0x7F;
        let len: usize = match len7 {
            0..=125 => usize::from(len7),
            126 => {
                let mut b = [0u8; 2];
                stream.read_exact(&mut b).await.unwrap();
                usize::from(u16::from_be_bytes(b))
            }
            _ => {
                let mut b = [0u8; 8];
                stream.read_exact(&mut b).await.unwrap();
                u64::from_be_bytes(b) as usize
            }
        };
        let mut mask = [0u8; 4];
        stream.read_exact(&mut mask).await.unwrap();
        let mut payload = vec![0u8; len];
        stream.read_exact(&mut payload).await.unwrap();
        for (i, b) in payload.iter_mut().enumerate() {
            *b ^= mask[i & 3];
        }
        (fin, opcode, payload)
    }

    #[tokio::test]
    async fn ws_handshake_headers_large_frames_fragmentation_autopong_and_close() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_task = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut req_buf = Vec::new();
            let mut tmp = [0u8; 2048];
            let header_end = loop {
                let n = sock.read(&mut tmp).await.unwrap();
                req_buf.extend_from_slice(&tmp[..n]);
                if let Some(pos) = req_buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break pos + 4;
                }
            };
            let req_text = String::from_utf8_lossy(&req_buf[..header_end]);
            assert!(req_text.contains("Authorization: Bearer ws-token-123"));
            assert!(req_text.contains("x-goog-api-key: goog-ws-key"));
            let ws_key = req_text
                .lines()
                .find_map(|l| {
                    l.split_once(':')
                        .filter(|(k, _)| k.eq_ignore_ascii_case("sec-websocket-key"))
                        .map(|(_, v)| v.trim().to_owned())
                })
                .unwrap();
            let accept =
                STANDARD.encode(crypto::sha1_digest(format!("{ws_key}{WS_GUID}").as_bytes()));

            // Send 101 response PLUS an immediate server frame in the SAME TCP write
            // to verify post-header trailing bytes are never dropped!
            let mut resp = format!(
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
            )
            .into_bytes();
            resp.extend_from_slice(&encode_server_frame(true, OP_TEXT, b"hello-early"));
            sock.write_all(&resp).await.unwrap();

            // Receive >125B text and >64KiB binary frames from client
            let (fin1, op1, pay1) = read_client_frame(&mut sock).await;
            assert!(fin1 && op1 == OP_TEXT && pay1.len() == 300);
            let (fin2, op2, pay2) = read_client_frame(&mut sock).await;
            assert!(fin2 && op2 == OP_BINARY && pay2.len() == 70_000);

            // Send fragmented text message with an interleaved Ping
            sock.write_all(&encode_server_frame(false, OP_TEXT, b"frag1-"))
                .await
                .unwrap();
            sock.write_all(&encode_server_frame(true, OP_PING, b"hb"))
                .await
                .unwrap();
            sock.write_all(&encode_server_frame(true, OP_CONTINUATION, b"frag2"))
                .await
                .unwrap();

            // Client must auto-respond with Pong("hb")
            let (p_fin, p_op, p_pay) = read_client_frame(&mut sock).await;
            assert!(p_fin && p_op == OP_PONG && p_pay == b"hb");

            // Send >64KiB server binary frame
            let big = vec![0xABu8; 70_000];
            sock.write_all(&encode_server_frame(true, OP_BINARY, &big))
                .await
                .unwrap();

            // Clean close handshake
            let mut close_payload = 1000u16.to_be_bytes().to_vec();
            close_payload.extend_from_slice(b"bye");
            sock.write_all(&encode_server_frame(true, OP_CLOSE, &close_payload))
                .await
                .unwrap();
            let (c_fin, c_op, c_pay) = read_client_frame(&mut sock).await;
            assert!(c_fin && c_op == OP_CLOSE && c_pay[..2] == 1000u16.to_be_bytes());
        });

        let keys = CredentialSet::new(vec!["ws-token-123".into(), "goog-ws-key".into()]);
        let cancel = CancellationToken::new();
        let config = WsConnectConfig {
            headers: vec![
                ("Authorization".into(), "Bearer ws-token-123".into()),
                ("x-goog-api-key".into(), "goog-ws-key".into()),
            ],
            sensitive_headers: vec!["Authorization".into(), "x-goog-api-key".into()],
            ..Default::default()
        };
        let (writer, mut reader) =
            connect_ws(&format!("ws://{addr}/realtime"), config, &keys, &cancel)
                .await
                .unwrap();

        assert_eq!(
            reader.recv().await.unwrap().unwrap(),
            WsMessage::Text("hello-early".into())
        );

        writer.send_text("x".repeat(300)).await.unwrap();
        writer.send_binary(vec![0x55u8; 70_000]).await.unwrap();

        assert_eq!(
            reader.recv().await.unwrap().unwrap(),
            WsMessage::Text("frag1-frag2".into())
        );
        assert_eq!(
            reader.recv().await.unwrap().unwrap(),
            WsMessage::Binary(Bytes::from(vec![0xABu8; 70_000]))
        );
        assert_eq!(
            reader.recv().await.unwrap().unwrap(),
            WsMessage::Close(Some(WsCloseFrame {
                code: 1000,
                reason: "bye".into()
            }))
        );
        server_task.await.unwrap();
    }

    async fn accept_and_upgrade(listener: &tokio::net::TcpListener) -> TcpStream {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut req_buf = Vec::new();
        let mut tmp = [0u8; 2048];
        let header_end = loop {
            let n = sock.read(&mut tmp).await.unwrap();
            req_buf.extend_from_slice(&tmp[..n]);
            if let Some(pos) = req_buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos + 4;
            }
        };
        let req_text = String::from_utf8_lossy(&req_buf[..header_end]);
        let ws_key = req_text
            .lines()
            .find_map(|l| {
                l.split_once(':')
                    .filter(|(k, _)| k.eq_ignore_ascii_case("sec-websocket-key"))
                    .map(|(_, v)| v.trim().to_owned())
            })
            .unwrap();
        let accept = STANDARD.encode(crypto::sha1_digest(format!("{ws_key}{WS_GUID}").as_bytes()));
        let resp = format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
        );
        sock.write_all(resp.as_bytes()).await.unwrap();
        sock
    }

    #[tokio::test]
    async fn ws_server_close_immediate_reader_drop_50_trials() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_task = tokio::spawn(async move {
            for _ in 0..50 {
                let mut sock = accept_and_upgrade(&listener).await;
                let mut close_payload = 1000u16.to_be_bytes().to_vec();
                close_payload.extend_from_slice(b"srv-bye");
                sock.write_all(&encode_server_frame(true, OP_CLOSE, &close_payload))
                    .await
                    .unwrap();
                let (c_fin, c_op, c_pay) =
                    tokio::time::timeout(Duration::from_millis(500), read_client_frame(&mut sock))
                        .await
                        .expect("timed out waiting for echoed OP_CLOSE");
                assert!(c_fin && c_op == OP_CLOSE);
                assert_eq!(&c_pay[..2], &1000u16.to_be_bytes());
            }
        });

        let keys = CredentialSet::new(vec![]);
        let cancel = CancellationToken::new();
        for _ in 0..50 {
            let (_writer, mut reader) = connect_ws(
                &format!("ws://{addr}/rt"),
                WsConnectConfig::default(),
                &keys,
                &cancel,
            )
            .await
            .unwrap();
            let msg = reader.recv().await.unwrap().unwrap();
            assert_eq!(
                msg,
                WsMessage::Close(Some(WsCloseFrame {
                    code: 1000,
                    reason: "srv-bye".into(),
                }))
            );
            drop(reader);
        }
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn ws_rejects_fragmented_and_oversized_close_frames() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_task = tokio::spawn(async move {
            let mut sock1 = accept_and_upgrade(&listener).await;
            sock1
                .write_all(&encode_server_frame(
                    false,
                    OP_CLOSE,
                    &1000u16.to_be_bytes(),
                ))
                .await
                .unwrap();

            let mut sock2 = accept_and_upgrade(&listener).await;
            let mut oversized = 1000u16.to_be_bytes().to_vec();
            oversized.extend_from_slice(&[b'a'; 124]); // 126 bytes total > 125
            sock2
                .write_all(&encode_server_frame(true, OP_CLOSE, &oversized))
                .await
                .unwrap();
        });

        let keys = CredentialSet::new(vec![]);
        let cancel = CancellationToken::new();

        let (_w1, mut r1) = connect_ws(
            &format!("ws://{addr}/frag"),
            WsConnectConfig::default(),
            &keys,
            &cancel,
        )
        .await
        .unwrap();
        let err1 = r1.recv().await.unwrap().unwrap_err();
        assert_eq!(err1.code, MediaErrorCode::DecodeError);

        let (_w2, mut r2) = connect_ws(
            &format!("ws://{addr}/big"),
            WsConnectConfig::default(),
            &keys,
            &cancel,
        )
        .await
        .unwrap();
        let err2 = r2.recv().await.unwrap().unwrap_err();
        assert_eq!(err2.code, MediaErrorCode::DecodeError);

        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn ws_scrubs_semicolon_bearer_secret_on_handshake_error() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server_task = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 2048];
            let _ = sock.read(&mut buf).await.unwrap();
            let body = r#"{"error":{"message":"invalid Bearer;v1-secret-tok"}}"#;
            let resp = format!(
                "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            sock.write_all(resp.as_bytes()).await.unwrap();
        });

        let keys = CredentialSet::new(vec![]);
        let cancel = CancellationToken::new();
        let config = WsConnectConfig {
            headers: vec![("Authorization".into(), "Bearer;v1-secret-tok".into())],
            sensitive_headers: vec!["Authorization".into()],
            ..Default::default()
        };
        let err = match connect_ws(&format!("ws://{addr}/v1"), config, &keys, &cancel).await {
            Ok(_) => panic!("expected 401 handshake failure"),
            Err(e) => e,
        };
        assert_eq!(err.code, MediaErrorCode::AuthRejected);
        assert!(!err.message.contains("v1-secret-tok"));
        assert!(err.message.contains("[redacted]"));
        server_task.await.unwrap();
    }
}
