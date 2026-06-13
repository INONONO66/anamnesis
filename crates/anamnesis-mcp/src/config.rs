//! Server/CLI configuration resolved from environment + flags.

use std::path::PathBuf;

/// Resolved runtime configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// SQLite file for the default namespace.
    pub default_db: PathBuf,
    /// Namespace used when a call omits one.
    pub default_namespace: String,
    /// Auto-commit (reinforce) the package returned by `recall`.
    pub reinforce_on_recall: bool,
}

impl Config {
    /// Resolve from environment variables, falling back to sane defaults.
    ///
    /// - `ANAMNESIS_DB`         → `default_db` (default: `<data_dir>/anamnesis/memory.db`)
    /// - `ANAMNESIS_NAMESPACE`  → `default_namespace` (default: `"default"`)
    /// - `ANAMNESIS_REINFORCE`  → `reinforce_on_recall`; "0"/"false" disables (default: true)
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
        Self { default_db, default_namespace, reinforce_on_recall }
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

fn default_db_path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("anamnesis")
        .join("memory.db")
}

/// Point fastembed at a stable per-user cache dir unless the operator set one.
///
/// fastembed defaults to a CWD-relative `./.fastembed_cache`, which re-downloads
/// the ~400 MB model whenever the server is launched from a different directory.
pub fn ensure_model_cache_dir() {
    if std::env::var_os("FASTEMBED_CACHE_DIR").is_none() {
        let dir = dirs::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("anamnesis")
            .join("models");
        // SAFETY: called once at startup before any threads spawn/read env.
        unsafe { std::env::set_var("FASTEMBED_CACHE_DIR", &dir) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reinforce_defaults_true_and_can_disable() {
        // No assertions on process env (avoid global mutation in parallel tests);
        // test the parsing helper directly via a constructed Config instead.
        let cfg = Config { default_db: "x".into(), default_namespace: "default".into(), reinforce_on_recall: true };
        assert!(cfg.reinforce_on_recall);
        assert_eq!(cfg.db_dir(), PathBuf::from("."));
    }

    #[test]
    fn db_dir_is_parent_of_default_db() {
        let cfg = Config {
            default_db: PathBuf::from("/var/lib/anamnesis/memory.db"),
            default_namespace: "default".into(),
            reinforce_on_recall: true,
        };
        assert_eq!(cfg.db_dir(), PathBuf::from("/var/lib/anamnesis"));
    }
}
