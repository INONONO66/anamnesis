//! On-demand shared daemon: one process owns the DB + graph + model and serves
//! every Claude Code session over a per-DB Unix socket.
//!
//! The daemon binds a Unix socket derived from the *resolved* DB path, holds the
//! single `<db>.lock` (so it is the only process that opens the DB), and runs the
//! rmcp `AnamnesisServer` over the socket — one MCP session per accepted
//! connection, all sharing ONE `MemoryRegistry`. It ref-counts connected clients
//! and grace-shuts-down when the last client leaves.
//!
//! Lock-then-bind is load-bearing: the `<db>.lock` is the single race arbiter
//! (byte-identical to the lock [`crate::memory::MemoryRegistry`] takes), so a
//! second daemon racing on the same DB loses the lock and exits with zero side
//! effects; only the lock holder binds the socket.

use std::hash::{Hash, Hasher};
use std::io;
use std::os::unix::net::UnixListener as StdUnixListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use rmcp::ServiceExt;
use rmcp::service::QuitReason;
use tokio::net::UnixListener;
use tokio::sync::Notify;

use crate::config::Config;
use crate::memory::MemoryRegistry;
use crate::server::AnamnesisServer;

/// Unix `sun_path` is the binding constraint: 104 bytes on macOS/BSD, 108 on
/// Linux — INCLUDING the NUL terminator. Bind to the smaller limit everywhere so
/// a path that works on Linux can't silently fail on macOS. The kernel does NOT
/// truncate; it returns EINVAL ("invalid argument"), the single most confusing
/// failure mode here — hence we check length ourselves and surface a clear error.
const SUN_PATH_MAX: usize = 104;
/// Leave headroom for the trailing NUL.
const SOCKET_PATH_BUDGET: usize = SUN_PATH_MAX - 1;

// ── (1) STABLE, COLLISION-SAFE SOCKET PATH ───────────────────────────────────

/// Derive the daemon's Unix socket path from the *resolved* DB path.
///
/// Identity rule: the socket is the sibling `<db_dir>/<stem>.sock`, so two
/// processes that resolve to the same DB (the project/global resolution in
/// `config.rs`) derive the *same* socket — that's what makes it the daemon's
/// rendezvous point. The DB stem is sanitized like `MemoryRegistry`'s namespaces
/// (alnum/`-`/`_`, lowercased) so it's a safe filename.
///
/// `sun_path` length is the trap. If the natural sibling path fits the 103-byte
/// budget we use it verbatim (human-readable, easy to `ls`/`nc`). If it does NOT,
/// we fall back to a fixed-length name in a short per-user runtime dir,
/// disambiguated by a hash of the *full canonical DB path* — collision-safe
/// because the hash covers the whole path, not just the stem.
pub fn socket_path_for_db(db: &Path) -> Result<PathBuf> {
    // Canonicalize the *directory* (the file may not exist yet) so two spellings
    // of the same DB (`./x/m.db` vs `/abs/x/m.db`, symlinks) hash identically and
    // land on one socket. Fall back to the raw path if the dir doesn't exist yet.
    let canon = canonicalize_dir_of(db);
    let stem = sanitize_stem(
        canon
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("memory"),
    );

    let dir = canon.parent().unwrap_or_else(|| Path::new("."));
    let sibling = dir.join(format!("{stem}.sock"));

    if path_byte_len(&sibling) <= SOCKET_PATH_BUDGET {
        return Ok(sibling);
    }

    // Sibling is too long for sun_path → short runtime dir + hashed name.
    // `<runtime>/anamnesis-<stem-prefix>-<hash16>.sock` is always short.
    let runtime = runtime_dir()?;
    let h = short_hash(&canon);
    // Keep a few human-recognizable stem chars before the hash, then hard-trim.
    let mut prefix: String = stem.chars().take(16).collect();
    if prefix.is_empty() {
        prefix.push_str("db");
    }
    let name = format!("anamnesis-{prefix}-{h}.sock");
    let hashed = runtime.join(&name);

    if path_byte_len(&hashed) > SOCKET_PATH_BUDGET {
        // Pathological runtime dir; last resort is /tmp which is guaranteed short.
        let tmp = std::env::temp_dir().join(format!("anamnesis-{h}.sock"));
        if path_byte_len(&tmp) > SOCKET_PATH_BUDGET {
            bail!(
                "cannot derive a Unix socket path under {} bytes for DB {:?}; \
                 set ANAMNESIS_SOCKET to a short path (e.g. /tmp/a.sock)",
                SUN_PATH_MAX,
                db
            );
        }
        return Ok(tmp);
    }
    Ok(hashed)
}

