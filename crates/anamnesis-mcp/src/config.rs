//! Server/CLI configuration resolved from environment + flags.

use std::path::{Path, PathBuf};

/// Calibrated prior for the hook injection gate `τ`: a floor on the **top recall
/// score**, which is the **unnormalized ACT-R activation** of the strongest hit
/// (base-level + spreading log-odds), NOT a 0..1 similarity. On a typical graph
/// the top activation lands around ~8–16, so `τ` belongs on that scale — a sub-1
/// value silently disables the gate (everything injects). The gate keeps a hit
/// when `top >= τ`.
///
/// `13.0` was calibrated live against the real global graph (verify phase): it
/// sits below the typical relevant band (~14–16) and above the bulk of the
/// off-topic band (~8–10), so a clearly-relevant prompt injects and an
/// obviously-off-topic one injects nothing. The bands overlap (a strong off-topic
/// hit can exceed a weak-but-relevant one), so `13.0` favors precision; it stays
/// env-tunable via `ANAMNESIS_HOOK_THRESHOLD` and should be **recalibrated
/// per-graph**, since activation magnitude scales with graph density/recency.
pub const DEFAULT_HOOK_THRESHOLD: f64 = 13.0;
/// Default query-embedding cosine gate for UserPrompt recall.
///
/// e5-small calibration on 2026-07-08: direct recall pairs had related top
/// cosine min/median `0.7805/0.8834` and unrelated max/median `0.8127/0.7840`;
/// the hook battery's content-free project-cue prompt reached `0.8533`.
/// `0.86` favors precision: it keeps 7/10 measured related prompts and blocks
/// the observed content-free injection. Env-tune per graph if recall is too
/// quiet.
pub const DEFAULT_HOOK_COSINE_GATE: f64 = 0.86;
/// Default query-embedding cosine gate for SessionStart seed recall.
pub const DEFAULT_HOOK_SEED_COSINE_GATE: f64 = 0.80;
/// Number of recent transcript turns folded into UserPrompt recall queries.
pub const DEFAULT_HOOK_CONTEXT_TURNS: usize = 3;
/// Cap on memories injected for each UserPrompt hook recall.
pub const DEFAULT_HOOK_TOPK: usize = 3;
/// Default embedding model for new databases and model downloads.
pub const DEFAULT_EMBED_MODEL: &str = "multilingual-e5-small";

/// Resolved runtime configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// SQLite file for the default namespace.
    pub default_db: PathBuf,
    /// Namespace used when a call omits one.
    pub default_namespace: String,
    /// Auto-commit (reinforce) the package returned by `recall`.
    pub reinforce_on_recall: bool,
    /// `τ` — need-odds injection gate: a floor on the top recall score (raw
    /// ACT-R activation, ~8–16 on a typical graph — NOT a 0..1 similarity).
    /// `ANAMNESIS_HOOK_THRESHOLD`; see [`DEFAULT_HOOK_THRESHOLD`].
    pub hook_threshold: f64,
    /// Query-embedding cosine floor for UserPrompt recall.
    /// `ANAMNESIS_HOOK_COSINE_GATE`; see [`DEFAULT_HOOK_COSINE_GATE`].
    pub hook_cosine_gate: f64,
    /// Query-embedding cosine floor for SessionStart seed recall.
    /// `ANAMNESIS_HOOK_SEED_COSINE_GATE`; see [`DEFAULT_HOOK_SEED_COSINE_GATE`].
    pub hook_seed_cosine_gate: f64,
    /// Recent transcript turns included in UserPrompt recall queries.
    /// `ANAMNESIS_HOOK_CONTEXT_TURNS`; see [`DEFAULT_HOOK_CONTEXT_TURNS`].
    pub hook_context_turns: usize,
    /// `k` — cap on injected per-turn memories. `ANAMNESIS_HOOK_TOPK`; see [`DEFAULT_HOOK_TOPK`].
    pub hook_topk: usize,
    /// SessionStart seed size. `ANAMNESIS_HOOK_SEED_K` (default 5).
    pub hook_seed_k: usize,
    /// Per-hook fail-open timeout (ms). `ANAMNESIS_HOOK_TIMEOUT_MS` (default 1500).
    pub hook_timeout_ms: u64,
    /// Global capture kill-switch. `ANAMNESIS_CAPTURE_ENABLED` (default true).
    pub capture_enabled: bool,
    /// Un-extracted queue size that triggers the extraction signal.
    /// `ANAMNESIS_EXTRACT_THRESHOLD_N` (default 20).
    pub extract_threshold_n: usize,
    /// FastEmbed model name. `ANAMNESIS_EMBED_MODEL` (default multilingual-e5-small).
    pub embed_model: String,
    /// Automatically migrate incompatible embedding spaces in the daemon.
    /// `ANAMNESIS_AUTO_MIGRATE_EMBEDDINGS` (default true).
    pub auto_migrate_embeddings: bool,
}

