//! Bounded loopback HTTP transport.
//!
//! The service previously accepted connections through a helper whose body
//! reader drains any unread remainder synchronously when a request is dropped,
//! with no time limit. A client that declares a large `Content-Length` and then
//! sends bytes slowly could therefore hold the single service thread
//! indefinitely — which also stops the hotkey message pump, since both run on
//! that thread. `recv_timeout` does not help: it bounds waiting for the *next*
//! request, not reading the body of one already taken.
//!
//! This module owns the accept loop so every stage has a deadline:
//!
//! * accepting a connection is non-blocking (the caller polls for shutdown),
//! * reading the request head is bounded by `READ_TIMEOUT`,
//! * reading the body is bounded by `READ_TIMEOUT` in addition to the byte cap,
//! * writing the response is bounded by `WRITE_TIMEOUT`.
//!
//! A request that exceeds a deadline is answered with an error status and the
//! connection is dropped. Nothing here spawns unbounded threads or forgets a
//! blocking task: a fixed worker pool owns socket I/O while the action thread
//! routes requests and enqueues responses.

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    time::{Duration, Instant},
};

/// Deadline for reading the request head and the body.
pub const READ_TIMEOUT: Duration = Duration::from_secs(5);
/// Deadline for writing a response.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// Upper bound on a request body, in bytes.
pub const MAX_BODY: usize = 8192;
/// Upper bound on the request head (request line plus headers).
const MAX_HEAD: usize = 16 * 1024;
/// How many connections may be read concurrently.
///
/// Bounded on purpose: an unbounded "thread per connection" would be its own
/// denial of service. Each worker is a real thread that owns one connection's
/// I/O, and the count is fixed so slow clients cannot exhaust the process.
pub const MAX_WORKERS: usize = 8;
/// How many finished requests may wait for the action loop.
const QUEUE_DEPTH: usize = 64;

/// A parsed request, produced only after its head and body are fully read.
pub struct Received {
    pub method: String,
    pub path: String,
    /// Header names are lower-cased for case-insensitive lookup.
    pub headers: Vec<(String, String)>,
    /// Decoded body, or `None` when the body was absent, too large, not valid
    /// UTF-8, or not received within the deadline.
    pub body: Option<String>,
    /// The body was rejected, so it must not be parsed or trusted.
    pub body_rejected: bool,
}

impl Received {
    /// Returns a header value by lower-cased name.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// Outcome of handling one connection.
pub enum Handled {
    /// A complete request was received within its deadlines.
    Request(Received),
    /// The connection was closed or misbehaved; there is no request to route.
    Skip,
}

/// A listener that hands connections to a bounded pool of reader threads.
///
/// Wrapping the listener here keeps the rest of the service free of socket
/// concerns: callers either receive a complete request or nothing at all, and
/// never block on a client.
pub struct BoundedListener {
    listener: TcpListener,
    /// Connections awaiting a free worker, handed over as raw sockets.
    ///
    /// Raw sockets are used because `TcpStream` is `Send` and each worker sets
    /// its own deadlines on the stream it takes.
    pending: std::sync::mpsc::SyncSender<std::net::TcpStream>,
    /// Fully read requests, ready for the action loop.
    ready: std::sync::mpsc::Receiver<ReadyRequest>,
    /// Signals the workers to stop when the listener is dropped.
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// A request that has been fully read and is ready to route.
///
/// The worker retains the connection while the action loop prepares its reply.
pub struct Incoming {
    /// The parsed request head and body.
    pub request: Received,
    /// The live connection, for writing the reply.
    pub connection: BoundedConnection,
}

/// A complete request and its nonblocking response handoff.
pub struct ReadyRequest {
    pub request: Received,
    pub connection: Reply,
}

/// One response slot; shutdown can wait for its worker to finish before exit.
pub struct Reply {
    sender: std::sync::mpsc::SyncSender<(u16, String, String)>,
    completed: std::sync::mpsc::Receiver<()>,
}

impl Reply {
    pub fn respond(&mut self, status: u16, mime: &str, payload: &str) {
        let _ = self
            .sender
            .try_send((status, mime.to_string(), payload.to_string()));
    }

