//! Guards the plugin hook wiring: capture events present, Codex has NO SessionEnd.
use serde_json::Value;

fn load(rel: &str) -> Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../");
    let full = std::path::Path::new(path).join(rel);
    serde_json::from_str(&std::fs::read_to_string(full).unwrap()).unwrap()
}

#[test]
fn cc_hooks_have_all_capture_events() {
    let v = load("plugin/hooks/hooks.json");
    let hooks = v["hooks"].as_object().unwrap();
    for e in ["SessionStart", "UserPromptSubmit", "Stop", "PreCompact", "SessionEnd"] {
        assert!(hooks.contains_key(e), "CC hooks.json missing {e}");
    }
}

#[test]
fn codex_hooks_have_capture_but_no_session_end() {
    let v = load("plugin/hooks/codex-hooks.json");
    let hooks = v["hooks"].as_object().unwrap();
    for e in ["SessionStart", "UserPromptSubmit", "Stop", "PreCompact"] {
        assert!(hooks.contains_key(e), "codex-hooks.json missing {e}");
    }
    assert!(!hooks.contains_key("SessionEnd"), "Codex MUST NOT have SessionEnd (#79 strict parser)");
    assert!(v.get("description").is_none(), "no top-level description (#79)");
}