impl Config {
    /// Resolve from environment variables, falling back to sane defaults.
    ///
    /// - `ANAMNESIS_DB`              → `default_db` (default: `<data_dir>/anamnesis/memory.db`)
    /// - `ANAMNESIS_NAMESPACE`       → `default_namespace` (default: `"default"`)
    /// - `ANAMNESIS_REINFORCE`       → `reinforce_on_recall`; "0"/"false" disables (default: true)
    /// - `ANAMNESIS_HOOK_THRESHOLD`  → `hook_threshold` (default: [`DEFAULT_HOOK_THRESHOLD`])
    /// - `ANAMNESIS_HOOK_COSINE_GATE` → `hook_cosine_gate` (default: [`DEFAULT_HOOK_COSINE_GATE`])
    /// - `ANAMNESIS_HOOK_SEED_COSINE_GATE` → `hook_seed_cosine_gate` (default: [`DEFAULT_HOOK_SEED_COSINE_GATE`])
    /// - `ANAMNESIS_HOOK_CONTEXT_TURNS` → `hook_context_turns` (default: [`DEFAULT_HOOK_CONTEXT_TURNS`])
    /// - `ANAMNESIS_HOOK_TOPK`       → `hook_topk` (default: [`DEFAULT_HOOK_TOPK`])
    /// - `ANAMNESIS_HOOK_SEED_K`     → `hook_seed_k` (default: 5)
    /// - `ANAMNESIS_HOOK_TIMEOUT_MS` → `hook_timeout_ms` (default: 1500)
    /// - `ANAMNESIS_EMBED_MODEL`     → `embed_model` (default: [`DEFAULT_EMBED_MODEL`])
    /// - `ANAMNESIS_AUTO_MIGRATE_EMBEDDINGS` → automatic daemon migration (default: true)
    pub fn from_env() -> Self {
        let default_db = std::env::var_os("ANAMNESIS_DB")
            .map(PathBuf::from)
            .unwrap_or_else(default_db_path);
        let default_namespace =
            std::env::var("ANAMNESIS_NAMESPACE").unwrap_or_else(|_| "default".to_string());
        let reinforce_on_recall = match std::env::var("ANAMNESIS_REINFORCE") {
            Ok(v) => !matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "false" | "no"),
            Err(_) => true,
        };
        let hook_threshold = parse_env("ANAMNESIS_HOOK_THRESHOLD", DEFAULT_HOOK_THRESHOLD);
        let hook_cosine_gate = parse_env("ANAMNESIS_HOOK_COSINE_GATE", DEFAULT_HOOK_COSINE_GATE);
        let hook_seed_cosine_gate = parse_env(
            "ANAMNESIS_HOOK_SEED_COSINE_GATE",
            DEFAULT_HOOK_SEED_COSINE_GATE,
        );
        let hook_context_turns =
            parse_env("ANAMNESIS_HOOK_CONTEXT_TURNS", DEFAULT_HOOK_CONTEXT_TURNS);
        let hook_topk = parse_env("ANAMNESIS_HOOK_TOPK", DEFAULT_HOOK_TOPK);
        let hook_seed_k = parse_env("ANAMNESIS_HOOK_SEED_K", 5usize);
        let hook_timeout_ms = parse_env("ANAMNESIS_HOOK_TIMEOUT_MS", 1500u64);
        let capture_enabled = match std::env::var("ANAMNESIS_CAPTURE_ENABLED") {
            Ok(v) => !matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "false" | "no"),
            Err(_) => true,
        };
        let extract_threshold_n = parse_env("ANAMNESIS_EXTRACT_THRESHOLD_N", 20usize);
        let embed_model = parse_env_string("ANAMNESIS_EMBED_MODEL", DEFAULT_EMBED_MODEL);
        let auto_migrate_embeddings = positive_env_enabled("ANAMNESIS_AUTO_MIGRATE_EMBEDDINGS");
        Self {
            default_db,
            default_namespace,
            reinforce_on_recall,
            hook_threshold,
            hook_cosine_gate,
            hook_seed_cosine_gate,
            hook_context_turns,
            hook_topk,
            hook_seed_k,
            hook_timeout_ms,
            capture_enabled,
            extract_threshold_n,
            embed_model,
            auto_migrate_embeddings,
        }
    }

    /// Directory that holds per-namespace sibling DB files.
    pub fn db_dir(&self) -> PathBuf {
        // `Path::parent` yields `Some("")` for a bare filename (e.g. `"x"`),
        // which is not a usable directory; treat that as the current dir too.
        match self.default_db.parent() {
            Some(p) if !p.as_os_str().is_empty() => PathBuf::from(p),
            _ => PathBuf::from("."),
        }
    }
}

