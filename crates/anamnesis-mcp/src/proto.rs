//! Bespoke daemon wire protocol — the MCP-free internal request/response the
//! daemon speaks over its unix socket.
//!
//! MCP (rmcp) is the *agent's* protocol; the `serve` adapter translates it to
//! these requests. Everything else on the socket path (the daemon itself, the
//! hook, the CLI one-shots) speaks only `proto` and never touches rmcp.
//!
//! Framing: one JSON object per line, `\n`-terminated. A connection is
//! persistent and carries sequential request→response pairs (no correlation
//! ids: calls are serialized, and the daemon serializes at its single registry
//! mutex anyway). See `docs/adr/0012-daemon-core-mcp-plugin-clients.md`.

use serde::{Deserialize, Serialize};

/// One conversation turn for [`Request::Ingest`] (serde-only mirror of the
/// engine's `Turn`; kept here so `proto` has no rmcp/schemars dependency).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnInput {
    pub speaker: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_ms: Option<u64>,
}

/// A single operation sent to the daemon. Tagged by `op` so the wire is a flat
/// self-describing JSON object, e.g. `{"op":"recall","query":"…"}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Recall {
        query: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
        /// `false` ⇒ pure read (no reinforcing commit). The hook path passes `false`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reinforce: Option<bool>,
        /// Need-odds gate `τ`: below it, recall returns nothing.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gate_threshold: Option<f64>,
        /// Query-embedding cosine gate `τ_cos`: below it, recall returns nothing.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cosine_gate: Option<f64>,
        /// Render only durable knowledge; omit episodic/capture fragments.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        knowledge_only: Option<bool>,
        /// Post-filter: drop hits whose node origin scope doesn't match.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<String>,
        /// Post-filter: drop hits whose node doesn't carry this entity tag.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tag: Option<String>,
    },
    Remember {
        content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tags: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<std::collections::HashMap<String, String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<String>,
    },
    Relate {
        from_id: u64,
        to_id: u64,
        relation: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
    },
    Ingest {
        session: String,
        turns: Vec<TurnInput>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        capture: Option<bool>,
    },
    Stats {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
    },
    PullPending {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
    },
    ExtractionStatus {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
    },
    Update {
        id: u64,
        new_content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
    },
    Forget {
        id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        /// `true` ⇒ permanently remove the node (irreversible); omitted/`false`
        /// ⇒ soft-delete (reversible via `unforget`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hard: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
    },
    Supersede {
        new_id: u64,
        old_id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
    },
    List {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min_salience: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_type: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tag: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<String>,
        /// `"key=value"`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<String>,
    },
    Get {
        id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
    },
    Graph {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seeds: Option<Vec<u64>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        query: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        depth: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
    },
}

/// Whether a failed request is the caller's fault (e.g. a bad relation label or
/// missing node id) or an internal fault. Mirrors the MCP `invalid_params` vs
/// `internal_error` split so the `serve` adapter can re-map it faithfully.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrKind {
    Internal,
    InvalidParams,
}

/// The daemon's reply. `text` is the fully-formatted, consumer-ready output
/// (identical to what the current MCP tools return), so every client — the
/// `serve` adapter, the hook, the CLI — uses it verbatim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Response {
    Ok { text: String },
    Err { kind: ErrKind, message: String },
}

impl Response {
    pub fn ok(text: impl Into<String>) -> Self {
        Response::Ok { text: text.into() }
    }
    pub fn internal(message: impl std::fmt::Display) -> Self {
        Response::Err {
            kind: ErrKind::Internal,
            message: message.to_string(),
        }
    }
    pub fn invalid_params(message: impl std::fmt::Display) -> Self {
        Response::Err {
            kind: ErrKind::InvalidParams,
            message: message.to_string(),
        }
    }
}

/// Encode a value as one wire line: compact JSON followed by `\n`.
pub fn encode_line<T: Serialize>(value: &T) -> Result<String, serde_json::Error> {
    let mut s = serde_json::to_string(value)?;
    s.push('\n');
    Ok(s)
}

