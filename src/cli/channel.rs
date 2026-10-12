//! Channel: pub/sub stream of string events over a Unix domain socket.
//!
//! ## Security model
//!
//! - On Linux, the publisher binds an **abstract socket** (path starts with
//!   `\0`) — no filesystem exposure, so symlink attacks are impossible and
//!   the kernel cleans up the name when the publisher's file descriptor closes.
//! - On macOS and other Unixes, the publisher binds a filesystem path in
//!   `$HQ_STORAGE_PATH/channels/<id>.sock` with 0o700 permissions. Only the
//!   owner can connect.
//! - On every platform, the publisher verifies each incoming connection's
//!   peer credentials (`peer_cred`) and rejects connections from processes
//!   owned by a different user. This is defense in depth on top of file
//!   permissions.
//!
//! ## Trust boundary
//!
//! Unix sockets carry no encryption. The trust boundary is "same user on the
//! same host" — any process owned by the publisher's user can subscribe.
//! Events are visible to any such process; do not send secrets if untrusted
//! local processes share the host.

use anyhow::{Result, anyhow, bail};
use async_stream::stream;
use futures::StreamExt;
use futures::stream::BoxStream;
#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader, stdin};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast;
use tokio::time::{Instant, sleep};
use tracing::warn;

/// Validate a channel ID. Must be non-empty and alphanumeric with dashes/underscores.
///
/// This prevents path traversal (`../`), embedded null bytes, and slash separators
/// from reaching socket path construction.
pub fn validate_channel_id(id: &str) -> Result<()> {
    if id.is_empty() {
        bail!("channel ID cannot be empty");
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!("channel ID must be alphanumeric with dashes/underscores only");
    }
    Ok(())
}

/// Construct the socket path for a channel ID.
///
/// On Linux: uses the abstract socket namespace — path bytes start with `\0`,
/// so there is no filesystem exposure and symlink attacks are impossible.
///
/// On macOS/other Unix: filesystem path in `<storage_path>/channels/<id>.sock`.
pub fn socket_path(storage_path: &str, id: &str) -> Result<PathBuf> {
    validate_channel_id(id)?;

    #[cfg(target_os = "linux")]
    {
        let _ = storage_path; // Linux abstract sockets don't use the filesystem
        let mut bytes = Vec::new();
        bytes.push(0u8); // abstract socket marker
        bytes.extend_from_slice(b"hq-channel-");
        bytes.extend_from_slice(id.as_bytes());
        Ok(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
    }

    #[cfg(not(target_os = "linux"))]
    {
        Ok(PathBuf::from(storage_path)
            .join("channels")
            .join(format!("{}.sock", id)))
    }
}

/// RAII guard that removes a socket file on Drop.
///
/// On macOS (filesystem paths), this ensures the `.sock` file is removed even
/// if the publisher panics or exits via Ctrl-C. On Linux (abstract sockets),
/// there is no file to clean up — the kernel handles it when the fd closes.
struct SocketGuard {
    path: Option<PathBuf>,
}

impl SocketGuard {
    fn new_file(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    #[cfg(target_os = "linux")]
    fn no_file() -> Self {
        Self { path: None }
    }
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            // Best-effort cleanup; don't panic in Drop.
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Transform a single event. Currently an identity function.
///
/// This is the stub for future transforms that modify or enrich events before
/// they are emitted by the channel.
pub async fn transform(input: String) -> String {
    input
}

/// Build a stream of events read from stdin, with the channel transform applied.
///
/// Incoming lines are coalesced into events using a debounce window: lines that
/// arrive within `debounce` of each other are grouped into a single event. This
/// treats a burst of multi-line output from a piped command (e.g. a website
/// dumped every 60s) as one event rather than one event per line. The stream
/// ends when stdin reaches EOF.
pub fn event_stream(debounce: Duration) -> BoxStream<'static, String> {
    event_stream_from_reader(BufReader::new(stdin()), debounce)
}

/// Debounce-coalesced event stream over an arbitrary async buffered reader.
///
/// Reads lines, appending to a buffer, and resets an idle timer on each line.
/// When the timer expires with no new input, the accumulated buffer is yielded
/// as one event. The reader is generic so the coalescing logic is testable
/// without touching process stdin, and reusable by subscribers.
pub(crate) fn event_stream_from_reader<R: AsyncBufRead + Unpin + Send + 'static>(
    reader: R,
    debounce: Duration,
) -> BoxStream<'static, String> {
    let s = stream! {
        let mut reader = reader;
        // `line` holds the most recently read line; `burst` accumulates the
        // lines that arrive within a single debounce window.
        let mut line = Vec::new();
        let mut burst = String::new();
        let idle = sleep(debounce);
        tokio::pin!(idle);
        loop {
            tokio::select! {
                n = reader.read_until(b'\n', &mut line) => {
                    match n {
                        Ok(0) => {
                            // EOF: flush any remaining burst, then end.
                            if !burst.is_empty() {
                                let out = burst.strip_suffix('\n').unwrap_or(&burst).to_string();
                                yield transform(out).await;
                            }
                            return;
                        }
                        Ok(_) => {
                            burst.push_str(std::str::from_utf8(&line).unwrap_or(""));
                            line.clear();
                            // New data: restart the idle window.
                            idle.as_mut().reset(Instant::now() + debounce);
                        }
                        Err(_) => return,
                    }
                }
                _ = &mut idle => {
                    // Idle for `debounce`: emit the accumulated burst as one event.
                    if !burst.is_empty() {
                        let out = burst.strip_suffix('\n').unwrap_or(&burst).to_string();
                        yield transform(out).await;
                        burst.clear();
                    }
                    idle.as_mut().reset(Instant::now() + debounce);
                }
            }
        }
    };
    Box::pin(s)
}

