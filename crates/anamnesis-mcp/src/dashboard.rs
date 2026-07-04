//! Local read-only observability/management dashboard — a NEW daemon client.
//!
//! Like the `serve` MCP adapter (ADR-0012), the dashboard is just another
//! bespoke daemon client: it NEVER opens the SQLite DB directly (the daemon
//! holds the single-writer lock). It ensures the shared daemon is up
//! ([`DaemonClient::connect`] → [`crate::launcher::ensure_daemon`]) and forwards
//! browser requests to it as [`crate::proto::Request`]s (`List`/`Get`/`Stats`/`Forget`).
//!
//! The HTTP surface is a MINIMAL, local-only [`tiny_http`] server bound to
//! `127.0.0.1` (no auth — a personal, single-user inspector). One embedded HTML
//! page (`include_str!`, no build step) drives a small JSON API:
//!
//! | Method + path                       | Daemon op | Shape                     |
//! |-------------------------------------|-----------|---------------------------|
//! | `GET  /`                            | —         | embedded HTML             |
//! | `GET  /api/memories`                | `List`    | JSON array (passthrough)  |
//! | `GET  /api/memory/{id}`             | `Get`     | JSON object (passthrough) |
//! | `GET  /api/graph`                   | `Graph`   | JSON object (passthrough) |
//! | `GET  /api/stats`                   | `Stats`   | `{"stats": "<text>"}`     |
//! | `POST /api/memory/{id}/forget`      | `Forget`  | `{"ok": true, ...}`       |
//!
//! `List`/`Get` already emit JSON from the daemon (see `dispatch::render`), so
//! those responses pass through verbatim; only `Stats` (human text) is wrapped.
//!
//! The router ([`route`]) is a pure function over a [`Daemon`] abstraction so the
//! request→[`crate::proto::Request`]→HTTP-shape mapping is unit-tested with a stub, and
//! the real server bridges to the async [`DaemonClient`] through a small
//! blocking [`DaemonBridge`].

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Mutex;

use anyhow::{Context, Result, anyhow};
use tiny_http::{Header, Response as HttpResponse, Server};

use crate::client::DaemonClient;
use crate::config::Config;
use crate::proto::{ErrKind, Request, Response};

/// The single embedded UI — one static HTML+CSS+vanilla-JS file, no build step.
const DASHBOARD_HTML: &str = include_str!("dashboard.html");

/// A synchronous abstraction over one daemon round-trip.
///
/// The pure [`route`] logic depends only on this, so it is unit-testable with a
/// stub; the real server implements it over the async [`DaemonClient`] via
/// [`DaemonBridge`]. A transport failure is surfaced as an internal-kind
/// [`Response`] so the router treats "daemon unreachable" like any other 500.
trait Daemon {
    fn call(&self, req: Request) -> Response;
}

/// A fully-shaped HTTP reply: status, content type, and body string. Kept
/// transport-agnostic so [`route`] can be asserted without a live socket.
struct HttpReply {
    status: u16,
    content_type: &'static str,
    body: String,
}

impl HttpReply {
    fn json(status: u16, body: String) -> Self {
        Self {
            status,
            content_type: "application/json",
            body,
        }
    }

    fn html(body: &str) -> Self {
        Self {
            status: 200,
            content_type: "text/html; charset=utf-8",
            body: body.to_string(),
        }
    }

    /// A JSON error envelope: `{"error": "<message>"}`.
    fn error(status: u16, message: &str) -> Self {
        let body = serde_json::json!({ "error": message }).to_string();
        Self::json(status, body)
    }

    fn method_not_allowed() -> Self {
        Self::error(405, "method not allowed")
    }
}

struct Incoming<'a> {
    method: &'a str,
    path: &'a str,
    query: &'a HashMap<String, String>,
}

