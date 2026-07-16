//! Guards exact Claude and Codex plugin hook wiring.
use serde_json::Value;

fn load(rel: &str) -> Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../");
    let full = std::path::Path::new(path).join(rel);
    serde_json::from_str(&std::fs::read_to_string(full).unwrap()).unwrap()
}

fn only_hook<'a>(hooks: &'a serde_json::Map<String, Value>, event: &str) -> &'a Value {
    let entries = hooks[event]
        .as_array()
        .unwrap_or_else(|| panic!("{event} must have a hook entry"));
    assert_eq!(entries.len(), 1, "{event} must have exactly one hook entry");

    let commands = entries[0]["hooks"]
        .as_array()
        .unwrap_or_else(|| panic!("{event} entry must have hooks"));
    assert_eq!(commands.len(), 1, "{event} must have exactly one hook");
    &commands[0]
}

#[test]
fn claude_hooks_are_exactly_wired() {
    let v = load("plugin/hooks/hooks.json");
    let hooks = v["hooks"].as_object().unwrap();
    assert_eq!(hooks.len(), 5, "Claude must wire exactly five events");

    for (event, argument) in [
        ("SessionStart", "session-start"),
        ("UserPromptSubmit", "user-prompt"),
        ("Stop", "stop"),
        ("PreCompact", "pre-compact"),
        ("SessionEnd", "session-end"),
    ] {
        let entry = &hooks[event][0];
        assert!(
            entry.get("matcher").is_none(),
            "{event} must not set a matcher"
        );

        let hook = only_hook(hooks, event);
        assert_eq!(hook["type"], "command", "{event} must be a command hook");
        assert_eq!(
            hook["command"], "${CLAUDE_PLUGIN_ROOT}/hooks/anamnesis-hook.sh",
            "{event} must use the Claude hook executable"
        );
        assert_eq!(
            hook["args"],
            serde_json::json!([argument]),
            "{event} must pass its exact event argument"
        );
    }
}

#[test]
fn codex_hooks_are_exactly_wired_without_session_end() {
    let v = load("plugin/hooks/codex-hooks.json");
    let hooks = v["hooks"].as_object().unwrap();
    assert_eq!(hooks.len(), 4, "Codex must wire exactly four events");

    for (event, argument, matcher) in [
        ("SessionStart", "session-start", Some("startup|resume")),
        ("UserPromptSubmit", "user-prompt", None),
        ("Stop", "stop", None),
        ("PreCompact", "pre-compact", None),
    ] {
        let entry = &hooks[event][0];
        assert_eq!(
            entry.get("matcher").and_then(Value::as_str),
            matcher,
            "{event} must have its exact matcher"
        );

        let hook = only_hook(hooks, event);
        assert_eq!(hook["type"], "command", "{event} must be a command hook");
        assert_eq!(
            hook["command"],
            format!("${{PLUGIN_ROOT}}/hooks/anamnesis-hook.sh {argument}"),
            "{event} must use the Codex hook command and exact event argument"
        );
    }

    assert!(
        !hooks.contains_key("SessionEnd"),
        "Codex MUST NOT have SessionEnd (#79 strict parser)"
    );
    assert!(
        v.get("description").is_none(),
        "no top-level description (#79)"
    );
}