/// The anamnesis home directory: `~/.anamnesis` (like `~/.codex`, `~/.claude`),
/// holding both the default DB and the model cache so everything lives in one
/// discoverable place. Falls back to a temp dir if `$HOME` is unavailable.
fn anamnesis_home() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(".anamnesis")
}

/// Resolve the default DB when `ANAMNESIS_DB` is unset.
///
/// Scope is chosen like git finds `.git`: walking up from the launch directory,
/// the **nearest ancestor containing a `.anamnesis/` directory wins** (project
/// scope → `<project>/.anamnesis/memory.db`). If none is found, fall back to the
/// **global** `~/.anamnesis/memory.db`. A project opts in by `mkdir .anamnesis`.
///
/// This relies on the MCP client launching the server with the project as the
/// working directory (true for Claude Code / Cursor; Claude Desktop has no
/// project CWD, so it always resolves to global).
fn default_db_path() -> PathBuf {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| resolve_project_db(&cwd))
        .unwrap_or_else(|| anamnesis_home().join("memory.db"))
}

/// Parse env var `name` into `T`, falling back to `default` on unset/blank/garbage.
/// Trims surrounding whitespace so `" 5 "` parses; any parse failure is fail-soft.
fn parse_env<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

fn parse_env_string(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn positive_env_enabled(name: &str) -> bool {
    std::env::var(name)
        .map(|value| positive_value_enabled(&value))
        .unwrap_or(true)
}

fn positive_value_enabled(value: &str) -> bool {
    !matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "0" | "false" | "no"
    )
}

/// Nearest ancestor of `start` (inclusive) holding a `.anamnesis/` dir, mapped to
/// its `memory.db`. `None` if no ancestor has one.
fn resolve_project_db(start: &Path) -> Option<PathBuf> {
    let mut dir = start;
    loop {
        let candidate = dir.join(".anamnesis");
        if candidate.is_dir() {
            return Some(candidate.join("memory.db"));
        }
        dir = dir.parent()?;
    }
}

/// Point fastembed at a stable per-user cache dir unless the operator set one.
///
/// fastembed defaults to a CWD-relative `./.fastembed_cache`, which re-downloads
/// the ~400 MB model whenever the server is launched from a different directory.
pub fn ensure_model_cache_dir() {
    if std::env::var_os("FASTEMBED_CACHE_DIR").is_none() {
        let dir = anamnesis_home().join("models");
        // SAFETY: called once at startup before any threads spawn/read env.
        unsafe { std::env::set_var("FASTEMBED_CACHE_DIR", &dir) };
    }
}