/// Byte length of the path as the kernel sees it (OS string is the source of
/// truth — never `.len()` on the lossy `display()` string).
fn path_byte_len(p: &Path) -> usize {
    use std::os::unix::ffi::OsStrExt;
    p.as_os_str().as_bytes().len()
}

/// Same sanitization contract as `MemoryRegistry::sanitize`: alnum / `-` / `_`,
/// lowercased, non-empty.
fn sanitize_stem(s: &str) -> String {
    let out: String = s
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if out.is_empty() {
        "memory".to_string()
    } else {
        out.to_lowercase()
    }
}

/// 16 hex chars of SipHash over the canonical DB path. Not cryptographic — we
/// only need within-machine collision resistance for a socket *name*, and this
/// needs no extra crate. Stable for a given path within one std version, which is
/// all the daemon lifecycle requires.
fn short_hash(p: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    p.as_os_str().as_bytes().hash(&mut h);
    format!("{:016x}", h.finish())
}

fn canonicalize_dir_of(db: &Path) -> PathBuf {
    let dir = match db.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let file = db.file_name().map(PathBuf::from).unwrap_or_default();
    match dir.canonicalize() {
        Ok(c) => c.join(file),
        Err(_) => db.to_path_buf(),
    }
}

/// A short, stable, per-user runtime dir for the hashed-name fallback:
/// `$XDG_RUNTIME_DIR` if set (Linux), else `$TMPDIR`/`/tmp`. Created if absent.
fn runtime_dir() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&base).with_context(|| format!("create runtime dir {base:?}"))?;
    Ok(base)
}

// ── (2)+(3) ATOMIC RACE RESOLUTION + STALE-SOCKET RECLAIM ────────────────────

/// What a successful daemon bind owns: the held exclusive lock file (must stay
/// alive for the process lifetime — drop = unlock) and the bound listener.
pub struct DaemonBind {
    pub listener: UnixListener,
    pub socket_path: PathBuf,
    /// Held for the process lifetime. The OS releases it on exit/crash; we also
    /// release explicitly on graceful shutdown.
    lock_file: std::fs::File,
    lock_path: PathBuf,
}

/// Acquire daemon ownership atomically.
///
/// THE RACE: two daemons start at once on the same DB. The fs4 exclusive lock on
/// `<db>.lock` is the single source of truth for "who is the daemon" — exactly
/// the lock `MemoryRegistry::open_namespace` already takes. We acquire the lock
/// FIRST: whoever wins owns the socket; the loser sees the lock held and exits
/// immediately (no socket games, no TOCTOU). Only the lock holder then binds the
/// listener, so the socket and the lock can never disagree.
///
/// Order is load-bearing: lock → (reclaim stale socket) → bind. If we bound first
/// we could unlink a *live* peer's socket during reclaim.
///
/// `Ok(None)` ⇒ we lost the race; the caller should exit 0 immediately.
pub fn acquire_daemon(db: &Path, socket_path: &Path) -> Result<Option<DaemonBind>> {
    // <db>.lock — byte-identical to memory.rs so the daemon and an embedded
    // registry contend on the SAME lock file.
    let mut lock_os = db.to_path_buf().into_os_string();
    lock_os.push(".lock");
    let lock_path = PathBuf::from(lock_os);

    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create db dir for lock {lock_path:?}"))?;
    }

    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("open lock file {lock_path:?}"))?;

    // UFCS to fs4's `try_lock` (exclusive) — same note as memory.rs/cli.rs: on
    // rustc >= 1.89 `File` grows an inherent `try_lock` with a different signature
    // that would shadow the trait method. Pin to fs4's for identical behavior on
    // our 1.88 MSRV and for byte-identical contention with `MemoryRegistry`.
    if fs4::FileExt::try_lock(&lock_file).is_err() {
        // Lock already held → another daemon owns this DB. Exit immediately.
        return Ok(None);
    }

    // We hold the lock; we are THE daemon. Bind, reclaiming a stale socket if the
    // previous daemon died without unlinking (lock released by the OS, but the
    // socket inode lingers).
    let listener = bind_reclaiming_stale(socket_path)
        .with_context(|| format!("bind daemon socket {socket_path:?}"))?;

    Ok(Some(DaemonBind {
        listener,
        socket_path: socket_path.to_path_buf(),
        lock_file,
        lock_path,
    }))
}