/// Decode one wire line (the trailing `\n` may be present or already stripped).
pub fn decode_line<T: for<'de> Deserialize<'de>>(line: &str) -> Result<T, serde_json::Error> {
    serde_json::from_str(line.trim_end_matches(['\n', '\r']))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip_request(req: Request) {
        let line = encode_line(&req).expect("encode");
        assert!(line.ends_with('\n'), "line must be newline-terminated");
        assert!(!line[..line.len() - 1].contains('\n'), "exactly one line");
        let back: Request = decode_line(&line).expect("decode");
        assert_eq!(req, back);
    }

    #[test]
    fn request_variants_round_trip() {
        round_trip_request(Request::Recall {
            query: "how does the gate work".into(),
            limit: Some(5),
            namespace: None,
            reinforce: Some(false),
            gate_threshold: Some(13.0),
            cosine_gate: Some(0.83),
            knowledge_only: Some(true),
            scope: Some("projA".into()),
            tag: Some("auth".into()),
        });
        round_trip_request(Request::Remember {
            content: "a lesson".into(),
            namespace: Some("proj".into()),
            tags: Some(vec!["auth".into()]),
            metadata: Some(std::collections::HashMap::from([(
                "k".to_string(),
                "v".to_string(),
            )])),
            scope: Some("projA".into()),
        });
        round_trip_request(Request::Relate {
            from_id: 1,
            to_id: 2,
            relation: "supports".into(),
            namespace: None,
        });
        round_trip_request(Request::Ingest {
            session: "s1".into(),
            turns: vec![
                TurnInput {
                    speaker: "user".into(),
                    text: "hi".into(),
                    at_ms: Some(1000),
                },
                TurnInput {
                    speaker: "assistant".into(),
                    text: "hello".into(),
                    at_ms: None,
                },
            ],
            namespace: None,
            capture: None,
        });
        round_trip_request(Request::Stats { namespace: None });
        round_trip_request(Request::Update {
            id: 7,
            new_content: "revised content".into(),
            namespace: Some("proj".into()),
        });
        round_trip_request(Request::Forget {
            id: 7,
            reason: Some("superseded".into()),
            hard: Some(false),
            namespace: None,
        });
        round_trip_request(Request::Supersede {
            new_id: 9,
            old_id: 7,
            namespace: None,
        });
        round_trip_request(Request::List {
            min_salience: Some(0.2),
            limit: Some(10),
            node_type: Some("semantic".into()),
            tag: Some("auth".into()),
            namespace: None,
            scope: Some("projA".into()),
            metadata: Some("k=v".into()),
        });
        round_trip_request(Request::Get {
            id: 7,
            namespace: None,
        });
        round_trip_request(Request::Graph {
            seeds: Some(vec![1, 2]),
            query: Some("how does the gate work".into()),
            depth: Some(2),
            limit: Some(50),
            namespace: Some("projA".into()),
        });
    }

    #[test]
    fn graph_omits_none_fields_on_the_wire() {
        let line = encode_line(&Request::Graph {
            seeds: None,
            query: None,
            depth: None,
            limit: None,
            namespace: None,
        })
        .unwrap();
        assert_eq!(line, "{\"op\":\"graph\"}\n", "got: {line}");
    }

    #[test]
    fn recall_omits_none_fields_on_the_wire() {
        let line = encode_line(&Request::Recall {
            query: "q".into(),
            limit: None,
            namespace: None,
            reinforce: None,
            gate_threshold: None,
            cosine_gate: None,
            knowledge_only: None,
            scope: None,
            tag: None,
        })
        .unwrap();
        assert!(line.contains("\"op\":\"recall\""), "tagged by op: {line}");
        assert!(line.contains("\"query\":\"q\""));
        assert!(!line.contains("limit"), "None optionals omitted: {line}");
        assert!(!line.contains("namespace"));
        assert!(!line.contains("gate_threshold"));
        assert!(!line.contains("cosine_gate"));
        assert!(!line.contains("knowledge_only"));
    }

    #[test]
    fn response_round_trips_and_distinguishes_error_kind() {
        for resp in [
            Response::ok("stored node 5"),
            Response::internal("boom"),
            Response::invalid_params("unknown relation label"),
        ] {
            let line = encode_line(&resp).unwrap();
            let back: Response = decode_line(&line).unwrap();
            assert_eq!(resp, back);
        }
        // The kind is preserved across the wire (serve re-maps it to MCP).
        let line = encode_line(&Response::invalid_params("bad")).unwrap();
        let back: Response = decode_line(&line).unwrap();
        assert!(matches!(
            back,
            Response::Err {
                kind: ErrKind::InvalidParams,
                ..
            }
        ));
    }

    #[test]
    fn decode_tolerates_crlf_and_trailing_newline() {
        let r: Response = decode_line("{\"status\":\"ok\",\"text\":\"hi\"}\r\n").unwrap();
        assert_eq!(r, Response::ok("hi"));
    }

    #[test]
    fn pull_and_status_round_trip() {
        let a = Request::PullPending {
            limit: Some(10),
            namespace: None,
        };
        assert_eq!(
            decode_line::<Request>(&encode_line(&a).unwrap()).unwrap(),
            a
        );
        let b = Request::ExtractionStatus { namespace: None };
        let line = encode_line(&b).unwrap();
        assert!(line.contains("\"op\":\"extraction_status\""), "got: {line}");
        assert_eq!(decode_line::<Request>(&line).unwrap(), b);
    }

    #[test]
    fn ingest_capture_flag_round_trips_and_defaults_absent() {
        // capture omitted ⇒ absent on the wire (skip_serializing_if = None).
        let req = Request::Ingest {
            session: "s".into(),
            turns: vec![],
            namespace: None,
            capture: None,
        };
        let line = encode_line(&req).unwrap();
        assert!(
            !line.contains("capture"),
            "None capture must be omitted: {line}"
        );
        let back: Request = decode_line(&line).unwrap();
        assert_eq!(back, req);

        // capture=true ⇒ present and round-trips.
        let req2 = Request::Ingest {
            session: "s".into(),
            turns: vec![],
            namespace: None,
            capture: Some(true),
        };
        let line2 = encode_line(&req2).unwrap();
        assert!(line2.contains("\"capture\":true"), "got: {line2}");
        assert_eq!(decode_line::<Request>(&line2).unwrap(), req2);
    }
}