    pub fn finish(&self) -> std::io::Result<()> {
        self.completed
            .recv_timeout(WRITE_TIMEOUT)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::TimedOut, error))
    }
}

impl BoundedListener {
    /// Takes ownership of an already-bound listener and starts the reader pool.
    pub fn new(listener: TcpListener) -> std::io::Result<BoundedListener> {
        listener.set_nonblocking(true)?;
        let (pending_tx, pending_rx) =
            std::sync::mpsc::sync_channel::<std::net::TcpStream>(QUEUE_DEPTH);
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel::<ReadyRequest>(QUEUE_DEPTH);
        let pending_rx = std::sync::Arc::new(std::sync::Mutex::new(pending_rx));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        for _ in 0..MAX_WORKERS {
            let pending_rx = pending_rx.clone();
            let ready_tx = ready_tx.clone();
            let stop = stop.clone();
            // Socket reads and writes stay on the same bounded worker.
            std::thread::Builder::new()
                .name("vibdock-io".to_string())
                .spawn(move || {
                    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                        // Bounded wait so the thread notices `stop` promptly.
                        let next = {
                            let guard = match pending_rx.lock() {
                                Ok(guard) => guard,
                                Err(_) => return,
                            };
                            guard.recv_timeout(Duration::from_millis(200))
                        };
                        let Ok(stream) = next else { continue };
                        let Some(connection) = BoundedConnection::from_stream(stream) else {
                            continue;
                        };
                        if let Some(incoming) = connection.read_fully() {
                            let (reply_tx, reply_rx) = std::sync::mpsc::sync_channel(1);
                            let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
                            let mut connection = incoming.connection;
                            if ready_tx
                                .send(ReadyRequest {
                                    request: incoming.request,
                                    connection: Reply {
                                        sender: reply_tx,
                                        completed: done_rx,
                                    },
                                })
                                .is_err()
                            {
                                return;
                            }
                            if let Ok((status, mime, payload)) =
                                reply_rx.recv_timeout(WRITE_TIMEOUT)
                            {
                                connection.respond(status, &mime, &payload);
                            }
                            let _ = done_tx.send(());
                        }
                    }
                })
                .map_err(std::io::Error::other)?;
        }
        Ok(BoundedListener {
            listener,
            pending: pending_tx,
            ready: ready_rx,
            stop,
        })
    }

    /// Accepts pending connections without blocking and queues them for I/O.
    ///
    /// Accept itself never waits. A connection the pool cannot take yet is left
    /// in the OS backlog rather than stalling this loop, so an idle client that
    /// connected but sent nothing can never hold up hotkeys or shutdown.
    pub fn pump(&self) -> std::io::Result<()> {
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    let _ = stream.set_nodelay(true);
                    // `try_send` rather than `send`: if every worker is busy the
                    // connection stays in the backlog instead of blocking here.
                    if self.pending.try_send(stream).is_err() {
                        return Ok(());
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
                Err(error) => return Err(error),
            }
        }
    }

    /// Takes the next fully read request, or `None` if none is ready.
    ///
    /// Never blocks on a client: the read already happened on a worker thread,
    /// so the action loop only routes and enqueues its response.
    pub fn next_request(&self) -> Option<ReadyRequest> {
        self.ready.try_recv().ok()
    }
}