/// Route one request to a [`crate::proto::Request`], call the daemon, and shape the
/// reply. Pure over `daemon` — the whole HTTP contract is exercised in tests
/// through a stub. `default_namespace` is the dashboard's `--namespace` flag; a
/// per-request `?namespace=` query overrides it.
fn route(req: &Incoming, default_namespace: Option<&str>, daemon: &dyn Daemon) -> HttpReply {
    let namespace = effective_namespace(req.query, default_namespace);
    let method = req.method;
    let segments: Vec<&str> = req.path.split('/').filter(|s| !s.is_empty()).collect();

    match segments.as_slice() {
        [] => require(method, "GET", || HttpReply::html(DASHBOARD_HTML)),

        ["api", "memories"] => require(method, "GET", || {
            let list = Request::List {
                min_salience: req.query.get("min_salience").and_then(|s| s.parse().ok()),
                limit: req.query.get("limit").and_then(|s| s.parse().ok()),
                node_type: None,
                tag: None,
                namespace,
                scope: None,
                metadata: None,
            };
            json_passthrough(daemon.call(list))
        }),

        ["api", "stats"] => require(method, "GET", || {
            match daemon.call(Request::Stats { namespace }) {
                Response::Ok { text } => {
                    HttpReply::json(200, serde_json::json!({ "stats": text }).to_string())
                }
                Response::Err { kind, message } => error_reply(kind, &message),
            }
        }),

        ["api", "graph"] => require(method, "GET", || {
            let seed = req.query.get("seed").and_then(|s| s.parse::<u64>().ok());
            let seeds_csv = req.query.get("seeds").and_then(|s| parse_csv_ids(s));
            let seeds = seed.map(|s| vec![s]).or(seeds_csv);
            let query_text = req
                .query
                .get("q")
                .map(String::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            if seeds.is_none() && query_text.is_none() {
                return HttpReply::error(400, "graph requires seed(s) or q");
            }
            let graph = Request::Graph {
                seeds,
                query: query_text,
                depth: req.query.get("depth").and_then(|s| s.parse().ok()),
                limit: req.query.get("limit").and_then(|s| s.parse().ok()),
                namespace,
            };
            json_passthrough(daemon.call(graph))
        }),

        ["api", "memory", id] => require(method, "GET", || match parse_id(id) {
            Some(id) => json_passthrough(daemon.call(Request::Get { id, namespace })),
            None => HttpReply::error(400, "invalid node id"),
        }),

        ["api", "memory", id, "forget"] => require(method, "POST", || match parse_id(id) {
            Some(id) => {
                let forget = Request::Forget {
                    id,
                    reason: Some("forgotten via dashboard".to_string()),
                    hard: Some(false),
                    namespace,
                };
                match daemon.call(forget) {
                    Response::Ok { text } => HttpReply::json(
                        200,
                        serde_json::json!({ "ok": true, "message": text }).to_string(),
                    ),
                    Response::Err { kind, message } => error_reply(kind, &message),
                }
            }
            None => HttpReply::error(400, "invalid node id"),
        }),

        _ => HttpReply::error(404, "not found"),
    }
}

fn require(method: &str, allowed: &str, handler: impl FnOnce() -> HttpReply) -> HttpReply {
    if method == allowed {
        handler()
    } else {
        HttpReply::method_not_allowed()
    }
}

fn parse_id(s: &str) -> Option<u64> {
    s.parse::<u64>().ok()
}

/// Parse a comma-separated list of `u64` ids (`"1,2,3"`). Returns `None` if the
/// string is empty or any entry fails to parse — a partially-malformed list is
/// rejected wholesale rather than silently dropping the bad entries.
fn parse_csv_ids(raw: &str) -> Option<Vec<u64>> {
    if raw.is_empty() {
        return None;
    }
    raw.split(',').map(|s| s.parse::<u64>().ok()).collect()
}

/// Pass a daemon reply straight through as JSON (`List`/`Get` already emit JSON),
/// mapping a daemon error to the right HTTP status.
fn json_passthrough(resp: Response) -> HttpReply {
    match resp {
        Response::Ok { text } => HttpReply::json(200, text),
        Response::Err { kind, message } => error_reply(kind, &message),
    }
}

/// Map a daemon [`ErrKind`] to an HTTP error: a caller fault (a bad/missing node
/// id) → 404, an internal fault → 500.
fn error_reply(kind: ErrKind, message: &str) -> HttpReply {
    let status = match kind {
        ErrKind::InvalidParams => 404,
        ErrKind::Internal => 500,
    };
    HttpReply::error(status, message)
}

/// Resolve the effective namespace: a non-empty `?namespace=` query wins,
/// otherwise the dashboard's `--namespace` default (or `None`).
fn effective_namespace(
    query: &HashMap<String, String>,
    default_namespace: Option<&str>,
) -> Option<String> {
    query
        .get("namespace")
        .map(String::as_str)
        .filter(|s| !s.is_empty())
        .or(default_namespace)
        .map(str::to_string)
}

/// Split a request URL into `(path, raw_query)` on the first `?`.
fn split_path_query(url: &str) -> (&str, &str) {
    match url.split_once('?') {
        Some((p, q)) => (p, q),
        None => (url, ""),
    }
}

/// Parse a raw `key=value&…` query string into a map, percent-decoding both
/// sides. A bare `key` (no `=`) maps to an empty value.
fn parse_query(raw: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for pair in raw.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        map.insert(percent_decode(k), percent_decode(v));
    }
    map
}

