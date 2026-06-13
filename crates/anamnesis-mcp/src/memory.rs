//! Namespace-aware wrapper over `anamnesis::Memory`.
//!
//! Owns one `Memory<SqliteStorage>` per namespace, all sharing a single
//! embedding provider that is built lazily on first use. All access is
//! single-threaded by construction (the server holds this behind a Mutex).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anamnesis::embedding::EmbeddingProvider;
use anamnesis::graph::Timestamp;
use anamnesis::memory::Hit;
use anamnesis::storage::SqliteStorage;
use anamnesis::{Error, Memory};

/// One conversational turn for `ingest_conversation`.
#[derive(Debug, Clone)]
pub struct Turn {
    pub speaker: String,
    pub text: String,
    /// Unix-millis timestamp; if `None`, a monotonic value is assigned.
    pub at_ms: Option<u64>,
}

/// Summary returned by `ingest_conversation`.
#[derive(Debug, Clone, Copy)]
pub struct IngestSummary {
    pub episodic: usize,
    pub semantic: usize,
}

/// How the shared embedding provider is obtained.
enum ProviderSource {
    /// Pre-built provider (tests, or a caller-supplied embedder).
    #[cfg(test)]
    Ready(Arc<dyn EmbeddingProvider>),
    /// Build the FastEmbed (bge-base-en-v1.5) provider on first use.
    FastEmbedLazy,
}

/// Storage backend for opened namespaces.
enum Backend {
    /// File-backed: `<dir>/<namespace>.db` (the default namespace uses `default_db`).
    File {
        default_db: PathBuf,
        dir: PathBuf,
        default_namespace: String,
    },
    /// In-memory (tests): every namespace is a fresh in-memory graph.
    #[cfg(test)]
    Memory,
}

pub struct MemoryRegistry {
    provider: Option<Arc<dyn EmbeddingProvider>>,
    source: ProviderSource,
    backend: Backend,
    reinforce_on_recall: bool,
    open: HashMap<String, Memory<SqliteStorage>>,
    default_namespace: String,
}

impl MemoryRegistry {
    /// Production constructor: file-backed, FastEmbed provider built lazily.
    pub fn file_backed(
        default_db: PathBuf,
        dir: PathBuf,
        default_namespace: String,
        reinforce_on_recall: bool,
    ) -> Self {
        Self {
            provider: None,
            source: ProviderSource::FastEmbedLazy,
            backend: Backend::File {
                default_db,
                dir,
                default_namespace: default_namespace.clone(),
            },
            reinforce_on_recall,
            open: HashMap::new(),
            default_namespace,
        }
    }

    /// Test/embeddable constructor: in-memory graphs + a caller-supplied provider.
    #[cfg(test)]
    pub fn in_memory_with(provider: Arc<dyn EmbeddingProvider>, reinforce_on_recall: bool) -> Self {
        Self {
            provider: Some(provider.clone()),
            source: ProviderSource::Ready(provider),
            backend: Backend::Memory,
            reinforce_on_recall,
            open: HashMap::new(),
            default_namespace: "default".to_string(),
        }
    }

    fn provider(&mut self) -> Result<Arc<dyn EmbeddingProvider>, Error> {
        if let Some(p) = &self.provider {
            return Ok(p.clone());
        }
        // anamnesis-mcp depends on `anamnesis` with features = ["embed"]
        // unconditionally, so FastEmbedProvider is always available here.
        let p: Arc<dyn EmbeddingProvider> = match &self.source {
            #[cfg(test)]
            ProviderSource::Ready(p) => p.clone(),
            ProviderSource::FastEmbedLazy => {
                Arc::new(anamnesis::embedding::fastembed::FastEmbedProvider::new()?)
            }
        };
        self.provider = Some(p.clone());
        Ok(p)
    }

    /// Sanitize a namespace into a safe file stem (no path traversal).
    fn sanitize(ns: &str) -> String {
        let s: String = ns
            .trim()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        if s.is_empty() {
            "default".to_string()
        } else {
            s.to_lowercase()
        }
    }

    fn resolve_namespace<'a>(&'a self, ns: Option<&'a str>) -> &'a str {
        ns.unwrap_or(&self.default_namespace)
    }

