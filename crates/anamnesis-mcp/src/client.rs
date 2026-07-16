//! Bespoke daemon client — the MCP-free wire to the shared daemon.
//!
//! The daemon speaks [`crate::proto`] (newline-delimited JSON, one
//! request→response per line) over its per-DB unix socket. This connects (via the
//! launcher's spawn/retry) and issues requests. The `serve` adapter holds one
//! persistent [`DaemonClient`] for the whole agent session and forwards each MCP
//! tool call; the CLI one-shots and the hook use [`call_oneshot`]. No rmcp here —
//! MCP lives only in `server.rs` (see ADR-0012).

use anyhow::{Context, Result, anyhow};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

use crate::config::Config;
use crate::extract::types::{
    ExtractionScanResult, ExtractionSource, ExtractorProfileComponents, ValidatedExtraction,
};
use crate::launcher::ensure_daemon;
use crate::proto::{self, ExtractionErrorKind, Request, Response, StageExtractionResult};

/// A connected, persistent client of the shared daemon.
///
/// Calls are serialized — one request→response at a time over the single
/// connection. That loses no throughput because the daemon serializes every op
/// at its one registry `Mutex` regardless, and it lets the wire stay correlation-
/// id-free (a reply always belongs to the most recent request).
pub struct DaemonClient {
    reader: Lines<BufReader<OwnedReadHalf>>,
    writer: OwnedWriteHalf,
}

impl DaemonClient {
    /// Ensure the daemon for `cfg`'s resolved DB is up and connect to it.
    pub async fn connect(cfg: &Config) -> Result<Self> {
        let stream = ensure_daemon(&cfg.default_db)
            .await
            .context("connect to the anamnesis daemon")?;
        let (rd, wr) = stream.into_split();
        Ok(Self {
            reader: BufReader::new(rd).lines(),
            writer: wr,
        })
    }

    /// Send one request and read its reply.
    ///
    /// Transport failures (write/read/EOF) are `Err`. A daemon-level error comes
    /// back as `Ok(Response::Err{..})` — NOT collapsed — so the caller can re-map
    /// the [`proto::ErrKind`] faithfully (the `serve` adapter turns it back into
    /// an MCP `invalid_params` vs `internal_error`).
    pub async fn call(&mut self, req: &Request) -> Result<Response> {
        let line = proto::encode_line(req).context("encode request")?;
        self.writer
            .write_all(line.as_bytes())
            .await
            .context("send request to daemon")?;
        self.writer
            .flush()
            .await
            .context("flush request to daemon")?;
        let resp_line = self
            .reader
            .next_line()
            .await
            .context("read daemon response")?
            .ok_or_else(|| anyhow!("daemon closed the connection without responding"))?;
        proto::decode_line::<Response>(&resp_line).context("decode daemon response")
    }
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "Task 7 extraction facade is wired by Task 9")
    )]
    /// Request a bounded extraction scan from the daemon.
    pub async fn extraction_scan(
        &mut self,
        namespace: Option<&str>,
        profile: &ExtractorProfileComponents,
        min_turns: u32,
        max_turns: u32,
    ) -> Result<ExtractionScanResult> {
        self.extraction_response(Request::ExtractionScan {
            namespace: namespace.map(str::to_owned),
            profile: profile.clone(),
            min_turns,
            max_turns,
        })
        .await
    }

    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "Task 7 extraction facade is wired by Task 9")
    )]
    /// Stage validated extraction output through the daemon.
    pub async fn stage_extraction(
        &mut self,
        namespace: Option<&str>,
        profile: &ExtractorProfileComponents,
        duration_ms: u64,
        sources: Vec<ExtractionSource>,
        extraction: ValidatedExtraction,
    ) -> Result<StageExtractionResult> {
        self.extraction_response(Request::StageExtraction {
            namespace: namespace.map(str::to_owned),
            profile: profile.clone(),
            llm_duration_ms: duration_ms,
            sources,
            extraction,
        })
        .await
    }

    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "Task 7 extraction facade is wired by Task 9")
    )]
    /// Record a typed extraction failure through the daemon.
    pub async fn record_extraction_failure(
        &mut self,
        namespace: Option<&str>,
        profile: &ExtractorProfileComponents,
        turn_count: u32,
        llm_invoked: bool,
        error_kind: ExtractionErrorKind,
        duration_ms: u64,
    ) -> Result<()> {
        match self
            .call(&Request::RecordExtractionFailure {
                namespace: namespace.map(str::to_owned),
                profile: profile.clone(),
                turn_count,
                llm_invoked,
                error_kind,
                duration_ms,
            })
            .await?
        {
            Response::Ok { .. } => Ok(()),
            Response::Err { message, .. } => Err(anyhow!(message)),
        }
    }

    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "Task 7 extraction facade is wired by Task 9")
    )]
    async fn extraction_response<T>(&mut self, request: Request) -> Result<T>
    where
        T: serde::de::DeserializeOwned,
    {
        match self.call(&request).await? {
            Response::Ok { text } => {
                serde_json::from_str(&text).context("decode extraction response")
            }
            Response::Err { message, .. } => Err(anyhow!(message)),
        }
    }
}

/// Connect, issue one request, return its `Ok` text (or the daemon's error as an
/// `anyhow` error), and disconnect — the CLI one-shot / hook path. Dropping the
/// client closes the connection, so the daemon's client ref-count falls and its
/// grace timer can start.
pub async fn call_oneshot(cfg: &Config, req: Request) -> Result<String> {
    let mut client = DaemonClient::connect(cfg).await?;
    match client.call(&req).await? {
        Response::Ok { text } => Ok(text),
        Response::Err { message, .. } => Err(anyhow!(message)),
    }
}