/// Bind the listener; on `AddrInUse`, decide stale-vs-live by *connecting*.
///
/// We already hold the exclusive lock here, so if the socket exists it MUST be
/// stale (a live daemon would hold the lock and we'd never have reached this
/// point). We still probe with `connect()` — defensive, and the canonical Unix
/// idiom — then unlink + rebind. ECONNREFUSED / ENOENT ⇒ no one is listening ⇒
/// safe to reclaim.
fn bind_reclaiming_stale(path: &Path) -> io::Result<UnixListener> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match StdUnixListener::bind(path) {
        Ok(l) => from_std(l),
        Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
            // Is anybody actually listening?
            match std::os::unix::net::UnixStream::connect(path) {
                Ok(_) => {
                    // Someone is alive on it. With the lock held this should be
                    // impossible; treat it as a hard error rather than unlinking a
                    // live peer's socket.
                    Err(io::Error::new(
                        io::ErrorKind::AddrInUse,
                        format!(
                            "socket {path:?} is live but we hold the DB lock — refusing to reclaim"
                        ),
                    ))
                }
                Err(ce)
                    if ce.kind() == io::ErrorKind::ConnectionRefused
                        || ce.kind() == io::ErrorKind::NotFound =>
                {
                    // Stale: previous daemon gone. Unlink + rebind.
                    std::fs::remove_file(path)?;
                    from_std(StdUnixListener::bind(path)?)
                }
                Err(ce) => Err(ce),
            }
        }
        Err(e) => Err(e),
    }
}

/// Promote a std listener to a tokio one (must be non-blocking for the runtime).
fn from_std(l: StdUnixListener) -> io::Result<UnixListener> {
    l.set_nonblocking(true)?;
    UnixListener::from_std(l)
}

impl DaemonBind {
    /// Release ownership in the correct order on graceful exit:
    ///   1) caller flushes memory (`registry.flush_all_open`) BEFORE this,
    ///   2) unlink the socket so the next daemon binds fresh (no stale reclaim),
    ///   3) release the fs4 lock LAST, so no second daemon can bind the socket
    ///      until ours is gone — preserving the lock⇒socket invariant on the way
    ///      out just as `acquire_daemon` preserved it on the way in.
    ///
    /// `process::exit` skips `Drop`, so on the signal path call this explicitly
    /// (mirrors how `main.rs` already flushes on SIGTERM).
    pub fn release(self) {
        // 2) unlink socket (best-effort; a concurrent reclaim handles a miss).
        if let Err(e) = std::fs::remove_file(&self.socket_path)
            && e.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!("unlink socket {:?} failed: {e}", self.socket_path);
        }
        // 3) release lock last.
        let _ = fs4::FileExt::unlock(&self.lock_file);
        // lock_file drops here → fd closed; lock already released.
        let _ = &self.lock_path; // kept for diagnostics if extended.
    }
}

// ── (4) GRACE SHUTDOWN: zero-client idle timeout, cancelable by a new connect ─

/// Shared connection accounting. Every accepted client bumps `count`; on
/// disconnect it decrements and, if it hit zero, pokes `idle` so the grace timer
/// (re)starts. A *new* connection bumps `count` back above zero AND pokes `idle`,
/// which the grace loop observes to cancel a pending shutdown.
///
/// `Notify` is the right primitive: we only need an edge-triggered "state may
/// have changed, re-check the count" wakeup, and `Notify::notify_one` coalesces
/// — no value to carry, no missed-update window because `notified()` futures
/// created before a `notify_*` still fire.
pub struct ClientTracker {
    count: AtomicUsize,
    idle: Notify,
}