    /// Canonical identity for a namespace: the key under which its single
    /// `Memory` instance (and, for the file backend, its on-disk file) lives.
    ///
    /// For the file backend this is the sanitized stem, so raw namespaces that
    /// sanitize to the same stem (e.g. `"alpha"`/`"Alpha"`, `"a/b"`/`"a-b"`)
    /// collapse to ONE instance over ONE file instead of two instances racing
    /// over the same file. The default namespace keeps its raw key so it stays
    /// distinct from any sanitized collision (and an explicit request for the
    /// default namespace name still routes to `default_db`).
    fn canonical_key(&self, ns: &str) -> String {
        match &self.backend {
            Backend::File {
                default_namespace, ..
            } if ns == default_namespace => ns.to_string(),
            Backend::File { .. } => Self::sanitize(ns),
            #[cfg(test)]
            Backend::Memory => ns.to_string(),
        }
    }

    fn open_namespace(&mut self, ns: &str) -> Result<Memory<SqliteStorage>, Error> {
        let provider = self.provider()?;
        match &self.backend {
            #[cfg(test)]
            Backend::Memory => Memory::in_memory_with_provider(provider),
            Backend::File {
                default_db,
                dir,
                default_namespace,
            } => {
                let path = if ns == default_namespace {
                    default_db.clone()
                } else {
                    let path = dir.join(format!("{}.db", Self::sanitize(ns)));
                    // A non-default namespace whose sanitized file resolves to the
                    // default namespace's DB would silently share one SQLite file
                    // under two HashMap keys (divergent caches, data leak). Reject it.
                    if path == *default_db {
                        return Err(Error::InvalidInput(format!(
                            "namespace {ns:?} collides with the default namespace's database file; \
                             choose a different namespace name"
                        )));
                    }
                    path
                };
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| Error::StorageError(format!("create db dir: {e}")))?;
                }
                Memory::with_provider(path, provider)
            }
        }
    }

    fn get(&mut self, ns: Option<&str>) -> Result<&mut Memory<SqliteStorage>, Error> {
        let raw = self.resolve_namespace(ns).to_string();
        let key = self.canonical_key(&raw);
        if !self.open.contains_key(&key) {
            let mem = self.open_namespace(&key)?;
            self.open.insert(key.clone(), mem);
        }
        Ok(self.open.get_mut(&key).expect("just inserted"))
    }

    /// Search; on success optionally auto-commit (reinforce) the returned package.
    /// A lazy `tick(now)` keeps forgetting current without a background thread and
    /// persists the reinforcement.
    pub fn recall(
        &mut self,
        query: &str,
        limit: usize,
        ns: Option<&str>,
    ) -> Result<Vec<Hit>, Error> {
        let reinforce = self.reinforce_on_recall;
        let mem = self.get(ns)?;
        let recall = mem.search(query, limit)?;
        let hits = recall.hits.clone();
        if reinforce {
            mem.used(recall)?;
        }
        // Lazy tick AFTER the reinforcing commit. `Engine::commit` does not flush
        // storage; `tick` does. Ticking here both applies forgetting and persists
        // the reinforcement to SQLite — without it, CLI one-shot `recall` would
        // always lose its reinforcement and `serve` would lose the last recall
        // before shutdown. Decay is one recall behind, which is negligible.
        mem.tick(Timestamp::now())?;
        Ok(hits)
    }

    /// Store one distilled insight (`add_note`). Returns the episodic node id.
    pub fn remember(&mut self, text: &str, ns: Option<&str>) -> Result<u64, Error> {
        let mem = self.get(ns)?;
        let receipt = mem.add_note(text, Timestamp::now())?;
        mem.flush_all()?;
        Ok(receipt.episodic.0)
    }

    /// Ingest a batch of conversational turns via the bench windowing recipe.
    pub fn ingest_conversation(
        &mut self,
        session: &str,
        turns: &[Turn],
        ns: Option<&str>,
    ) -> Result<IngestSummary, Error> {
        let mem = self.get(ns)?;
        let base = Timestamp::now().0;
        let mut episodic = 0usize;
        let mut semantic = 0usize;
        for (i, turn) in turns.iter().enumerate() {
            let at = Timestamp(turn.at_ms.unwrap_or(base + i as u64));
            let receipt = mem.add(session, &turn.speaker, &turn.text, at)?;
            episodic += 1;
            if receipt.finalized_semantic.is_some() {
                semantic += 1;
            }
        }
        if mem.flush_session(session)?.is_some() {
            semantic += 1;
        }
        Ok(IngestSummary { episodic, semantic })
    }

    /// Force the embedding provider to build (download the model). For `prewarm`.
    pub fn prewarm(&mut self) -> Result<(), Error> {
        let provider = self.provider()?;
        provider.embed_single("warm")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic 64-dim provider — no network, no model download.
    struct StubProvider;
    impl EmbeddingProvider for StubProvider {
        fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
            Ok(texts
                .iter()
                .map(|t| {
                    let len = t.len() as f32;
                    (0..64)
                        .map(|i| ((len * (i as f32 + 1.0)) % 100.0) / 100.0)
                        .collect()
                })
                .collect())
        }
        fn dimensions(&self) -> usize {
            64
        }
        fn model_name(&self) -> &str {
            "stub-64"
        }
    }

    fn registry(reinforce: bool) -> MemoryRegistry {
        MemoryRegistry::in_memory_with(Arc::new(StubProvider), reinforce)
    }

    #[test]
    fn remember_then_recall_returns_a_hit() {
        let mut reg = registry(true);
        // `remember` returns the episodic node id; `.unwrap()` already proves it
        // stored without error (any u64 is a valid id).
        let _id = reg
            .remember("the auth bug was a race in the middleware", None)
            .unwrap();
        let hits = reg.recall("auth race condition", 5, None).unwrap();
        assert!(!hits.is_empty(), "expected at least one hit after remember");
    }

    #[test]
    fn ingest_conversation_counts_turns() {
        let mut reg = registry(true);
        let turns = vec![
            Turn {
                speaker: "alice".into(),
                text: "we picked postgres".into(),
                at_ms: None,
            },
            Turn {
                speaker: "bob".into(),
                text: "because of jsonb".into(),
                at_ms: None,
            },
            Turn {
                speaker: "alice".into(),
                text: "and row-level security".into(),
                at_ms: None,
            },
        ];
        let summary = reg
            .ingest_conversation("design-chat", &turns, None)
            .unwrap();
        assert_eq!(summary.episodic, 3);
        assert!(summary.semantic >= 1);
    }

    #[test]
    fn namespaces_are_isolated() {
        let mut reg = registry(true);
        reg.remember("alpha-only secret", Some("alpha")).unwrap();
        let beta_hits = reg.recall("alpha-only secret", 5, Some("beta")).unwrap();
        assert!(
            beta_hits.is_empty(),
            "namespace beta must not see alpha's memory"
        );
    }

    #[test]
    fn sanitize_blocks_path_traversal() {
        assert_eq!(
            MemoryRegistry::sanitize("../../etc/passwd"),
            "------etc-passwd"
        );
        assert_eq!(MemoryRegistry::sanitize(""), "default");
        assert_eq!(MemoryRegistry::sanitize("Work Project:1"), "work-project-1");
    }

    /// A non-default namespace whose sanitized stem equals the default DB file
    /// stem must be rejected, not silently aliased onto the default file.
    #[test]
    fn namespace_colliding_with_default_db_file_is_rejected() {
        let dir = std::env::temp_dir().join("anamnesis-ns-collision-test");
        let default_db = dir.join("memory.db");
        let mut reg = MemoryRegistry::file_backed(default_db, dir, "default".to_string(), false);
        reg.provider = Some(Arc::new(StubProvider));
        // ns "memory" sanitizes to "memory" → <dir>/memory.db == default_db.
        let err = reg.remember("leak attempt", Some("memory")).unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)), "got {err:?}");
    }

    /// Raw namespaces that sanitize to the same stem must collapse to ONE
    /// instance over ONE file, not two instances racing over the same file.
    #[test]
    fn sanitize_equal_namespaces_share_one_instance() {
        let dir = std::env::temp_dir().join("anamnesis-ns-canonical-test");
        let _ = std::fs::remove_dir_all(&dir);
        let default_db = dir.join("memory.db");
        let mut reg = MemoryRegistry::file_backed(default_db, dir, "default".to_string(), false);
        reg.provider = Some(Arc::new(StubProvider));
        reg.remember("shared via Alpha", Some("Alpha")).unwrap();
        // "alpha" sanitizes to the same stem as "Alpha"; it must see the write.
        let hits = reg.recall("shared via Alpha", 5, Some("alpha")).unwrap();
        assert!(
            !hits.is_empty(),
            "alpha must see Alpha's write (same canonical namespace)"
        );
        // Exactly one open instance for both raw spellings.
        assert_eq!(reg.open.len(), 1, "Alpha and alpha must share one instance");
    }
}