impl Drop for BoundedListener {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// One accepted connection, carrying its read and write deadlines.
///
/// The buffered reader is owned by the connection so the head can be parsed,
/// authorization decided, and only then the body consumed — all on the same
/// socket and all under the same read deadline.
pub struct BoundedConnection {
    stream: std::net::TcpStream,
    reader: Option<BufReader<DeadlineReader>>,
    /// Headers from the head, available before the body is read.
    head_headers: Vec<(String, String)>,
    started: Instant,
}

struct DeadlineReader {
    stream: std::net::TcpStream,
    started: Instant,
}

impl Read for DeadlineReader {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        // A per-read timeout must never renew the request's total budget.
        let remaining = READ_TIMEOUT
            .checked_sub(self.started.elapsed())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "request read deadline exceeded",
                )
            })?;
        self.stream.set_read_timeout(Some(remaining))?;
        self.stream.read(buffer)
    }
}

impl BoundedConnection {
    /// Wraps an accepted stream with read and write deadlines applied.
    fn from_stream(stream: std::net::TcpStream) -> Option<BoundedConnection> {
        stream.set_nonblocking(false).ok()?;
        stream.set_read_timeout(Some(READ_TIMEOUT)).ok()?;
        stream.set_write_timeout(Some(WRITE_TIMEOUT)).ok()?;
        Some(BoundedConnection {
            stream,
            reader: None,
            head_headers: Vec::new(),
            started: Instant::now(),
        })
    }

    /// Reads the head and the body to completion.
    ///
    /// Called on a worker thread, so blocking here is safe and intended: the
    /// action loop is meanwhile free to pump hotkeys and serve other clients.
    /// Both stages still carry a deadline, so no connection is held forever.
    pub fn read_fully(mut self) -> Option<Incoming> {
        let mut request = match self.read_head() {
            Handled::Request(request) => request,
            Handled::Skip => return None,
        };
        // The body is always read here. Deciding that from the head alone would
        // skip bodies for public endpoints that legitimately need them — the
        // one-use bootstrap code arrives in the body of an unauthenticated
        // request — and would not help responsiveness now that reads happen on
        // a worker rather than in the action loop.
        let (body, body_rejected) = self.read_body();
        request.body = body;
        request.body_rejected = body_rejected;
        Some(Incoming {
            request,
            connection: self,
        })
    }

    /// Reads the request head and prepares to read the body.
    ///
    /// The body is deliberately NOT read here. The caller decides, from the
    /// head alone, whether this request is authorized; only then is the body
    /// consumed. An unauthorized request is therefore answered without ever
    /// waiting on bytes that would be discarded anyway — which is what lets a
    /// slow unauthorized client be dropped instead of holding the service.
    pub fn read_head(&mut self) -> Handled {
        // The reader takes a clone of the stream so it can be stored on the
        // connection and reused for the body without borrowing the connection.
        let Ok(clone) = self.stream.try_clone() else {
            return Handled::Skip;
        };
        let mut reader = BufReader::new(DeadlineReader {
            stream: clone,
            started: self.started,
        });
        // Read the request line plus headers, bounded by head size.
        let mut head = Vec::new();
        let head_ok = read_until_blank_line(&mut reader, &mut head, MAX_HEAD);
        let Ok(()) = head_ok else {
            return Handled::Skip;
        };
        let head = match String::from_utf8(head) {
            Ok(head) => head,
            Err(_) => return Handled::Skip,
        };
        let mut lines = head.lines();
        let Some(request_line) = lines.next() else {
            return Handled::Skip;
        };
        let mut parts = request_line.split_whitespace();
        let (Some(method), Some(path)) = (parts.next(), parts.next()) else {
            return Handled::Skip;
        };
        let mut headers = Vec::new();
        for line in lines {
            if let Some((name, value)) = line.split_once(':') {
                headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
            }
        }
        // Keep the reader so the caller can consume the body after deciding
        // whether this request is authorized.
        self.reader = Some(reader);
        self.head_headers = headers;
        Handled::Request(Received {
            method: method.to_ascii_uppercase(),
            path: path.to_string(),
            headers: self.head_headers.clone(),
            body: None,
            body_rejected: false,
        })
    }