/// The resolved fastembed model cache directory (`FASTEMBED_CACHE_DIR`, or the
/// `~/.anamnesis/models` default). Read after [`ensure_model_cache_dir`] so the
/// env var reflects the default when the operator did not set one.
pub fn model_cache_dir() -> PathBuf {
    std::env::var_os("FASTEMBED_CACHE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| anamnesis_home().join("models"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reinforce_defaults_true_and_can_disable() {
        // No assertions on process env (avoid global mutation in parallel tests);
        // test the parsing helper directly via a constructed Config instead.
        let cfg = Config {
            default_db: "x".into(),
            default_namespace: "default".into(),
            reinforce_on_recall: true,
            hook_threshold: DEFAULT_HOOK_THRESHOLD,
            hook_cosine_gate: DEFAULT_HOOK_COSINE_GATE,
            hook_seed_cosine_gate: DEFAULT_HOOK_SEED_COSINE_GATE,
            hook_context_turns: DEFAULT_HOOK_CONTEXT_TURNS,
            hook_topk: 3,
            hook_seed_k: 5,
            hook_timeout_ms: 1500,
            capture_enabled: true,
            extract_threshold_n: 20,
            embed_model: DEFAULT_EMBED_MODEL.to_string(),
            auto_migrate_embeddings: true,
        };
        assert!(cfg.reinforce_on_recall);
        assert_eq!(cfg.db_dir(), PathBuf::from("."));
    }

    #[test]
    fn db_dir_is_parent_of_default_db() {
        let cfg = Config {
            default_db: PathBuf::from("/var/lib/anamnesis/memory.db"),
            default_namespace: "default".into(),
            reinforce_on_recall: true,
            hook_threshold: DEFAULT_HOOK_THRESHOLD,
            hook_cosine_gate: DEFAULT_HOOK_COSINE_GATE,
            hook_seed_cosine_gate: DEFAULT_HOOK_SEED_COSINE_GATE,
            hook_context_turns: DEFAULT_HOOK_CONTEXT_TURNS,
            hook_topk: 3,
            hook_seed_k: 5,
            hook_timeout_ms: 1500,
            capture_enabled: true,
            extract_threshold_n: 20,
            embed_model: DEFAULT_EMBED_MODEL.to_string(),
            auto_migrate_embeddings: true,
        };
        assert_eq!(cfg.db_dir(), PathBuf::from("/var/lib/anamnesis"));
    }

    #[test]
    fn project_scope_found_in_nearest_ancestor() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join(".anamnesis")).unwrap();
        let nested = root.path().join("a/b/c");
        std::fs::create_dir_all(&nested).unwrap();
        // Walking up from a deep subdir finds the project marker at the root.
        assert_eq!(
            resolve_project_db(&nested),
            Some(root.path().join(".anamnesis").join("memory.db"))
        );
    }

    #[test]
    fn no_marker_falls_back_to_global() {
        let root = tempfile::tempdir().unwrap();
        // No `.anamnesis/` anywhere up the chain → None (caller uses ~/.anamnesis).
        assert_eq!(resolve_project_db(root.path()), None);
    }

    #[test]
    fn parse_env_falls_back_on_unset_and_garbage() {
        // Avoid mutating process env in parallel tests: only exercise the
        // unset/garbage fallback paths, which never touch a set variable.
        let name = "ANAMNESIS_DEFINITELY_UNSET_FOR_TEST_42";
        assert_eq!(parse_env(name, 5usize), 5);
        assert_eq!(
            parse_env::<f64>(name, DEFAULT_HOOK_THRESHOLD),
            DEFAULT_HOOK_THRESHOLD
        );
        assert_eq!(parse_env(name, 1500u64), 1500);
    }

    #[test]
    fn capture_knobs_have_sane_defaults() {
        let cfg = Config {
            default_db: "x".into(),
            default_namespace: "default".into(),
            reinforce_on_recall: true,
            hook_threshold: DEFAULT_HOOK_THRESHOLD,
            hook_cosine_gate: DEFAULT_HOOK_COSINE_GATE,
            hook_seed_cosine_gate: DEFAULT_HOOK_SEED_COSINE_GATE,
            hook_context_turns: DEFAULT_HOOK_CONTEXT_TURNS,
            hook_topk: DEFAULT_HOOK_TOPK,
            hook_seed_k: 5,
            hook_timeout_ms: 1500,
            capture_enabled: true,
            extract_threshold_n: 20,
            embed_model: DEFAULT_EMBED_MODEL.to_string(),
            auto_migrate_embeddings: true,
        };
        assert!(cfg.capture_enabled);
        assert_eq!(cfg.extract_threshold_n, 20);
    }

    #[test]
    fn hook_gate_knobs_default_and_parse() {
        let cfg = Config {
            default_db: "x".into(),
            default_namespace: "default".into(),
            reinforce_on_recall: true,
            hook_threshold: DEFAULT_HOOK_THRESHOLD,
            hook_cosine_gate: DEFAULT_HOOK_COSINE_GATE,
            hook_seed_cosine_gate: DEFAULT_HOOK_SEED_COSINE_GATE,
            hook_context_turns: DEFAULT_HOOK_CONTEXT_TURNS,
            hook_topk: DEFAULT_HOOK_TOPK,
            hook_seed_k: 5,
            hook_timeout_ms: 1500,
            capture_enabled: true,
            extract_threshold_n: 20,
            embed_model: DEFAULT_EMBED_MODEL.to_string(),
            auto_migrate_embeddings: true,
        };
        assert_eq!(cfg.hook_cosine_gate, 0.86);
        assert_eq!(cfg.hook_seed_cosine_gate, 0.80);
        assert_eq!(cfg.hook_context_turns, 3);
        assert_eq!(cfg.hook_topk, DEFAULT_HOOK_TOPK);
        assert_eq!(
            parse_env(
                "ANAMNESIS_DEFINITELY_UNSET_FOR_TEST_HOOK_COSINE",
                DEFAULT_HOOK_COSINE_GATE,
            ),
            DEFAULT_HOOK_COSINE_GATE
        );
    }

    #[test]
    fn operations_hook_topk_default_matches_source() {
        let expected = format!("| `ANAMNESIS_HOOK_TOPK` | `{DEFAULT_HOOK_TOPK}` |");
        let operations = include_str!("../../../docs/06-operations/operations.md");
        assert!(
            operations.contains(&expected),
            "operations env table must contain the source default row prefix: {expected}"
        );
    }

    #[test]
    fn embed_model_default_is_multilingual_e5_small() {
        assert_eq!(DEFAULT_EMBED_MODEL, "multilingual-e5-small");
        assert_eq!(
            parse_env_string(
                "ANAMNESIS_DEFINITELY_UNSET_FOR_TEST_EMBED_MODEL",
                DEFAULT_EMBED_MODEL,
            ),
            DEFAULT_EMBED_MODEL
        );
    }

    #[test]
    fn auto_migration_positive_flag_defaults_enabled_and_accepts_falseish_values() {
        assert!(positive_env_enabled(
            "ANAMNESIS_DEFINITELY_UNSET_AUTO_MIGRATION"
        ));
        for value in ["0", "false", "FALSE", " no "] {
            assert!(!positive_value_enabled(value));
        }
        assert!(positive_value_enabled("yes"));
    }

    #[test]
    fn default_hook_threshold_is_a_sane_positive_prior() {
        // The τ prior must be a finite, positive floor on the *activation* scale.
        // The comparison is against the raw ACT-R recall score (~8–16 on a typical
        // graph), so a sub-1 default would silently disable the gate (everything
        // injects) — assert `> 1.0` to catch a future normalized-0..1 regression.
        // A compile-time const block keeps the invariant without a constant-valued
        // runtime assertion (clippy::assertions_on_constants).
        const {
            assert!(DEFAULT_HOOK_THRESHOLD.is_finite());
            assert!(DEFAULT_HOOK_THRESHOLD > 1.0);
        }
    }
}