/// Current process UID (for peer authentication).
fn current_uid() -> u32 {
    unsafe { libc::getuid() }
}

/// Flush any pending input from the terminal's input buffer.
///
/// When a program reading stdin exits (e.g. via Ctrl-C), partial input that
/// was typed before the signal remains in the terminal buffer. The shell reads
/// this leftover data on its next prompt, which suppresses the prompt display
/// until the user presses Enter. Flushing the buffer prevents this.
fn flush_stdin() {
    unsafe { libc::tcflush(libc::STDIN_FILENO, libc::TCIFLUSH) };
}

/// Wait for SIGTERM so the publisher can clean up on `kill` (not just Ctrl-C).
pub async fn sigterm() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut s) = signal(SignalKind::terminate()) {
            s.recv().await;
        }
    }
    #[cfg(not(unix))]
    {
        std::future::pending::<()>().await;
    }
}

/// Bind the publisher's Unix socket, handling stale socket files on macOS.
///
/// On Linux (abstract sockets), `bind` either succeeds or returns EADDRINUSE
/// if another publisher owns the name — there are no stale files to recover.
///
/// On macOS (filesystem paths), `bind` may fail because a previous publisher
/// crashed without cleaning up. We detect this by attempting to connect: if the
/// connection succeeds, another publisher is live (channel in use); if it fails,
/// the socket file is stale and we remove + retry.
async fn bind_socket(path: &Path) -> Result<UnixListener> {
    match UnixListener::bind(path) {
        Ok(listener) => Ok(listener),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            // Stale socket? Try connecting to see if a publisher is live.
            match UnixStream::connect(path).await {
                Ok(_) => bail!("channel already in use (socket is live)"),
                Err(_) => {
                    // Stale — remove and retry
                    let _ = tokio::fs::remove_file(path).await;
                    UnixListener::bind(path)
                        .map_err(|e| anyhow!("failed to bind after stale cleanup: {}", e))
                }
            }
        }
        Err(e) => bail!("failed to bind socket: {}", e),
    }
}

/// A bound channel publisher.
///
/// [`bind`](Publisher::bind) creates the socket and starts listening; the value
/// is then consumed by [`serve`](Publisher::serve) to broadcast events from any
/// stream. Binding is separated from serving so a caller can guarantee the
/// socket is live before spawning the broadcast task — the server binds its
/// default timer channels before respawning loops that connect to them.
pub struct Publisher {
    listener: UnixListener,
    _guard: SocketGuard,
}

impl Publisher {
    /// Bind the channel socket, recovering a stale socket file if present.
    ///
    /// Fails if another live publisher already owns the channel.
    pub async fn bind(storage_path: &str, id: &str) -> Result<Self> {
        let path = socket_path(storage_path, id)?;

        // Ensure channels dir exists with 0o700 perms (macOS filesystem paths).
        #[cfg(not(target_os = "linux"))]
        {
            let channels_dir = PathBuf::from(storage_path).join("channels");
            tokio::fs::create_dir_all(&channels_dir).await?;
            let perms = std::fs::Permissions::from_mode(0o700);
            tokio::fs::set_permissions(&channels_dir, perms).await?;
        }

        let listener = bind_socket(&path).await?;

        // Set 0o700 perms on the socket file (macOS).
        #[cfg(not(target_os = "linux"))]
        {
            let perms = std::fs::Permissions::from_mode(0o700);
            tokio::fs::set_permissions(&path, perms).await?;
        }

        // RAII cleanup guard: removes the socket file on Drop (macOS).
        #[cfg(target_os = "linux")]
        let _guard = SocketGuard::no_file();
        #[cfg(not(target_os = "linux"))]
        let _guard = SocketGuard::new_file(path.clone());

        Ok(Self { listener, _guard })
    }

