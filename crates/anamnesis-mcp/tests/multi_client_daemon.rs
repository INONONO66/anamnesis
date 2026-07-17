//! Multi-client end-to-end test for the on-demand shared daemon.
//!
//! MODEL-FREE by construction: every client only runs `initialize` + `tools/list`
//! (and the `notifications/initialized` ack), never a `tools/call`, so the daemon
//! lists its tools from the static router WITHOUT building the embedding provider
//! — no ~400 MB model download.
//!
//! What it proves end-to-end against the REAL `anamnesis-mcp` binary:
//!   1. TWO `serve` launchers (separate processes) against one tempdir DB both
//!      complete the MCP handshake and `tools/list` over stdio.
//!   2. Exactly ONE daemon backs them: exactly one `*.sock` exists in the DB dir
//!      while both are connected (the launchers are stdio↔socket proxies; the one
//!      daemon owns the DB lock and the socket).
//!   3. After both launchers close, with `ANAMNESIS_DAEMON_GRACE_SECS=1`, the
//!      daemon exits and unlinks its socket within a few seconds.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

/// A spawned `serve` launcher with framed stdio access to the daemon behind it.
struct Launcher {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Launcher {
    /// Spawn the external `serve` adapter, which ensures the shared daemon is
    /// ready and proxies stdio to its socket. This is adapter startup timing, not
    /// daemon last-client/grace timing.
    fn spawn(db: &Path) -> Self {
        let bin = env!("CARGO_BIN_EXE_anamnesis");
        let mut child = Command::new(bin)
            .arg("serve")
            .env("ANAMNESIS_DB", db)
            // Short grace so the post-shutdown assertion is fast and deterministic.
            .env("ANAMNESIS_DAEMON_GRACE_SECS", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn serve launcher");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin,
            stdout,
        }
    }

    fn send(&mut self, v: serde_json::Value) {
        let line = serde_json::to_string(&v).unwrap();
        self.stdin.write_all(line.as_bytes()).unwrap();
        self.stdin.write_all(b"\n").unwrap();
        self.stdin.flush().unwrap();
    }

    fn read(&mut self) -> serde_json::Value {
        let mut line = String::new();
        let n = self.stdout.read_line(&mut line).expect("read line");
        assert!(n > 0, "daemon closed the stream before responding");
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("bad json line {line:?}: {e}"))
    }

    /// Run `initialize` → `notifications/initialized` → `tools/list` and return
    /// the listed tool names. No model is touched.
    fn handshake_and_list_tools(&mut self) -> Vec<String> {
        self.send(serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "e2e", "version": "0" }
            }
        }));
        let init = self.read();
        assert_eq!(init["id"], 1, "initialize response: {init}");

        self.send(serde_json::json!({
            "jsonrpc": "2.0", "method": "notifications/initialized"
        }));

        self.send(serde_json::json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}
        }));
        let listed = self.read();
        assert_eq!(listed["jsonrpc"], "2.0", "tools/list response: {listed}");
        assert_eq!(listed["id"], 2, "tools/list response: {listed}");
        listed["result"]["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    }

    /// Close the external adapter (stdin EOF) and reap it. This disconnects one
    /// daemon client; it is distinct from the daemon's later idle-grace shutdown.
    fn close(self) -> Duration {
        let started = Instant::now();
        let Self {
            mut child, stdin, ..
        } = self;
        drop(stdin); // stdin EOF → the proxy's stdin→socket copy finishes → exit.
        let _ = child.wait();
        started.elapsed()
    }
}

/// Count `*.sock` files directly inside `dir`.
fn sock_files(dir: &Path) -> Vec<PathBuf> {
    let mut socks: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("sock"))
                .collect()
        })
        .unwrap_or_default();
    socks.sort();
    socks
}

/// Poll `cond` until it returns true or `timeout` elapses (10ms steps).
fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    cond()
}