impl ClientTracker {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            count: AtomicUsize::new(0),
            idle: Notify::new(),
        })
    }

    /// RAII guard returned on accept; decrements on drop. Use this instead of
    /// manual inc/dec so a panicking handler can't leak the count and pin the
    /// daemon alive forever.
    pub fn connect(self: &Arc<Self>) -> ClientGuard {
        self.count.fetch_add(1, Ordering::SeqCst);
        // A fresh connection is exactly the cancel signal for a pending grace
        // window — wake the loop so it re-reads the (now non-zero) count.
        self.idle.notify_one();
        ClientGuard(self.clone())
    }

    fn current(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }
}

pub struct ClientGuard(Arc<ClientTracker>);

impl Drop for ClientGuard {
    fn drop(&mut self) {
        // If this was the last client, wake the grace loop to start the timer.
        if self.0.count.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.0.idle.notify_one();
        }
    }
}

/// Resolves when the daemon has been idle (0 clients) for `grace`, with no new
/// connection arriving in that window. Callers race it against the accept loop in
/// `tokio::select!`.
///
/// The loop is a classic "wait, then re-validate" guard against spurious/stale
/// wakeups: when notified we re-read the live atomic count rather than trusting
/// the wakeup, so a connect→disconnect flurry can't trip a false shutdown.
pub async fn wait_for_idle_grace(tracker: Arc<ClientTracker>, grace: Duration) {
    loop {
        // Arm the notification BEFORE checking the count: a `notify_one` racing
        // between the check and the await is preserved by tokio's Notify (the
        // permit is stored), so we never miss a "client arrived/left" edge.
        let notified = tracker.idle.notified();

        if tracker.current() > 0 {
            // Busy: block until activity changes (a client leaves → maybe zero).
            notified.await;
            continue;
        }

        // Idle right now. Start the grace window, cancelable by any notify
        // (a new connect calls notify_one).
        tokio::select! {
            _ = notified => {
                // Activity within the window → re-evaluate (cancels shutdown).
                continue;
            }
            _ = tokio::time::sleep(grace) => {
                // Survived the full window. Re-check under the same arm-before-
                // check discipline: only exit if STILL zero.
                if tracker.current() == 0 {
                    return;
                }
                // A client slipped in at the exact boundary; loop and re-arm.
            }
        }
    }
}

/// `ANAMNESIS_DAEMON_GRACE_SECS` (default 30). `0` ⇒ exit as soon as the last
/// client leaves (no lingering); parse failures fall back to the default.
pub fn grace_duration() -> Duration {
    let secs = std::env::var("ANAMNESIS_DAEMON_GRACE_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(30);
    Duration::from_secs(secs)
}

// ── SERVE LOOP ───────────────────────────────────────────────────────────────

/// Entry point for the `daemon` subcommand. Resolves the socket from the DB,
/// acquires ownership (lock + bind), builds ONE shared registry/server, and runs
/// the accept loop until a signal or the idle-grace window expires.
///
/// Returns `Ok(())` immediately (race loser) if another daemon already owns the
/// DB. On graceful shutdown it flushes memory, unlinks the socket, and releases
/// the lock — in that order — and returns `Ok(())`.
pub async fn run(cfg: Config) -> Result<()> {
    let socket = socket_path_for_db(&cfg.default_db)?;
    let Some(bind) = acquire_daemon(&cfg.default_db, &socket)? else {
        tracing::info!(
            "another anamnesis daemon already owns {:?}; exiting",
            cfg.default_db
        );
        return Ok(()); // RACE loser: exit immediately, zero side effects.
    };
    tracing::info!(socket = %bind.socket_path.display(), "anamnesis-mcp daemon serving");

    // The daemon holds the DB lock and is the sole opener → unlocked registry.
    let registry =
        std::sync::Arc::new(std::sync::Mutex::new(MemoryRegistry::file_backed_unlocked(
            cfg.default_db.clone(),
            cfg.db_dir(),
            cfg.default_namespace.clone(),
            cfg.reinforce_on_recall,
        )));

    serve_loop(bind, registry, grace_duration()).await;
    Ok(())
}

/// The shared accept/grace/shutdown loop, parameterized over an already-acquired
/// [`DaemonBind`] and a pre-built registry so tests can drive it with a stub
/// embedding provider (no model download).
///
/// Runs until a shutdown signal or the idle-grace window expires, then flushes
/// memory, unlinks the socket, and releases the lock — in that order.
pub(crate) async fn serve_loop(
    bind: DaemonBind,
    registry: std::sync::Arc<std::sync::Mutex<MemoryRegistry>>,
    grace: Duration,
) {
    let server = AnamnesisServer::new(registry.clone());
    let tracker = ClientTracker::new();

    let shutdown = async {
        tokio::select! {
            _ = crate::shutdown_signal() => tracing::info!("shutdown signal received"),
            _ = wait_for_idle_grace(tracker.clone(), grace) => {
                tracing::info!("idle for {grace:?} with no clients; shutting down");
            }
        }
    };
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            accepted = bind.listener.accept() => {
                let (stream, _addr) = match accepted {
                    Ok(pair) => pair,
                    // Transient accept errors (e.g. EMFILE) must not kill the
                    // daemon — log and keep serving.
                    Err(e) => {
                        tracing::warn!("accept failed: {e}");
                        continue;
                    }
                };
                // Bump the count (and cancel any pending grace) BEFORE spawning,
                // so a connect racing the grace window is never missed.
                let guard = tracker.connect();
                let server = server.clone(); // cheap Arc clone → SHARED registry.
                tokio::spawn(async move {
                    let _g = guard; // drop on task end → decrement.
                    serve_connection(server, stream).await;
                });
            }
            _ = &mut shutdown => break,
        }
    }

    // `process::exit` is skipped here (we return normally), but flush + release in
    // the correct order regardless: flush FIRST, then unlink socket, then unlock.
    if let Ok(mut g) = registry.lock()
        && let Err(e) = g.flush_all_open()
    {
        tracing::error!("flush on shutdown failed: {e}");
    }
    bind.release(); // unlink socket + release lock
}

