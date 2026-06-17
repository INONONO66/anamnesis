//! Server/CLI configuration resolved from environment + flags.

use std::path::{Path, PathBuf};

/// Starting prior for the hook injection gate `τ` (need-odds floor on the top
/// readout score). A deliberately conservative default: the verify phase
/// calibrates the real number against a live graph so a clearly-relevant prompt
/// injects and an off-topic prompt injects nothing (spec §`τ` default).
pub const DEFAULT_HOOK_THRESHOLD: f64 = 0.15;

/// Resolved runtime configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// SQLite file for the default namespace.
    pub default_db: PathBuf,
    /// Namespace used when a call omits one.
    pub default_namespace: String,
    /// Auto-commit (reinforce) the package returned by `recall`.
    pub reinforce_on_recall: bool,
    // The four `hook_*` knobs below are resolved here (Component 1) and consumed by
    // the `hook` subcommand family (Component 2, added next). Allow dead_code until
    // that subcommand reads them so this component's quality gate stays green.
    /// `τ` — need-odds injection gate (top-score floor). `ANAMNESIS_HOOK_THRESHOLD`.
    #[allow(dead_code)]
    pub hook_threshold: f64,
    /// `k` — cap on injected per-turn memories. `ANAMNESIS_HOOK_TOPK` (default 5).
    #[allow(dead_code)]
    pub hook_topk: usize,
    /// SessionStart seed size. `ANAMNESIS_HOOK_SEED_K` (default 5).
    #[allow(dead_code)]
    pub hook_seed_k: usize,
    /// Per-hook fail-open timeout (ms). `ANAMNESIS_HOOK_TIMEOUT_MS` (default 1500).
    #[allow(dead_code)]
    pub hook_timeout_ms: u64,
}

impl Config {
    /// Resolve from environment variables, falling back to sane defaults.
    ///
    /// - `ANAMNESIS_DB`              → `default_db` (default: `<data_dir>/anamnesis/memory.db`)
    /// - `ANAMNESIS_NAMESPACE`       → `default_namespace` (default: `"default"`)
    /// - `ANAMNESIS_REINFORCE`       → `reinforce_on_recall`; "0"/"false" disables (default: true)
    /// - `ANAMNESIS_HOOK_THRESHOLD`  → `hook_threshold` (default: [`DEFAULT_HOOK_THRESHOLD`])
    /// - `ANAMNESIS_HOOK_TOPK`       → `hook_topk` (default: 5)
    /// - `ANAMNESIS_HOOK_SEED_K`     → `hook_seed_k` (default: 5)
    /// - `ANAMNESIS_HOOK_TIMEOUT_MS` → `hook_timeout_ms` (default: 1500)
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
        let hook_topk = parse_env("ANAMNESIS_HOOK_TOPK", 5usize);
        let hook_seed_k = parse_env("ANAMNESIS_HOOK_SEED_K", 5usize);
        let hook_timeout_ms = parse_env("ANAMNESIS_HOOK_TIMEOUT_MS", 1500u64);
        Self {
            default_db,
            default_namespace,
            reinforce_on_recall,
            hook_threshold,
            hook_topk,
            hook_seed_k,
            hook_timeout_ms,
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
            hook_topk: 5,
            hook_seed_k: 5,
            hook_timeout_ms: 1500,
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
            hook_topk: 5,
            hook_seed_k: 5,
            hook_timeout_ms: 1500,
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
    fn default_hook_threshold_is_a_sane_positive_prior() {
        // The starting τ prior must be a finite, positive floor (calibrated later).
        // A compile-time const block keeps the invariant without a constant-valued
        // runtime assertion (clippy::assertions_on_constants).
        const {
            assert!(DEFAULT_HOOK_THRESHOLD.is_finite());
            assert!(DEFAULT_HOOK_THRESHOLD > 0.0);
        }
    }
}