#[test]
fn two_launchers_share_one_daemon_then_grace_exits() {
    let tmp = tempfile::tempdir().unwrap();
    // The DB lives in its own subdir so the only `*.sock` in that dir is the
    // daemon's — no other test artifacts to confuse the count.
    let db_dir = tmp.path().join("anamnesis");
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = db_dir.join("memory.db");

    let expect_tools = |names: &[String]| {
        let mut actual = names.to_vec();
        actual.sort_unstable();
        let expected = vec![
            "extract_pending".to_string(),
            "forget".to_string(),
            "get".to_string(),
            "ingest_conversation".to_string(),
            "list".to_string(),
            "recall".to_string(),
            "relate".to_string(),
            "remember".to_string(),
            "stats".to_string(),
            "supersede".to_string(),
            "update".to_string(),
        ];
        assert_eq!(actual, expected, "unexpected MCP tool inventory");
    };

    // (1) First external adapter startup: readiness includes its MCP handshake,
    // not the daemon's later last-client/grace/unlink phases.
    let first_ready_started = Instant::now();
    let mut a = Launcher::spawn(&db);
    expect_tools(&a.handshake_and_list_tools());
    let first_launcher_ready = first_ready_started.elapsed();

    // The daemon must have bound exactly one socket in the DB dir.
    assert!(
        wait_until(Duration::from_secs(5), || sock_files(&db_dir).len() == 1),
        "first adapter ready in {first_launcher_ready:?}, but expected exactly one daemon socket; found {:?}",
        sock_files(&db_dir)
    );
    let the_socket = sock_files(&db_dir).into_iter().next().unwrap();

    // (2) Second external adapter startup: it connects to the existing daemon.
    let second_ready_started = Instant::now();
    let mut b = Launcher::spawn(&db);
    expect_tools(&b.handshake_and_list_tools());
    let second_launcher_ready = second_ready_started.elapsed();

    // Still exactly ONE socket → exactly one daemon serving both clients.
    let socks = sock_files(&db_dir);
    assert_eq!(
        socks.len(),
        1,
        "two launchers must share one daemon (one socket), found {socks:?}"
    );
    assert_eq!(socks[0], the_socket, "the socket identity must be stable");

    // (3) Close both external adapters and reap them. The daemon's separate
    // last-client/grace/unlink sequence begins only after the final disconnect.
    let first_adapter_close = a.close();
    let second_adapter_close = b.close();

    // STEP 12 timing investigation (2026-07-17, Apple M4 Max, debug build):
    // 20 external runs had post-close unlink min/median/max
    // 2.328/2.353/2.380s. Adapter readiness was 0.745/0.779/0.908s for the
    // spawning adapter and 8/10/12ms for the second; both adapter reaps were
    // <=1ms. Ten in-process daemon runs isolated last-client→grace at
    // 1.003/1.004/1.004s, while connection joins, flush, migration drain,
    // socket unlink, and lock release totaled 0–1ms. A correlated probe put
    // those internal phases at 1.0033s and 0.5ms within a 2.348s external
    // close→unlink interval: the remaining ~1.345s is the external process
    // close→daemon EOF/client-count handoff, not the 10ms filesystem poll,
    // grace timer, or unlink. The 20s bound is retained (not increased) for
    // scheduler-starved CI; timeout failures print every externally observable
    // phase, and daemon logs now emit its internal phases.
    let socket_unlink_started = Instant::now();
    let socket_unlinked = wait_until(Duration::from_secs(20), || !the_socket.exists());
    let post_close_socket_unlink = socket_unlink_started.elapsed();
    assert!(
        socket_unlinked,
        "daemon socket did not unlink within the unchanged 20s bound; first_adapter_ready={first_launcher_ready:?} second_adapter_ready={second_launcher_ready:?} first_adapter_close={first_adapter_close:?} second_adapter_close={second_adapter_close:?} post_close_socket_unlink={post_close_socket_unlink:?}"
    );
    assert!(
        sock_files(&db_dir).is_empty(),
        "no daemon socket should remain after grace shutdown, found {:?}",
        sock_files(&db_dir)
    );
    println!(
        "multi_client_daemon_metrics first_adapter_ready_ms={} second_adapter_ready_ms={} first_adapter_close_ms={} second_adapter_close_ms={} post_close_socket_unlink_ms={}",
        first_launcher_ready.as_millis(),
        second_launcher_ready.as_millis(),
        first_adapter_close.as_millis(),
        second_adapter_close.as_millis(),
        post_close_socket_unlink.as_millis(),
    );
}
