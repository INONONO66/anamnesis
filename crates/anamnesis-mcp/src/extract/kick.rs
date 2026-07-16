//! Detached extraction-worker kick policy and spawning.
//!
//! This module deliberately does not resolve worker configuration or open the
//! daemon. The hook/CLI caller supplies the already-resolved namespace and keeps
//! spawn errors typed so its own fail-open policy can decide how to report them.

use std::io;

use crate::extract::config::ExtractMode;
use crate::launcher::spawn_detached_current_exe;

/// Test seam for launching an asynchronous extraction pass.
pub(crate) trait ExtractKickSpawner {
    /// Start extraction for a resolved namespace without waiting for it.
    fn spawn_extract(&self, namespace: &str) -> io::Result<()>;
}

/// Production detached extraction-worker launcher.
pub(crate) struct DetachedExtractKickSpawner;

impl ExtractKickSpawner for DetachedExtractKickSpawner {
    fn spawn_extract(&self, namespace: &str) -> io::Result<()> {
        spawn_detached_current_exe(&[
            "extract".to_owned(),
            "--namespace".to_owned(),
            namespace.to_owned(),
        ])
    }
}

/// Whether a successful capture should start the optional extraction worker.
///
/// Only the exact opt-in `shadow` mode may kick, and only after the two capture
/// events that flush a substantial transcript tail. `capture_succeeded` keeps
/// failed or fail-open capture paths from triggering unrelated worker activity.
pub(crate) fn should_kick(mode: ExtractMode, event: &str, capture_succeeded: bool) -> bool {
    capture_succeeded && mode == ExtractMode::Shadow && matches!(event, "PreCompact" | "SessionEnd")
}