/// Run one MCP session over an accepted `UnixStream`. The `UnixStream` is a valid
/// rmcp transport via the `transport-async-rw` blanket impl (newline-delimited
/// JSON-RPC, identical framing to stdio). `waiting()` resolves when the peer
/// disconnects (transport receive yields None → `QuitReason::Closed`).
async fn serve_connection(server: AnamnesisServer, stream: tokio::net::UnixStream) {
    match server.serve(stream).await {
        Ok(running) => match running.waiting().await {
            Ok(QuitReason::Closed) => tracing::debug!("client disconnected"),
            Ok(QuitReason::Cancelled) => tracing::debug!("connection cancelled"),
            Ok(other) => tracing::debug!(?other, "connection ended"),
            Err(e) => tracing::error!("serve loop join error: {e}"),
        },
        Err(e) => tracing::error!("initialize handshake failed: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_path_is_sibling_of_db() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("memory.db");
        let sock = socket_path_for_db(&db).unwrap();
        // Sibling of the (canonicalized) DB dir with a sane stem. We canonicalize
        // here too because `socket_path_for_db` canonicalizes the dir (on macOS
        // `/var` resolves to `/private/var`).
        assert_eq!(
            sock.parent(),
            Some(dir.path().canonicalize().unwrap().as_path())
        );
        assert_eq!(
            sock.file_name().and_then(|s| s.to_str()),
            Some("memory.sock")
        );
    }

    #[test]
    fn socket_path_is_deterministic_for_same_db() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("memory.db");
        assert_eq!(
            socket_path_for_db(&db).unwrap(),
            socket_path_for_db(&db).unwrap()
        );
    }

    #[test]
    fn overlong_db_path_falls_back_to_short_hashed_socket() {
        // A deep path whose sibling .sock would blow past sun_path (104).
        let deep = PathBuf::from(format!("/tmp/{}/memory.db", "x".repeat(200)));
        let sock = socket_path_for_db(&deep).unwrap();
        assert!(
            path_byte_len(&sock) <= SOCKET_PATH_BUDGET,
            "socket path {sock:?} exceeds the sun_path budget"
        );
    }

    // `acquire_daemon` promotes the std listener to a tokio one, which needs a
    // reactor — run on the tokio test runtime.
    #[tokio::test]
    async fn second_acquire_on_same_db_loses_the_race() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("m.db");
        let sock = dir.path().join("m.sock");
        let first = acquire_daemon(&db, &sock).unwrap();
        assert!(first.is_some(), "first daemon should win the lock");
        // Second contends on the same <db>.lock and must lose.
        let second = acquire_daemon(&db, &sock).unwrap();
        assert!(second.is_none(), "second daemon must lose the race");
        // Releasing the first lets a fresh daemon acquire again.
        first.unwrap().release();
        let third = acquire_daemon(&db, &sock).unwrap();
        assert!(third.is_some(), "after release a new daemon can acquire");
        third.unwrap().release();
    }

    #[test]
    fn grace_duration_reads_env_with_default() {
        // We don't mutate process env here (parallel tests); just assert the
        // default applies when the var is absent in this process.
        if std::env::var_os("ANAMNESIS_DAEMON_GRACE_SECS").is_none() {
            assert_eq!(grace_duration(), Duration::from_secs(30));
        }
    }

    #[tokio::test]
    async fn idle_grace_returns_after_window_when_zero_clients() {
        let tracker = ClientTracker::new();
        let start = std::time::Instant::now();
        wait_for_idle_grace(tracker, Duration::from_millis(50)).await;
        assert!(start.elapsed() >= Duration::from_millis(50));
    }

    #[tokio::test]
    async fn idle_grace_is_canceled_by_a_new_connection() {
        let tracker = ClientTracker::new();
        // Hold a client for longer than the grace window; the grace future must
        // NOT resolve while a client is connected.
        let _guard = tracker.connect();
        let res = tokio::time::timeout(
            Duration::from_millis(150),
            wait_for_idle_grace(tracker.clone(), Duration::from_millis(30)),
        )
        .await;
        assert!(
            res.is_err(),
            "grace must not fire while a client is connected"
        );
    }

    /// End-to-end daemon lifecycle on a tempdir socket+DB with a STUB embedding
    /// provider (no bge download): connect a client, run initialize + tools/list,
    /// disconnect, then with a 1s grace window assert the daemon exits and the
    /// socket file is gone within a few seconds.
    #[tokio::test]
    async fn daemon_serves_then_grace_exits_and_unlinks_socket() {
        use rmcp::ServiceExt;

        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("memory.db");
        let socket = socket_path_for_db(&db).unwrap();

        // Acquire ownership exactly as the daemon does (lock <db>.lock + bind).
        let bind = acquire_daemon(&db, &socket)
            .unwrap()
            .expect("first daemon wins the lock");
        let bound_socket = bind.socket_path.clone();
        assert!(bound_socket.exists(), "socket should exist after bind");

        // Unlocked, file-backed registry with a stub provider — the daemon holds
        // the lock and is the sole opener, and we never touch the real model.
        let registry = std::sync::Arc::new(std::sync::Mutex::new(
            MemoryRegistry::file_backed_unlocked_with(
                Arc::new(crate::memory::StubProvider),
                db.clone(),
                dir.path().to_path_buf(),
                "default".to_string(),
                false,
            ),
        ));

        // Run the serve loop with a 1s grace window.
        let loop_handle = tokio::spawn(serve_loop(bind, registry, Duration::from_secs(1)));

        // Connect a client over the unix socket and drive a real MCP session.
        let stream = tokio::net::UnixStream::connect(&bound_socket)
            .await
            .expect("client connects to the daemon socket");
        let client = ().serve(stream).await.expect("MCP initialize handshake");

        // tools/list must surface the four anamnesis tools.
        let tools = client.peer().list_all_tools().await.expect("tools/list");
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
        assert!(
            names.contains(&"recall")
                && names.contains(&"remember")
                && names.contains(&"relate")
                && names.contains(&"ingest_conversation"),
            "expected the anamnesis tools, got {names:?}"
        );

        // Disconnect → the daemon's per-connection serve resolves, count → 0,
        // and the 1s grace window starts.
        client.cancel().await.expect("client disconnect");

        // The serve loop must exit within the grace window plus slack, and the
        // socket file must be unlinked on the way out.
        let exited = tokio::time::timeout(Duration::from_secs(5), loop_handle).await;
        assert!(
            exited.is_ok(),
            "daemon did not exit within the grace window after the last client left"
        );
        exited.unwrap().expect("serve loop task panicked");
        assert!(
            !bound_socket.exists(),
            "socket file {bound_socket:?} must be unlinked on graceful shutdown"
        );
    }
}