/// Minimal `application/x-www-form-urlencoded` decode: `%XX` → byte, `+` →
/// space; a malformed escape is left verbatim. Enough for query values (node
/// ids, namespaces, numbers) without pulling in a URL crate.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                match (hi, lo) {
                    (Some(h), Some(l)) => {
                        out.push((h * 16 + l) as u8);
                        i += 3;
                    }
                    _ => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The real [`Daemon`]: a persistent [`DaemonClient`] driven on a private
/// current-thread tokio runtime. The client is held for the dashboard's whole
/// lifetime (keeping the shared daemon warm); a dropped connection (daemon
/// grace-exit) is transparently reconnected once per call.
struct DaemonBridge {
    rt: tokio::runtime::Runtime,
    client: Mutex<DaemonClient>,
    cfg: Config,
}

impl Daemon for DaemonBridge {
    fn call(&self, req: Request) -> Response {
        let mut client = self.client.lock().unwrap_or_else(|p| p.into_inner());
        match self.rt.block_on(client.call(&req)) {
            Ok(resp) => resp,
            Err(_) => match self
                .rt
                .block_on(reconnect_and_call(&self.cfg, &mut client, &req))
            {
                Ok(resp) => resp,
                Err(e) => Response::internal(format!("daemon unreachable: {e}")),
            },
        }
    }
}

/// Re-establish the daemon connection and retry one request — the recovery path
/// when the persistent connection dropped (e.g. the daemon grace-exited while
/// the dashboard idled).
async fn reconnect_and_call(
    cfg: &Config,
    client: &mut DaemonClient,
    req: &Request,
) -> Result<Response> {
    *client = DaemonClient::connect(cfg).await?;
    client.call(req).await
}

/// Turn an [`HttpReply`] into a `tiny_http` response. The `Content-Type` header
/// is built from static, always-valid bytes; if that ever failed we simply omit
/// it (browsers fall back to `text/plain`) rather than panic.
fn build_response(reply: HttpReply) -> HttpResponse<Cursor<Vec<u8>>> {
    let mut resp = HttpResponse::from_string(reply.body).with_status_code(reply.status);
    if let Ok(header) = Header::from_bytes(b"Content-Type".as_ref(), reply.content_type.as_bytes())
    {
        resp = resp.with_header(header);
    }
    resp
}