    /// Broadcast each event from `events` to all connected subscribers as a
    /// newline-delimited string, until the stream ends.
    ///
    /// Accepts subscriber connections, verifying each peer's UID matches our
    /// own. Sends are non-blocking: a slow subscriber never blocks others, and
    /// a lagging subscriber misses events rather than stalling the publisher.
    pub async fn serve(self, mut events: BoxStream<'static, String>) -> Result<()> {
        let Self { listener, _guard } = self;

        // Broadcast channel: non-blocking send, no head-of-line blocking.
        // Lagging subscribers miss events (RecvError::Lagged).
        let (broadcast_tx, _) = broadcast::channel::<String>(128);
        let accept_tx = broadcast_tx.clone();

        // Accept loop (spawned task). For each accepted connection:
        // - Verify peer UID matches our UID (reject if not).
        // - Subscribe to the broadcast channel; spawn a writer task that reads
        //   events and writes `<msg>\n` to the subscriber's stream.
        let our_uid = current_uid();
        let accept_handle = tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        // Peer authentication: verify same UID.
                        match stream.peer_cred() {
                            Ok(creds) if creds.uid() == our_uid => {
                                let mut rx = accept_tx.subscribe();
                                let mut stream = stream;
                                tokio::spawn(async move {
                                    loop {
                                        match rx.recv().await {
                                            Ok(msg) => {
                                                let mut buf = msg.into_bytes();
                                                buf.push(b'\n');
                                                if stream.write_all(&buf).await.is_err() {
                                                    break;
                                                }
                                            }
                                            Err(broadcast::error::RecvError::Lagged(n)) => {
                                                warn!("subscriber lagged by {} messages", n);
                                                continue;
                                            }
                                            Err(broadcast::error::RecvError::Closed) => break,
                                        }
                                    }
                                });
                            }
                            Ok(_) => warn!("rejected subscriber: wrong UID"),
                            Err(e) => warn!("failed to read peer creds: {}", e),
                        }
                    }
                    Err(e) => {
                        warn!("accept error: {e}");
                        continue;
                    }
                }
            }
        });

        // Forward every event to subscribers. broadcast::Sender::send() is
        // non-blocking — a slow subscriber never blocks other subscribers.
        while let Some(event) = events.next().await {
            let _ = broadcast_tx.send(event);
        }

        // Cleanup: stop accept task, drop subscribers (they see EOF).
        accept_handle.abort();

        Ok(())
    }
}