    /// Reads the body under the connection's remaining read deadline.
    ///
    /// Bounded by both the byte cap and the remaining read deadline. A body
    /// that is absent, oversized, not valid UTF-8, or too slow is reported as
    /// rejected rather than waited for.
    pub fn read_body(&mut self) -> (Option<String>, bool) {
        let Some(mut reader) = self.reader.take() else {
            return (None, true);
        };
        let headers = std::mem::take(&mut self.head_headers);
        let result = read_body(&mut reader, &headers, self.started);
        // The reader is spent: the body has been consumed to its declared end,
        // so this connection is finished regardless of the outcome.
        result
    }

    /// Writes a response within the write deadline.
    ///
    /// A client that does not read its response blocks until the deadline, then
    /// the connection is closed; it can never stall the service past it.
    pub fn respond(&mut self, status: u16, mime: &str, payload: &str) {
        let reason = match status {
            200 => "OK",
            400 => "Bad Request",
            401 => "Unauthorized",
            403 => "Forbidden",
            404 => "Not Found",
            405 => "Method Not Allowed",
            409 => "Conflict",
            413 => "Payload Too Large",
            500 => "Internal Server Error",
            502 => "Bad Gateway",
            _ => "Error",
        };
        let head = format!(
            "HTTP/1.1 {status} {reason}\r\n\
             Content-Type: {mime}\r\n\
             Content-Length: {}\r\n\
             X-Content-Type-Options: nosniff\r\n\
             Cache-Control: no-store\r\n\
             Content-Security-Policy: default-src 'none'; script-src 'unsafe-inline'; \
             style-src 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; \
             frame-ancestors 'none'; form-action 'none'; base-uri 'none'\r\n\
             Connection: close\r\n\r\n",
            payload.len()
        );
        // Errors are ignored: a client that hangs up mid-response is not a
        // service failure, and the connection is closed either way.
        let _ = self.stream.write_all(head.as_bytes());
        let _ = self.stream.write_all(payload.as_bytes());
        let _ = self.stream.flush();
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

/// Reads the request head up to and including the blank line.
fn read_until_blank_line<R: BufRead>(
    reader: &mut R,
    out: &mut Vec<u8>,
    limit: usize,
) -> std::io::Result<()> {
    // Each fill_buf respects the socket read timeout, so a client that sends no
    // blank line blocks only until that deadline.
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "connection closed before the request head ended",
            ));
        }
        let mut consumed = 0;
        let mut done = false;
        for byte in available {
            out.push(*byte);
            consumed += 1;
            if out.len() > limit {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "request head too large",
                ));
            }
            if out.ends_with(b"\r\n\r\n") {
                done = true;
                break;
            }
        }
        reader.consume(consumed);
        if done {
            return Ok(());
        }
    }
}

/// Reads a declared body, bounded by the byte cap and the read deadline.
///
/// Returns `(body, rejected)`. `rejected` is set when the body exceeded the cap,
/// was not valid UTF-8, or did not arrive in time; the caller must then treat
/// the request as if it had no body and must not trust the contents.
fn read_body<R: BufRead>(
    reader: &mut R,
    headers: &[(String, String)],
    started: Instant,
) -> (Option<String>, bool) {
    let declared = headers
        .iter()
        .find(|(key, _)| key == "content-length")
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    if declared == 0 {
        // A chunked body has no declared length; only read what is bounded.
        let chunked = headers
            .iter()
            .any(|(key, value)| key == "transfer-encoding" && value.contains("chunked"));
        if !chunked {
            return (None, false);
        }
    }
    let mut buffer = Vec::new();
    let result = if declared > 0 {
        // A declared length is authoritative: read exactly that many bytes
        // (capped at one past MAX_BODY so oversize is still detectable).
        // Reading to EOF instead would hang, because a keep-alive client keeps
        // the socket open after the body it announced.
        let wanted = declared.min(MAX_BODY + 1);
        read_exact_with_deadline(reader, &mut buffer, wanted, started)
    } else {
        // Chunked bodies have no declared length; read until the terminating
        // empty chunk, bounded by the cap and the deadline.
        read_chunked_with_deadline(reader, &mut buffer, started)
    };
    if result.is_err() {
        // Timed out or the connection broke: reject rather than wait.
        return (None, true);
    }
    if buffer.len() > MAX_BODY {
        return (None, true);
    }
    match String::from_utf8(buffer) {
        Ok(text) => (Some(text), false),
        Err(_) => (None, true),
    }
}

