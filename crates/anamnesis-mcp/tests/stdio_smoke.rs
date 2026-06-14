//! Spawns `anamnesis-mcp serve` and verifies the MCP handshake + tool list.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

#[test]
fn lists_three_tools_over_stdio() {
    let bin = env!("CARGO_BIN_EXE_anamnesis-mcp");
    let mut child = Command::new(bin)
        .arg("serve")
        .env(
            "ANAMNESIS_DB",
            std::env::temp_dir().join("anamnesis-smoke/memory.db"),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn server");

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    let send = |stdin: &mut std::process::ChildStdin, v: serde_json::Value| {
        let line = serde_json::to_string(&v).unwrap();
        stdin.write_all(line.as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
    };
    let read = |stdout: &mut BufReader<std::process::ChildStdout>| -> serde_json::Value {
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    };

    // initialize
    send(
        &mut stdin,
        serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "smoke", "version": "0" }
            }
        }),
    );
    let init = read(&mut stdout);
    assert_eq!(init["id"], 1, "initialize response: {init}");

    // initialized notification (no id)
    send(
        &mut stdin,
        serde_json::json!({
            "jsonrpc": "2.0", "method": "notifications/initialized"
        }),
    );

    // tools/list
    send(
        &mut stdin,
        serde_json::json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}
        }),
    );
    let listed = read(&mut stdout);
    let names: Vec<String> = listed["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();

    assert!(names.contains(&"recall".to_string()), "tools: {names:?}");
    assert!(names.contains(&"remember".to_string()), "tools: {names:?}");
    assert!(
        names.contains(&"ingest_conversation".to_string()),
        "tools: {names:?}"
    );

    let _ = child.kill();
    // Reap the child so it does not linger as a zombie after kill().
    let _ = child.wait();
}