/// Run the channel publisher from stdin.
///
/// Binds a Unix domain socket at `socket_path(id, storage_path)`, accepts
/// subscriber connections (verifying peer UID matches our own), and broadcasts
/// each stdin event to all active subscribers as a newline-delimited string.
pub async fn run(storage_path: &str, id: &str, debounce: Duration) -> Result<()> {
    let publisher = Publisher::bind(storage_path, id).await?;
    let events = event_stream(debounce);

    // Ctrl-C / SIGTERM handling: race the broadcast loop against signals so the
    // socket file is cleaned up via the publisher's guard even on interrupt or
    // `kill` (SIGTERM). SIGKILL cannot be caught — that's the only case where
    // cleanup is skipped (stale socket recovery handles it on next bind).
    tokio::select! {
        result = publisher.serve(events) => result,
        _ = tokio::signal::ctrl_c() => {
            eprintln!("\nShutting down...");
            flush_stdin();
            Ok(())
        }
        _ = sigterm() => {
            eprintln!("\nShutting down (SIGTERM)...");
            flush_stdin();
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tempfile::TempDir;
    use tokio::io::{AsyncRead, ReadBuf};
    use tokio::sync::mpsc;

    /// An `AsyncRead` wrapper over a tokio mpsc receiver of byte chunks, used to
    /// simulate timed bursts of input in tests.
    struct MpscReader {
        rx: mpsc::Receiver<Vec<u8>>,
        current: Vec<u8>,
        pos: usize,
    }

    impl MpscReader {
        fn new(rx: mpsc::Receiver<Vec<u8>>) -> Self {
            Self {
                rx,
                current: Vec::new(),
                pos: 0,
            }
        }
    }

    impl AsyncRead for MpscReader {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            if self.pos < self.current.len() {
                let n = std::cmp::min(buf.remaining(), self.current.len() - self.pos);
                buf.put_slice(&self.current[self.pos..self.pos + n]);
                self.pos += n;
                return Poll::Ready(Ok(()));
            }
            match self.rx.poll_recv(cx) {
                Poll::Ready(Some(chunk)) => {
                    self.current = chunk;
                    self.pos = 0;
                    let n = std::cmp::min(buf.remaining(), self.current.len());
                    buf.put_slice(&self.current[..n]);
                    self.pos = n;
                    Poll::Ready(Ok(()))
                }
                Poll::Ready(None) => Poll::Ready(Ok(())), // EOF
                Poll::Pending => Poll::Pending,
            }
        }
    }

    #[test]
    fn test_validate_channel_id_valid() {
        assert!(validate_channel_id("my-channel").is_ok());
        assert!(validate_channel_id("channel_123").is_ok());
        assert!(validate_channel_id("a").is_ok());
        assert!(validate_channel_id("abc-123_def").is_ok());
    }

    #[test]
    fn test_validate_channel_id_empty() {
        let err = validate_channel_id("").unwrap_err().to_string();
        assert!(err.contains("cannot be empty"), "{err}");
    }

    #[test]
    fn test_validate_channel_id_rejects_special_chars() {
        let cases = [
            "../channel",
            "channel/../foo",
            "channel\x00name",
            "channel space",
            "channel.name",
            "channel@host",
            "",
        ];
        for id in cases {
            if id.is_empty() {
                continue; // tested separately
            }
            assert!(
                validate_channel_id(id).is_err(),
                "expected '{id}' to be rejected"
            );
        }
    }

    #[test]
    fn test_validate_channel_id_rejects_path_traversal_attempts() {
        // These look like they might slip through naive validation
        let attacks = ["../etc", "foo/bar", "a/b/c", "/absolute"];
        for id in attacks {
            assert!(
                validate_channel_id(id).is_err(),
                "expected '{id}' to be rejected"
            );
        }
    }

    #[test]
    fn test_transform_is_identity() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let cases = ["hello", "", "line with spaces", "  trimmed  "];
        for input in cases {
            let output = rt.block_on(transform(input.to_string()));
            assert_eq!(output, input);
        }
    }

    #[test]
    fn test_socket_path_rejects_invalid_id() {
        let err = socket_path("/tmp", "../bad").unwrap_err().to_string();
        assert!(err.contains("alphanumeric"), "{err}");
    }

    #[test]
    fn test_socket_path_constructs_filesystem_path() {
        // On macOS (non-Linux), socket_path returns a filesystem path.
        // On Linux with abstract sockets this test checks different output.
        let path = socket_path("/tmp/storage", "test-chan").unwrap();

        #[cfg(not(target_os = "linux"))]
        {
            let expected = PathBuf::from("/tmp/storage/channels/test-chan.sock");
            assert_eq!(path, expected, "expected {expected:?}, got {path:?}");
        }

        #[cfg(target_os = "linux")]
        {
            let bytes = path.as_os_str().as_encoded_bytes();
            assert_eq!(bytes[0], 0, "abstract socket should start with \\0");
            assert!(
                bytes.windows(b"test-chan".len()).any(|w| w == b"test-chan"),
                "abstract socket path should contain channel ID"
            );
        }
    }

    #[test]
    fn test_socket_guard_removes_file_on_drop() {
        let dir = TempDir::new().unwrap();
        let sock_path = dir.path().join("test.sock");

        // Create a file at the socket path
        std::fs::write(&sock_path, "data").unwrap();
        assert!(sock_path.exists());

        // Guard takes ownership; on drop it removes the file
        {
            let _guard = SocketGuard::new_file(sock_path.clone());
            assert!(sock_path.exists());
        }
        assert!(!sock_path.exists(), "SocketGuard should clean up on drop");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_socket_guard_no_file_does_nothing() {
        // no_file guard should not crash on drop
        let _guard = SocketGuard::no_file();
    }

    #[test]
    fn test_event_stream_coalesces_burst_into_one_event() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let reader = BufReader::new(std::io::Cursor::new(b"line1\nline2\nline3\n".as_slice()));
            let stream = event_stream_from_reader(reader, Duration::from_millis(10));
            let events: Vec<String> = stream.collect().await;
            assert_eq!(events, vec!["line1\nline2\nline3"]);
        });
    }

    #[test]
    fn test_event_stream_single_line_is_one_event() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let reader = BufReader::new(std::io::Cursor::new(b"hello\n".as_slice()));
            let stream = event_stream_from_reader(reader, Duration::from_millis(10));
            let events: Vec<String> = stream.collect().await;
            assert_eq!(events, vec!["hello"]);
        });
    }

    #[test]
    fn test_event_stream_splits_temporally_distinct_bursts() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let (tx, rx) = mpsc::channel(16);
            tokio::spawn(async move {
                tx.send(b"line1\n".to_vec()).await.unwrap();
                tokio::time::sleep(Duration::from_millis(50)).await;
                tx.send(b"line2\n".to_vec()).await.unwrap();
                // dropping tx signals EOF
            });
            let reader = BufReader::new(MpscReader::new(rx));
            let stream = event_stream_from_reader(reader, Duration::from_millis(10));
            let events: Vec<String> = stream.collect().await;
            assert_eq!(events, vec!["line1", "line2"]);
        });
    }
}