/// Run the dashboard HTTP server on `127.0.0.1:<port>` until the process is
/// terminated. `port == 0` binds an OS-assigned free port. Connects to the
/// shared daemon first (spawning it if needed), prints the resolved URL to
/// stderr, then serves the blocking accept loop.
pub fn run(cfg: &Config, port: u16, namespace: Option<String>) -> Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build dashboard tokio runtime")?;
    let client = rt
        .block_on(DaemonClient::connect(cfg))
        .context("connect dashboard to the anamnesis daemon")?;
    let bridge = DaemonBridge {
        rt,
        client: Mutex::new(client),
        cfg: cfg.clone(),
    };

    let server = Server::http(("127.0.0.1", port))
        .map_err(|e| anyhow!("bind dashboard HTTP server on 127.0.0.1:{port}: {e}"))?;
    let bound_port = server
        .server_addr()
        .to_ip()
        .map(|addr| addr.port())
        .unwrap_or(port);
    // The one permitted stderr print (constraint): the resolved URL on startup.
    eprintln!("anamnesis dashboard: http://127.0.0.1:{bound_port}");
    tracing::info!(
        port = bound_port,
        "anamnesis dashboard serving (read-only, local)"
    );

    let default_ns = namespace.as_deref();
    for request in server.incoming_requests() {
        let method = request.method().as_str().to_string();
        let url = request.url().to_string();
        let (path, raw_query) = split_path_query(&url);
        let query = parse_query(raw_query);
        let incoming = Incoming {
            method: &method,
            path,
            query: &query,
        };
        let reply = route(&incoming, default_ns, &bridge);
        if let Err(e) = request.respond(build_response(reply)) {
            tracing::debug!("dashboard: failed to write response: {e}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stub [`Daemon`] that records every request it sees and always returns a
    /// canned response — lets [`route`] be asserted with zero I/O.
    struct StubDaemon {
        canned: Response,
        seen: Mutex<Vec<Request>>,
    }

    impl StubDaemon {
        fn new(canned: Response) -> Self {
            Self {
                canned,
                seen: Mutex::new(Vec::new()),
            }
        }
        fn last(&self) -> Request {
            self.seen
                .lock()
                .unwrap()
                .last()
                .cloned()
                .expect("a request was recorded")
        }
        fn called(&self) -> bool {
            !self.seen.lock().unwrap().is_empty()
        }
    }

    impl Daemon for StubDaemon {
        fn call(&self, req: Request) -> Response {
            self.seen.lock().unwrap().push(req);
            self.canned.clone()
        }
    }

    fn q(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn send(d: &dyn Daemon, method: &str, path: &str) -> HttpReply {
        route(
            &Incoming {
                method,
                path,
                query: &HashMap::new(),
            },
            None,
            d,
        )
    }

    #[test]
    fn get_root_serves_html_without_calling_daemon() {
        let d = StubDaemon::new(Response::ok("unused"));
        let r = send(&d, "GET", "/");
        assert_eq!(r.status, 200);
        assert!(r.content_type.starts_with("text/html"));
        assert!(r.body.contains("Anamnesis"), "embedded page renders");
        assert!(!d.called(), "the index page must not hit the daemon");
    }

    #[test]
    fn get_memories_calls_list_and_passes_json_through() {
        let d = StubDaemon::new(Response::ok("[{\"node_id\":1},{\"node_id\":2}]"));
        let query = q(&[
            ("min_salience", "0.25"),
            ("limit", "10"),
            ("namespace", "proj"),
        ]);
        let r = route(
            &Incoming {
                method: "GET",
                path: "/api/memories",
                query: &query,
            },
            None,
            &d,
        );
        assert_eq!(r.status, 200);
        assert_eq!(r.content_type, "application/json");
        assert_eq!(r.body, "[{\"node_id\":1},{\"node_id\":2}]");
        match d.last() {
            Request::List {
                min_salience,
                limit,
                namespace,
                node_type,
                tag,
                scope,
                metadata,
            } => {
                assert_eq!(min_salience, Some(0.25));
                assert_eq!(limit, Some(10));
                assert_eq!(namespace.as_deref(), Some("proj"));
                assert!(
                    node_type.is_none() && tag.is_none() && scope.is_none() && metadata.is_none()
                );
            }
            other => panic!("expected List, got {other:?}"),
        }
    }

    #[test]
    fn get_memory_by_id_calls_get() {
        let d = StubDaemon::new(Response::ok("{\"node_id\":7,\"content\":\"hi\"}"));
        let r = send(&d, "GET", "/api/memory/7");
        assert_eq!(r.status, 200);
        assert_eq!(r.content_type, "application/json");
        assert_eq!(r.body, "{\"node_id\":7,\"content\":\"hi\"}");
        assert_eq!(
            d.last(),
            Request::Get {
                id: 7,
                namespace: None
            }
        );
    }

    #[test]
    fn non_numeric_id_is_400_and_skips_daemon() {
        let d = StubDaemon::new(Response::ok("unused"));
        let r = send(&d, "GET", "/api/memory/not-a-number");
        assert_eq!(r.status, 400);
        assert!(!d.called(), "a malformed id must never reach the daemon");
    }

    #[test]
    fn missing_memory_maps_invalid_params_to_404() {
        let d = StubDaemon::new(Response::invalid_params("no node 999"));
        let r = send(&d, "GET", "/api/memory/999");
        assert_eq!(r.status, 404);
        assert_eq!(
            d.last(),
            Request::Get {
                id: 999,
                namespace: None
            }
        );
    }

    #[test]
    fn internal_error_maps_to_500() {
        let d = StubDaemon::new(Response::internal("boom"));
        let r = send(&d, "GET", "/api/memories");
        assert_eq!(r.status, 500);
    }

    #[test]
    fn get_stats_wraps_daemon_text_as_json() {
        let text = "nodes:                2\nedges:                0\nhealth grade:         Fair";
        let d = StubDaemon::new(Response::ok(text));
        let r = send(&d, "GET", "/api/stats");
        assert_eq!(r.status, 200);
        assert_eq!(r.content_type, "application/json");
        let v: serde_json::Value = serde_json::from_str(&r.body).expect("stats body is valid JSON");
        assert_eq!(v["stats"], text);
        assert_eq!(d.last(), Request::Stats { namespace: None });
    }

    #[test]
    fn post_forget_sends_soft_forget_and_wraps_result() {
        let d = StubDaemon::new(Response::ok("forgot node 5 (soft delete)"));
        let r = send(&d, "POST", "/api/memory/5/forget");
        assert_eq!(r.status, 200);
        let v: serde_json::Value = serde_json::from_str(&r.body).expect("forget body is JSON");
        assert_eq!(v["ok"], true);
        assert_eq!(v["message"], "forgot node 5 (soft delete)");
        match d.last() {
            Request::Forget { id, hard, .. } => {
                assert_eq!(id, 5);
                assert_eq!(hard, Some(false), "dashboard forget is a soft-retract");
            }
            other => panic!("expected Forget, got {other:?}"),
        }
    }

    #[test]
    fn wrong_method_on_known_route_is_405() {
        let d = StubDaemon::new(Response::ok("unused"));
        assert_eq!(send(&d, "POST", "/api/memories").status, 405);
        assert_eq!(send(&d, "GET", "/api/memory/5/forget").status, 405);
        assert!(!d.called(), "a method mismatch must not reach the daemon");
    }

    #[test]
    fn unknown_route_is_404() {
        let d = StubDaemon::new(Response::ok("unused"));
        assert_eq!(send(&d, "GET", "/api/nope").status, 404);
    }

    #[test]
    fn get_graph_by_seed_calls_graph_op_and_passes_json_through() {
        let d = StubDaemon::new(Response::ok(
            "{\"schema\":1,\"seed_ids\":[5],\"truncated\":false,\"nodes\":[],\"edges\":[]}",
        ));
        let query = q(&[("seed", "5"), ("depth", "1"), ("limit", "100")]);
        let r = route(
            &Incoming {
                method: "GET",
                path: "/api/graph",
                query: &query,
            },
            None,
            &d,
        );
        assert_eq!(r.status, 200);
        assert_eq!(r.content_type, "application/json");
        assert_eq!(
            r.body,
            "{\"schema\":1,\"seed_ids\":[5],\"truncated\":false,\"nodes\":[],\"edges\":[]}"
        );
        match d.last() {
            Request::Graph {
                seeds,
                query,
                depth,
                limit,
                namespace,
            } => {
                assert_eq!(seeds, Some(vec![5]));
                assert_eq!(query, None);
                assert_eq!(depth, Some(1));
                assert_eq!(limit, Some(100));
                assert_eq!(namespace, None);
            }
            other => panic!("expected Graph, got {other:?}"),
        }
    }

    #[test]
    fn get_graph_by_query_passes_query() {
        let d = StubDaemon::new(Response::ok("{\"schema\":1}"));
        let query = q(&[("q", "wombat")]);
        let r = route(
            &Incoming {
                method: "GET",
                path: "/api/graph",
                query: &query,
            },
            None,
            &d,
        );
        assert_eq!(r.status, 200);
        match d.last() {
            Request::Graph { seeds, query, .. } => {
                assert_eq!(seeds, None);
                assert_eq!(query, Some("wombat".to_string()));
            }
            other => panic!("expected Graph, got {other:?}"),
        }
    }

    #[test]
    fn get_graph_without_seed_or_q_is_400() {
        let d = StubDaemon::new(Response::ok("unused"));
        let r = send(&d, "GET", "/api/graph");
        assert_eq!(r.status, 400);
        assert!(!d.called(), "no seed/q must never reach the daemon");
    }

    #[test]
    fn get_graph_wrong_method_is_405() {
        let d = StubDaemon::new(Response::ok("unused"));
        let r = send(&d, "POST", "/api/graph");
        assert_eq!(r.status, 405);
        assert!(!d.called());
    }

    #[test]
    fn graph_seeds_csv_parses() {
        let d = StubDaemon::new(Response::ok("{\"schema\":1}"));
        let query = q(&[("seeds", "1,2,3")]);
        let r = route(
            &Incoming {
                method: "GET",
                path: "/api/graph",
                query: &query,
            },
            None,
            &d,
        );
        assert_eq!(r.status, 200);
        match d.last() {
            Request::Graph { seeds, .. } => assert_eq!(seeds, Some(vec![1, 2, 3])),
            other => panic!("expected Graph, got {other:?}"),
        }
    }

    #[test]
    fn namespace_default_then_query_override() {
        let d = StubDaemon::new(Response::ok("[]"));
        let list_ns = |query: &HashMap<String, String>| {
            route(
                &Incoming {
                    method: "GET",
                    path: "/api/memories",
                    query,
                },
                Some("cli-ns"),
                &d,
            );
            match d.last() {
                Request::List { namespace, .. } => namespace,
                other => panic!("expected List, got {other:?}"),
            }
        };
        assert_eq!(
            list_ns(&q(&[])).as_deref(),
            Some("cli-ns"),
            "CLI default namespace applies with no query override"
        );
        assert_eq!(
            list_ns(&q(&[("namespace", "override")])).as_deref(),
            Some("override"),
            "a query namespace overrides the CLI default"
        );
        assert_eq!(
            list_ns(&q(&[("namespace", "")])).as_deref(),
            Some("cli-ns"),
            "an empty query namespace falls back to the CLI default"
        );
    }

    #[test]
    fn parse_query_decodes_pairs() {
        let m = parse_query("a=1&b=hello%20world&c=&d");
        assert_eq!(m.get("a").map(String::as_str), Some("1"));
        assert_eq!(m.get("b").map(String::as_str), Some("hello world"));
        assert_eq!(m.get("c").map(String::as_str), Some(""));
        assert_eq!(m.get("d").map(String::as_str), Some(""));
    }

    #[test]
    fn split_path_query_splits_on_question_mark() {
        assert_eq!(
            split_path_query("/api/memories?x=1"),
            ("/api/memories", "x=1")
        );
        assert_eq!(split_path_query("/api/stats"), ("/api/stats", ""));
    }
}