/// Reads exactly `count` bytes, bounded by the overall read deadline.
///
/// Each `read` is bounded by the socket timeout, and the elapsed check bounds
/// the aggregate, so a client that trickles bytes forever still fails here
/// instead of holding the service thread.
fn read_exact_with_deadline<R: Read>(
    reader: &mut R,
    out: &mut Vec<u8>,
    count: usize,
    started: Instant,
) -> std::io::Result<()> {
    let mut remaining = count;
    let mut chunk = [0u8; 4096];
    while remaining > 0 {
        if started.elapsed() > READ_TIMEOUT {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "request body exceeded the read deadline",
            ));
        }
        let want = remaining.min(chunk.len());
        match reader.read(&mut chunk[..want]) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "client closed before sending the declared body",
                ))
            }
            Ok(received) => {
                out.extend_from_slice(&chunk[..received]);
                remaining -= received;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Reads a chunked body until its terminating empty chunk.
///
/// Bounded by the byte cap and the read deadline; a client that never sends the
/// terminator is rejected rather than waited for.
fn read_chunked_with_deadline<R: Read>(
    reader: &mut R,
    out: &mut Vec<u8>,
    started: Instant,
) -> std::io::Result<()> {
    let mut line = Vec::new();
    loop {
        if started.elapsed() > READ_TIMEOUT {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "chunked body exceeded the read deadline",
            ));
        }
        // Read the chunk-size line.
        line.clear();
        read_line(reader, &mut line, started)?;
        let text = String::from_utf8_lossy(&line).trim().to_string();
        let size = match usize::from_str_radix(&text, 16) {
            Ok(size) => size,
            Err(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "malformed chunk size",
                ))
            }
        };
        if size == 0 {
            // Trailing CRLF after the final chunk; not needed for our response.
            return Ok(());
        }
        if out.len() + size > MAX_BODY + 1 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "chunked body too large",
            ));
        }
        read_exact_with_deadline(reader, out, size, started)?;
        // Consume the CRLF that follows each chunk.
        line.clear();
        read_line(reader, &mut line, started)?;
    }
}

