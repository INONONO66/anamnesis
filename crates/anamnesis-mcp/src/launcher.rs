//! `ensure_daemon`: bring up (or connect to) the shared daemon for a resolved DB.
//!
//! Every client of the daemon goes through here — the `serve` MCP adapter, the
//! CLI one-shots, and the hook (all via [`crate::client::DaemonClient`]). This
//! module does NO engine work; it only guarantees a connected socket:
//!
//! 1. [`ensure_daemon`] — derive the per-DB socket, try to `connect()`. If the
//!    daemon is up, return the stream. If not, spawn a **fully detached**
//!    `anamnesis-mcp daemon` (new session via `setsid`, null stdio, never waited
//!    on) and retry-connect with a short backoff until it binds.
//!
//! Lock-then-bind in the daemon makes concurrent starts safe: two clients that
//! both miss the daemon each spawn one, the DB lock picks a single winner, the
//! loser exits, and both end up connected to the one survivor.

use std::io;
use std::path::Path;
use std::process::{Command as StdCommand, Stdio};
use std::time::Duration;

use anyhow::{Context, Result};
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
    spawn_daemon_detached(&exe, resolved_db).with_context(|| format!("spawn daemon {exe:?}"))?;

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
fn spawn_daemon_detached(exe: &Path, db: &Path) -> io::Result<()> {
    // Pin the daemon to the launcher's resolved, ABSOLUTE db path so both sides
    // derive the SAME socket — independent of the daemon's inherited CWD/env
    // (a detached setsid child, or a Claude Desktop host with a different CWD,
    // must not re-resolve a project `.anamnesis/` to a different socket).
    let db = std::path::absolute(db).unwrap_or_else(|_| db.to_path_buf());
    spawn_detached(exe, &["daemon".to_owned()], Some(&db))
}

/// Spawn the current executable with detached null stdio.
///
/// `argv` excludes the executable name. The child is not waited on.
pub(crate) fn spawn_detached_current_exe(argv: &[String]) -> io::Result<()> {
    let exe = std::env::current_exe()?;
    spawn_detached(&exe, argv, None)
}

/// Spawn an executable with detached null stdio. `daemon_db`, when present,
/// preserves the daemon launcher's resolved database environment.
#[cfg(unix)]
fn spawn_detached(exe: &Path, argv: &[String], daemon_db: Option<&Path>) -> io::Result<()> {
    use std::os::unix::process::CommandExt;

    let mut cmd = StdCommand::new(exe);
    cmd.args(argv)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(db) = daemon_db {
        cmd.env("ANAMNESIS_DB", db);
    }

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
fn spawn_detached(exe: &Path, argv: &[String], daemon_db: Option<&Path>) -> io::Result<()> {
    let mut cmd = StdCommand::new(exe);
    cmd.args(argv)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(db) = daemon_db {
        cmd.env("ANAMNESIS_DB", db);
    }
    cmd.spawn().map(|_child| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

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
    static DETACHED_CHILD_LOCK: Mutex<()> = Mutex::new(());
    const DETACHED_CHILD_MARKER: &str = "ANAMNESIS_DETACHED_CHILD_MARKER";

    #[test]
    fn detached_child_marker() {
        let Ok(marker) = std::env::var(DETACHED_CHILD_MARKER) else {
            return;
        };
        let null_device = std::fs::metadata("/dev/null")
            .expect("inspect null device")
            .rdev();
        let null_stdio = ["/dev/fd/0", "/dev/fd/1", "/dev/fd/2"].iter().all(|fd| {
            std::fs::metadata(fd)
                .map(|metadata| metadata.rdev() == null_device)
                .unwrap_or(false)
        });
        let args = std::env::args().skip(1).collect::<Vec<_>>().join("\n");
        std::fs::write(marker, format!("{null_stdio}\n{args}"))
            .expect("detached child writes marker");
    }

    #[test]
    fn detached_current_executable_helper_runs_a_null_stdio_marker_and_reports_spawn_failure() {
        let _lock = DETACHED_CHILD_LOCK
            .lock()
            .expect("lock detached child fixture");
        let dir = tempfile::tempdir().expect("create detached child marker directory");
        let marker = dir.path().join("marker");
        let argv = vec![
            "--exact".to_string(),
            "launcher::tests::detached_child_marker".to_string(),
        ];
        unsafe {
            std::env::set_var(DETACHED_CHILD_MARKER, &marker);
        }
        let started = Instant::now();
        let spawned = spawn_detached_current_exe(&argv);
        unsafe {
            std::env::remove_var(DETACHED_CHILD_MARKER);
        }
        spawned.expect("detached fixture spawns");
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "detached spawn must return promptly"
        );

        let deadline = Instant::now() + Duration::from_secs(2);
        while !marker.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let marker_contents =
            std::fs::read_to_string(&marker).expect("detached child writes marker before deadline");
        let mut lines = marker_contents.lines();
        assert_eq!(
            lines.next(),
            Some("true"),
            "child stdio must all be /dev/null"
        );
        assert_eq!(
            lines.collect::<Vec<_>>(),
            argv.iter().map(String::as_str).collect::<Vec<_>>(),
            "detached child must receive the exact argv"
        );

        assert!(
            spawn_detached(
                std::path::Path::new("/definitely-not-anamnesis-executable"),
                &argv,
                None,
            )
            .is_err(),
            "spawn failure must be returned to the caller"
        );
    }
}
