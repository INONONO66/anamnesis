//! The `serve` launcher: a transparent stdio↔socket proxy in front of the shared
//! daemon.
//!
//! What Claude Code spawns is `anamnesis-mcp serve`. Instead of owning the DB,
//! the launcher does the minimum to connect a session to the one shared daemon:
//!
//! 1. [`ensure_daemon`] — derive the per-DB socket, try to `connect()`. If the
//!    daemon is up, return the stream. If not, spawn a **fully detached**
//!    `anamnesis-mcp daemon` (new session via `setsid`, null stdio, never waited
//!    on) and retry-connect with a short backoff until it binds.
//! 2. [`proxy_stdio`] — relay this process's stdin↔socket and socket↔stdout with
//!    two independent copy tasks, exiting as soon as *either* side EOFs. The
//!    launcher does NO engine work: Claude speaks MCP-stdio to it, it relays
//!    bytes to the daemon, which is the real MCP server.
//!
//! Lock-then-bind in the daemon makes concurrent starts safe: two launchers that
//! both miss the daemon each spawn one, the DB lock picks a single winner, the
//! loser exits, and both launchers connect to the one survivor.

use std::io;
use std::path::Path;
use std::process::{Command as StdCommand, Stdio};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;

use crate::daemon::socket_path_for_db;

/// Total time we'll wait for a freshly-spawned daemon to bind its socket before
/// giving up. The backoff schedule below sums to roughly this.
const CONNECT_BACKOFF_MS: &[u64] = &[10, 20, 40, 80, 160, 320, 640, 1000, 1000, 1000];

/// Ensure the shared daemon for `resolved_db` is running and return a connected
/// stream to it.
///
/// Fast path: the daemon is already up → one `connect()` and we're done. Slow
/// path: no daemon → spawn a detached one and retry-connect with backoff. Errors
/// clearly if the daemon never comes up (so the MCP host sees a real failure
/// rather than a silent hang).
pub async fn ensure_daemon(resolved_db: &Path) -> Result<UnixStream> {
    let socket = socket_path_for_db(resolved_db)?;

    // Fast path: an existing daemon is already listening.
    if let Ok(stream) = UnixStream::connect(&socket).await {
        tracing::debug!(socket = %socket.display(), "connected to existing daemon");
        return Ok(stream);
    }

    // No daemon yet — spawn a detached one and retry until its socket is ready.
    let exe = std::env::current_exe().context("resolve current executable for daemon spawn")?;
    tracing::info!(socket = %socket.display(), "no daemon; spawning detached anamnesis-mcp daemon");
    spawn_daemon_detached(&exe).with_context(|| format!("spawn daemon {exe:?}"))?;

    connect_with_retry(&socket).await.with_context(|| {
        format!(
            "daemon socket {} never became ready after spawning a daemon",
            socket.display()
        )
    })
}

/// Connect with bounded exponential-ish backoff until the daemon's socket is
/// ready. `ENOENT` (socket file not created yet) and `ECONNREFUSED` (file exists
/// but the daemon hasn't called `listen()`/`accept()` yet) are both retryable;
/// anything else (e.g. `EPERM`) fails fast.
async fn connect_with_retry(path: &Path) -> io::Result<UnixStream> {
    let mut last_err = None;
    for (i, delay) in CONNECT_BACKOFF_MS.iter().enumerate() {
        match UnixStream::connect(path).await {
            Ok(stream) => return Ok(stream),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                ) =>
            {
                last_err = Some(e);
                if i + 1 < CONNECT_BACKOFF_MS.len() {
                    tokio::time::sleep(Duration::from_millis(*delay)).await;
                }
            }
            Err(e) => return Err(e), // non-retryable
        }
    }
    Err(last_err.unwrap_or_else(|| {
        io::Error::new(io::ErrorKind::TimedOut, "daemon socket never became ready")
    }))
}

/// Spawn a fully detached `anamnesis-mcp daemon` that OUTLIVES the launcher.
///
/// Uses `std::process::Command` (NOT `tokio::process`): we want zero ownership of
/// the child, so we spawn and immediately drop the handle. Dropping a
/// `std::process::Child` neither kills nor waits — the orphan is reparented to
/// PID 1 (init/launchd), which reaps it, so there is no zombie.
///
/// Detachment recipe:
/// - `setsid` in a `pre_exec` hook → new session with no controlling terminal,
///   so the daemon is fully decoupled from the launcher's TTY and immune to
///   `SIGINT`/`SIGHUP` delivered to it.
/// - All three stdio fds → `/dev/null` so the daemon never holds the MCP host's
///   pipe open or writes into a dead pipe after the launcher exits.
#[cfg(unix)]
fn spawn_daemon_detached(exe: &Path) -> io::Result<()> {
    use std::os::unix::process::CommandExt;

    let mut cmd = StdCommand::new(exe);
    cmd.arg("daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    // SAFETY: the closure runs in the child after `fork()` and before `exec()`,
    // so it must be async-signal-safe. `setsid()` qualifies; it allocates
    // nothing and only reads errno via `last_os_error()`. It fails (-1) only if
    // the caller is already a process-group leader, which the post-fork child is
    // not — so in practice it cannot fail here.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }

    // Spawn, then drop the Child handle immediately → we never `wait()` on it.
    cmd.spawn().map(|_child| ())
}

/// Non-unix fallback: spawn detached with null stdio in a new process group.
/// (Anamnesis is unix-only in practice; this keeps the code portable.)
#[cfg(not(unix))]
fn spawn_daemon_detached(exe: &Path) -> io::Result<()> {
    StdCommand::new(exe)
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_child| ())
}

