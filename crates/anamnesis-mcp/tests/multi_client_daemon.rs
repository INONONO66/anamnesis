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
    /// Spawn `anamnesis-mcp serve` (launcher mode → ensures/starts the shared
    /// daemon and proxies stdio↔socket) against `db`, with a 1s grace window.
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
        listed["result"]["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    }

    /// Drop our stdin (EOF → launcher exits its proxy → disconnects from the
    /// daemon) and reap the launcher process.
    fn close(self) {
        let Self {
            mut child, stdin, ..
        } = self;
        drop(stdin); // stdin EOF → the proxy's stdin→socket copy finishes → exit.
        let _ = child.wait();
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
        for t in [
            "recall",
            "remember",
            "relate",
            "stats",
            "ingest_conversation",
            "extract_pending",
        ] {
            assert!(
                names.iter().any(|n| n == t),
                "expected tool {t:?} in {names:?}"
            );
        }
    };

    // (1) First launcher: starts (auto-spawns) the daemon, handshakes, lists tools.
    let mut a = Launcher::spawn(&db);
    expect_tools(&a.handshake_and_list_tools());

    // The daemon must have bound exactly one socket in the DB dir.
    assert!(
        wait_until(Duration::from_secs(5), || sock_files(&db_dir).len() == 1),
        "expected exactly one daemon socket after the first launcher, found {:?}",
        sock_files(&db_dir)
    );
    let the_socket = sock_files(&db_dir).into_iter().next().unwrap();

    // (2) Second launcher: connects to the SAME daemon (no second daemon spawns),
    // handshakes, lists tools.
    let mut b = Launcher::spawn(&db);
    expect_tools(&b.handshake_and_list_tools());

    // Still exactly ONE socket → exactly one daemon serving both clients.
    let socks = sock_files(&db_dir);
    assert_eq!(
        socks.len(),
        1,
        "two launchers must share one daemon (one socket), found {socks:?}"
    );
    assert_eq!(socks[0], the_socket, "the socket identity must be stable");

    // (3) Close both clients. With a 1s grace window, the now-idle daemon must
    // exit and unlink its socket within a few seconds.
    a.close();
    b.close();

    assert!(
        wait_until(Duration::from_secs(8), || !the_socket.exists()),
        "daemon did not exit / unlink its socket {the_socket:?} within the grace window"
    );
    assert!(
        sock_files(&db_dir).is_empty(),
        "no daemon socket should remain after grace shutdown, found {:?}",
        sock_files(&db_dir)
    );
}