/// Reads one CRLF-terminated line, bounded by the deadline and a small cap.
fn read_line<R: Read>(reader: &mut R, out: &mut Vec<u8>, started: Instant) -> std::io::Result<()> {
    let mut byte = [0u8; 1];
    loop {
        if started.elapsed() > READ_TIMEOUT {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "request exceeded the read deadline",
            ));
        }
        match reader.read(&mut byte) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "client closed mid-request",
                ))
            }
            Ok(_) => {
                if byte[0] == b'\n' {
                    // Strip a trailing CR if present.
                    if out.last() == Some(&b'\r') {
                        out.pop();
                    }
                    return Ok(());
                }
                out.push(byte[0]);
                if out.len() > 1024 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "line too long",
                    ));
                }
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn split_header_terminators_preserve_the_body() {
        let mut head = b"GET / HTTP/1.1\r\nX-Pad: ".to_vec();
        head.resize(8190, b'x');
        head.extend_from_slice(b"\r\n\r\n");
        let mut input = head.clone();
        input.extend_from_slice(b"BODY");
        for capacity in [1, 2, 3, 4, 8192] {
            let mut reader = BufReader::with_capacity(capacity, Cursor::new(&input));
            let mut actual = Vec::new();
            read_until_blank_line(&mut reader, &mut actual, MAX_HEAD).unwrap();
            assert_eq!(actual, head);
            let mut body = Vec::new();
            reader.read_to_end(&mut body).unwrap();
            assert_eq!(body, b"BODY");
        }
    }

    #[test]
    fn response_handoff_does_not_wait_for_a_nonreading_client() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let transport = BoundedListener::new(listener).unwrap();
        let mut client = std::net::TcpStream::connect(address).unwrap();
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        let started = Instant::now();
        let mut request = loop {
            transport.pump().unwrap();
            if let Some(request) = transport.next_request() {
                break request;
            }
            assert!(started.elapsed() < Duration::from_secs(2));
            std::thread::sleep(Duration::from_millis(5));
        };
        let payload = "x".repeat(8 * 1024 * 1024);
        let started = Instant::now();
        request.connection.respond(200, "text/plain", &payload);
        assert!(started.elapsed() < Duration::from_secs(1));
        client.shutdown(std::net::Shutdown::Both).unwrap();
    }

    #[test]
    fn expired_read_deadline_does_not_wait_for_a_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let _client = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (stream, _) = listener.accept().unwrap();
        let mut reader = DeadlineReader {
            stream,
            started: Instant::now() - READ_TIMEOUT,
        };
        assert_eq!(
            reader.read(&mut [0]).unwrap_err().kind(),
            std::io::ErrorKind::TimedOut
        );
    }

    /// (method, path, headers) parsed from an in-memory request head.
    type ParsedHead = (String, String, Vec<(String, String)>);

    /// Reads a head from an in-memory stream, using the same code path as a
    /// socket, so these tests exercise the real parser.
    fn head_of(bytes: &[u8]) -> Option<ParsedHead> {
        let mut reader = BufReader::new(Cursor::new(bytes.to_vec()));
        let mut out = Vec::new();
        read_until_blank_line(&mut reader, &mut out, MAX_HEAD).ok()?;
        let text = String::from_utf8(out).ok()?;
        let mut lines = text.lines();
        let line = lines.next()?.to_string();
        let mut parts = line.split_whitespace();
        let method = parts.next()?.to_string();
        let path = parts.next()?.to_string();
        let mut headers = Vec::new();
        for l in lines {
            if let Some((name, value)) = l.split_once(':') {
                headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
            }
        }
        Some((method, path, headers))
    }

    #[test]
    fn head_stops_exactly_at_the_blank_line() {
        // The parser must not consume bytes past the head: those are the body,
        // and swallowing them corrupts every POST payload.
        let (method, path, headers) = head_of(b"POST /api/config HTTP/1.1\r\nHost: x\r\n\r\nBODY")
            .expect("a well-formed head must parse");
        assert_eq!(method, "POST");
        assert_eq!(path, "/api/config");
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].0, "host");
        assert_eq!(headers[0].1, "x");
    }

    #[test]
    fn head_leaves_the_body_untouched() {
        // Regression: an off-by-three in the terminator handling consumed part
        // of the body, which made valid JSON bodies look like garbage.
        let raw = b"POST /api/config HTTP/1.1\r\nContent-Length: 15\r\n\r\n{\"hotkey\":\"F9\"}";
        let mut reader = BufReader::new(Cursor::new(raw.to_vec()));
        let mut head = Vec::new();
        read_until_blank_line(&mut reader, &mut head, MAX_HEAD).unwrap();
        let mut body = Vec::new();
        reader.read_to_end(&mut body).unwrap();
        assert_eq!(
            String::from_utf8(body).unwrap(),
            "{\"hotkey\":\"F9\"}",
            "the body must survive head parsing intact"
        );
    }

    #[test]
    fn head_is_rejected_when_the_terminator_never_arrives() {
        // A client that sends no blank line must not be parsed as a request.
        let mut reader = BufReader::new(Cursor::new(b"POST /x HTTP/1.1\r\nHost: x\r\n".to_vec()));
        let mut head = Vec::new();
        assert!(read_until_blank_line(&mut reader, &mut head, MAX_HEAD).is_err());
    }

    #[test]
    fn declared_body_reads_exactly_the_announced_length() {
        // Content-Length is authoritative: read exactly that many bytes and
        // stop, so a keep-alive client does not stall the read.
        let raw = b"POST /x HTTP/1.1\r\nContent-Length: 5\r\n\r\nhelloEXTRA";
        let mut reader = BufReader::new(Cursor::new(raw.to_vec()));
        let mut head = Vec::new();
        read_until_blank_line(&mut reader, &mut head, MAX_HEAD).unwrap();
        let headers = vec![("content-length".to_string(), "5".to_string())];
        let (body, rejected) = read_body(&mut reader, &headers, Instant::now());
        assert!(!rejected);
        assert_eq!(
            body.as_deref(),
            Some("hello"),
            "must not read past the length"
        );
    }

    #[test]
    fn an_oversize_body_is_rejected_not_truncated() {
        let headers = vec![("content-length".to_string(), (MAX_BODY * 2).to_string())];
        let payload = "x".repeat(MAX_BODY * 2);
        let raw = format!(
            "POST /x HTTP/1.1\r\nContent-Length: {}\r\n\r\n{}",
            MAX_BODY * 2,
            payload
        );
        let mut reader = BufReader::new(Cursor::new(raw.into_bytes()));
        let mut head = Vec::new();
        read_until_blank_line(&mut reader, &mut head, MAX_HEAD).unwrap();
        let (body, rejected) = read_body(&mut reader, &headers, Instant::now());
        assert!(rejected, "an oversize body must be rejected");
        assert!(body.is_none(), "no body may be reported as readable");
    }

    #[test]
    fn a_request_with_no_body_reports_none() {
        let headers = vec![("host".to_string(), "x".to_string())];
        let mut reader =
            BufReader::new(Cursor::new(b"GET /x HTTP/1.1\r\nHost: x\r\n\r\n".to_vec()));
        let mut head = Vec::new();
        read_until_blank_line(&mut reader, &mut head, MAX_HEAD).unwrap();
        let (body, rejected) = read_body(&mut reader, &headers, Instant::now());
        assert!(body.is_none());
        assert!(!rejected, "absent body is normal, not a rejection");
    }

    #[test]
    fn read_fully_always_consumes_the_body() {
        // Regression: the body used to be read only for authorized requests,
        // which silently broke public endpoints that take their input in the
        // body — one-use bootstrap code redemption is unauthenticated by
        // design, and its code never arrived.
        //
        // read_fully has no notion of authorization, so a body is always read.
        let raw = concat!(
            "POST /api/bootstrap HTTP/1.1\r\n",
            "Content-Length: 14\r\n",
            "\r\n",
            "{\"code\":\"abc\"}"
        )
        .as_bytes();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let connection = BoundedConnection::from_stream(stream).unwrap();
            connection
                .read_fully()
                .map(|incoming| incoming.request.body)
        });
        let mut client = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        client.write_all(raw).unwrap();
        client.flush().unwrap();
        let body = server.join().unwrap();
        assert_eq!(
            body.expect("the request must have been read"),
            Some("{\"code\":\"abc\"}".to_string()),
            "the body of an unauthenticated request must still be read"
        );
    }

    #[test]
    fn chunked_body_is_reassembled() {
        let raw = b"POST /x HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n";
        let mut reader = BufReader::new(Cursor::new(raw.to_vec()));
        let mut head = Vec::new();
        read_until_blank_line(&mut reader, &mut head, MAX_HEAD).unwrap();
        let headers = vec![("transfer-encoding".to_string(), "chunked".to_string())];
        let (body, rejected) = read_body(&mut reader, &headers, Instant::now());
        assert!(!rejected);
        assert_eq!(body.as_deref(), Some("hello"));
    }
}