/// Transparent proxy: relay this process's stdin↔socket and socket↔stdout, then
/// exit as soon as EITHER direction finishes (clean EOF on either side).
///
/// We deliberately do NOT use `tokio::io::copy_bidirectional`: it needs a single
/// `AsyncRead + AsyncWrite` object, but our two ends are stdin (read-only) and
/// stdout (write-only) — separate half-duplex handles — and it only returns once
/// BOTH directions shut down, which is the wrong "session over" semantics. Two
/// independent `copy` tasks raced with `select!` give us exit-on-either-EOF.
pub async fn proxy_stdio(stream: UnixStream) -> io::Result<()> {
    let (mut sock_rd, mut sock_wr) = stream.into_split();

    // stdin → socket. On client EOF, half-close the socket's write side so the
    // daemon sees EOF on its read.
    let up = tokio::spawn(async move {
        let mut stdin = tokio::io::stdin();
        let res = tokio::io::copy(&mut stdin, &mut sock_wr).await;
        let _ = sock_wr.shutdown().await;
        res
    });

    // socket → stdout. Flush so buffered bytes hit the host's pipe before we go.
    let down = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        let res = tokio::io::copy(&mut sock_rd, &mut stdout).await;
        let _ = stdout.flush().await;
        res
    });

    // Exit when EITHER side EOFs. The daemon closing its end (`down` completes)
    // is the real "session over" signal; stdin EOF (`up`) means the MCP host went
    // away. We surface a copy error but treat a broken pipe / reset as a normal
    // teardown rather than a hard failure.
    tokio::select! {
        r = down => squash_disconnect(flatten(r))?,
        r = up   => squash_disconnect(flatten(r))?,
    }
    Ok(())
}

/// Flatten a `JoinHandle` result of an inner `io::Result`. A task panic is
/// reported as a generic I/O error (the proxy tasks don't panic in practice).
fn flatten(joined: Result<io::Result<u64>, tokio::task::JoinError>) -> io::Result<u64> {
    match joined {
        Ok(inner) => inner,
        Err(e) => Err(io::Error::other(format!("proxy task panicked: {e}"))),
    }
}

/// A peer hanging up mid-stream (`BrokenPipe`/`ConnectionReset`) is a normal end
/// of session, not a launcher failure — swallow it; surface anything else.
fn squash_disconnect(res: io::Result<u64>) -> io::Result<()> {
    match res {
        Ok(_) => Ok(()),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
            ) =>
        {
            Ok(())
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    use crate::daemon::{acquire_daemon, serve_loop};
    use crate::memory::{MemoryRegistry, StubProvider};

    /// Stand up a real daemon serve loop on a tempdir socket+DB with a STUB
    /// embedding provider (no model download), then assert that TWO
    /// `ensure_daemon` calls against the SAME DB both connect to exactly ONE
    /// daemon: the first connects, and the second connects to the same socket
    /// without ever spawning a second daemon.
    ///
    /// We pre-bind the daemon here (rather than letting `ensure_daemon` spawn it)
    /// so the test never shells out to a real binary or downloads a model — the
    /// "exactly one daemon" guarantee under test is the connect-reuse path: both
    /// launchers hit the single already-listening socket.
    #[tokio::test]
    async fn two_ensure_daemon_calls_share_one_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("memory.db");
        let socket = socket_path_for_db(&db).unwrap();

        // Acquire ownership exactly as the daemon does (lock <db>.lock + bind),
        // then run the shared serve loop with a stub provider in the background.
        let bind = acquire_daemon(&db, &socket)
            .unwrap()
            .expect("first daemon wins the lock");
        let registry = Arc::new(std::sync::Mutex::new(
            MemoryRegistry::file_backed_unlocked_with(
                Arc::new(StubProvider),
                db.clone(),
                dir.path().to_path_buf(),
                "default".to_string(),
                false,
            ),
        ));
        // Long grace window so the daemon stays up for both connects.
        let loop_handle = tokio::spawn(serve_loop(bind, registry, Duration::from_secs(30)));

        // A second `acquire_daemon` on the same DB MUST lose the lock — proving no
        // second daemon could ever bind while ours holds the lock.
        assert!(
            acquire_daemon(&db, &socket).unwrap().is_none(),
            "a second daemon must lose the DB lock — only one daemon per DB"
        );

        // First launcher: connects to the already-listening daemon (fast path).
        let s1 = ensure_daemon(&db)
            .await
            .expect("first ensure_daemon connects");
        // Second launcher: same DB → same socket → connects to the SAME daemon.
        let s2 = ensure_daemon(&db)
            .await
            .expect("second ensure_daemon connects to the same daemon");

        // Both streams are connected to the same peer socket path.
        assert_eq!(
            s1.peer_addr().unwrap().as_pathname(),
            Some(socket.as_path())
        );
        assert_eq!(
            s2.peer_addr().unwrap().as_pathname(),
            Some(socket.as_path())
        );

        // Drop both clients → count returns to zero → grace timer starts. We don't
        // wait out the 30s grace; just confirm the loop is still alive (one daemon)
        // and then abort it to end the test deterministically.
        drop(s1);
        drop(s2);
        assert!(!loop_handle.is_finished(), "the single daemon is still up");
        loop_handle.abort();
    }
}
