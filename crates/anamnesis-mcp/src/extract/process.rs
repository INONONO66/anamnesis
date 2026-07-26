use std::fmt;
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use serde::Deserialize;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};

use super::config::ExtractCommand;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProcessOutput {
    pub(crate) stdout: Vec<u8>,
    pub(crate) duration: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutputStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProcessError {
    Spawn,
    Stdin,
    ProviderRequest,
    ProviderResponse,
    Timeout,
    OutputTooLarge {
        stream: OutputStream,
    },
    NonZero {
        code: Option<i32>,
        stderr_bytes: usize,
    },
    Wait,
}

impl fmt::Display for ProcessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn => formatter.write_str("could not start extraction provider"),
            Self::Stdin => formatter.write_str("could not write extraction provider stdin"),
            Self::ProviderRequest => {
                formatter.write_str("could not request the local extraction provider")
            }
            Self::ProviderResponse => {
                formatter.write_str("local extraction provider returned an invalid response")
            }
            Self::Timeout => formatter.write_str("extraction provider timed out"),
            Self::OutputTooLarge {
                stream: OutputStream::Stdout,
            } => formatter.write_str("extraction provider stdout exceeded its output limit"),
            Self::OutputTooLarge {
                stream: OutputStream::Stderr,
            } => formatter.write_str("extraction provider stderr exceeded its output limit"),
            Self::NonZero { code, stderr_bytes } => write!(
                formatter,
                "extraction provider exited unsuccessfully ({code:?}; {stderr_bytes} stderr bytes)"
            ),
            Self::Wait => formatter.write_str("could not collect extraction provider output"),
        }
    }
}

impl std::error::Error for ProcessError {}

/// Executes exactly the configured extractor argv without a shell.
pub(super) async fn run_provider(
    command: &ExtractCommand,
    prompt: &[u8],
    timeout: Duration,
    output_limit: usize,
) -> Result<ProcessOutput, ProcessError> {
    if is_ollama_structured_run(command) {
        return run_ollama_provider(command, prompt, timeout, output_limit).await;
    }
    run_subprocess_provider(command, prompt, timeout, output_limit).await
}

async fn run_subprocess_provider(
    command: &ExtractCommand,
    prompt: &[u8],
    timeout: Duration,
    output_limit: usize,
) -> Result<ProcessOutput, ProcessError> {
    let started = Instant::now();
    let mut process = Command::new(&command.program);
    process.args(&command.args);
    process
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .process_group(0);

    let mut child = process.spawn().map_err(|_| ProcessError::Spawn)?;
    let child_pid = child.id().ok_or(ProcessError::Wait)?;
    let process_group = i32::try_from(child_pid).map_err(|_| ProcessError::Wait)?;
    let stdin = child.stdin.take();
    let stdout = child.stdout.take().ok_or(ProcessError::Wait)?;
    let stderr = child.stderr.take().ok_or(ProcessError::Wait)?;

    let write_stdin = async {
        let Some(mut stdin) = stdin else {
            return Ok(());
        };
        stdin
            .write_all(prompt)
            .await
            .map_err(|_| ProcessError::Stdin)?;
        stdin.shutdown().await.map_err(|_| ProcessError::Stdin)
    };
    tokio::pin!(write_stdin);
    let mut stdout_task = tokio::spawn(read_capped(stdout, output_limit, OutputStream::Stdout));
    let mut stderr_task = tokio::spawn(read_capped(stderr, output_limit, OutputStream::Stderr));
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    let mut process_poll = tokio::time::interval(Duration::from_millis(1));

    let mut stdin_result = None;
    let mut stdout_result = None;
    let mut stderr_result = None;
    let mut status = None;
    loop {
        if stdin_result.is_some()
            && stdout_result.is_some()
            && stderr_result.is_some()
            && status.is_some()
        {
            break;
        }

        tokio::select! {
            biased;

            stdout = &mut stdout_task, if stdout_result.is_none() => {
                let stdout = stdout
                    .map_err(|_| ProcessError::Wait)
                    .and_then(|result| result);
                match stdout {
                    Err(error @ ProcessError::OutputTooLarge { .. }) => {
                        stderr_task.abort();
                        kill_process_group_and_reap(&mut child, process_group).await?;
                        return Err(error);
                    }
                    result => stdout_result = Some(result),
                }
            }
            stderr = &mut stderr_task, if stderr_result.is_none() => {
                let stderr = stderr
                    .map_err(|_| ProcessError::Wait)
                    .and_then(|result| result);
                match stderr {
                    Err(error @ ProcessError::OutputTooLarge { .. }) => {
                        stdout_task.abort();
                        kill_process_group_and_reap(&mut child, process_group).await?;
                        return Err(error);
                    }
                    result => stderr_result = Some(result),
                }
            }
            stdin = &mut write_stdin, if stdin_result.is_none() => {
                stdin_result = Some(stdin);
            }
            _ = process_poll.tick(), if status.is_none() => {
                match child.try_wait() {
                    Ok(Some(result)) => status = Some(result),
                    Ok(None) => {}
                    Err(_) => {
                        stdout_task.abort();
                        stderr_task.abort();
                        return match kill_process_group_and_reap(&mut child, process_group).await {
                            Ok(()) => Err(ProcessError::Wait),
                            Err(error) => Err(error),
                        };
                    }
                }
            }
            _ = &mut deadline => {
                stdout_task.abort();
                stderr_task.abort();
                return match kill_process_group_and_reap(&mut child, process_group).await {
                    Ok(()) => Err(ProcessError::Timeout),
                    Err(error) => Err(error),
                };
            }
        }
    }

    let (Some(status), Some(stdin_result), Some(stdout_result), Some(stderr_result)) =
        (status, stdin_result, stdout_result, stderr_result)
    else {
        return Err(ProcessError::Wait);
    };
    if !status.success() {
        return Err(ProcessError::NonZero {
            code: status.code(),
            stderr_bytes: stderr_result.as_ref().map_or(0, Vec::len),
        });
    }
    stdin_result?;
    let stdout = stdout_result?;
    let _stderr = stderr_result?;

    Ok(ProcessOutput {
        stdout,
        duration: started.elapsed(),
    })
}

fn is_ollama_structured_run(command: &ExtractCommand) -> bool {
    command
        .program
        .rsplit(['/', '\\'])
        .next()
        .is_some_and(|program| program == "ollama")
        && command.args.len() == 5
        && command.args.first().is_some_and(|value| value == "run")
        && command
            .args
            .get(2)
            .is_some_and(|value| value == "--think=false")
        && command.args.get(3).is_some_and(|value| value == "--format")
}

#[derive(Deserialize)]
struct OllamaChatResponse {
    message: OllamaMessage,
}

#[derive(Deserialize)]
struct OllamaMessage {
    content: String,
}

async fn run_ollama_provider(
    command: &ExtractCommand,
    prompt: &[u8],
    timeout: Duration,
    output_limit: usize,
) -> Result<ProcessOutput, ProcessError> {
    let host = std::env::var("OLLAMA_HOST").unwrap_or_else(|_| "127.0.0.1:11434".to_owned());
    let base_url = normalize_ollama_host(&host)?;
    run_ollama_provider_at(&base_url, command, prompt, timeout, output_limit).await
}

async fn run_ollama_provider_at(
    base_url: &str,
    command: &ExtractCommand,
    prompt: &[u8],
    timeout: Duration,
    output_limit: usize,
) -> Result<ProcessOutput, ProcessError> {
    let started = Instant::now();
    let model = command.args.get(1).ok_or(ProcessError::ProviderRequest)?;
    let output_schema = command
        .args
        .get(4)
        .ok_or(ProcessError::ProviderRequest)
        .and_then(|value| {
            serde_json::from_str::<serde_json::Value>(value)
                .map_err(|_| ProcessError::ProviderRequest)
        })?;
    let prompt = std::str::from_utf8(prompt).map_err(|_| ProcessError::ProviderRequest)?;
    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "stream": false,
        "think": false,
        "keep_alive": "10m",
        "format": output_schema,
        "options": {
            "temperature": 0,
            "seed": 42,
            "num_ctx": 32768,
            "num_predict": 8192
        }
    });
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(timeout)
        .build()
        .map_err(|_| ProcessError::ProviderRequest)?;
    let mut response = client
        .post(format!("{}/api/chat", base_url.trim_end_matches('/')))
        .json(&body)
        .send()
        .await
        .map_err(|error| {
            if error.is_timeout() {
                ProcessError::Timeout
            } else {
                ProcessError::ProviderRequest
            }
        })?;
    if !response.status().is_success() {
        return Err(ProcessError::NonZero {
            code: Some(i32::from(response.status().as_u16())),
            stderr_bytes: response
                .content_length()
                .and_then(|value| usize::try_from(value).ok())
                .unwrap_or(0),
        });
    }

    let response_limit = output_limit
        .checked_add(64 * 1024)
        .ok_or(ProcessError::ProviderResponse)?;
    let mut response_body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| ProcessError::ProviderResponse)?
    {
        if response_body.len().saturating_add(chunk.len()) > response_limit {
            return Err(ProcessError::OutputTooLarge {
                stream: OutputStream::Stdout,
            });
        }
        response_body.extend_from_slice(&chunk);
    }
    let parsed = serde_json::from_slice::<OllamaChatResponse>(&response_body)
        .map_err(|_| ProcessError::ProviderResponse)?;
    let stdout = parsed.message.content.into_bytes();
    if stdout.len() > output_limit {
        return Err(ProcessError::OutputTooLarge {
            stream: OutputStream::Stdout,
        });
    }

    Ok(ProcessOutput {
        stdout,
        duration: started.elapsed(),
    })
}

fn normalize_ollama_host(host: &str) -> Result<String, ProcessError> {
    let host = host.trim().trim_end_matches('/');
    let normalized = if host.starts_with("http://") || host.starts_with("https://") {
        host.to_owned()
    } else {
        format!("http://{host}")
    };
    let parsed = reqwest::Url::parse(&normalized).map_err(|_| ProcessError::ProviderRequest)?;
    let local = parsed.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    });
    if parsed.scheme() != "http" || !local {
        return Err(ProcessError::ProviderRequest);
    }
    Ok(normalized)
}

async fn read_capped(
    mut stream: impl AsyncRead + Unpin,
    output_limit: usize,
    stream_kind: OutputStream,
) -> Result<Vec<u8>, ProcessError> {
    let read_limit = output_limit.checked_add(1).ok_or(ProcessError::Wait)?;
    let mut captured = Vec::with_capacity(output_limit);
    let mut chunk = [0_u8; 8192];

    loop {
        let remaining = read_limit.saturating_sub(captured.len());
        if remaining == 0 {
            return Err(ProcessError::OutputTooLarge {
                stream: stream_kind,
            });
        }
        let chunk_len = remaining.min(chunk.len());
        let read = stream
            .read(&mut chunk[..chunk_len])
            .await
            .map_err(|_| ProcessError::Wait)?;
        if read == 0 {
            return Ok(captured);
        }
        captured.extend_from_slice(&chunk[..read]);
        if captured.len() == read_limit {
            return Err(ProcessError::OutputTooLarge {
                stream: stream_kind,
            });
        }
    }
}

const CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);

async fn kill_process_group_and_reap(
    child: &mut Child,
    process_group: i32,
) -> Result<(), ProcessError> {
    match killpg(Pid::from_raw(process_group), Signal::SIGKILL) {
        Ok(()) | Err(Errno::ESRCH) => {}
        Err(_) => return Err(ProcessError::Wait),
    }
    if child.try_wait().map_err(|_| ProcessError::Wait)?.is_some() {
        return Ok(());
    }
    tokio::time::timeout(CLEANUP_TIMEOUT, child.wait())
        .await
        .map_err(|_| ProcessError::Wait)?
        .map_err(|_| ProcessError::Wait)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::path::PathBuf;
    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::{
        OutputStream, ProcessError, is_ollama_structured_run, normalize_ollama_host,
        run_ollama_provider_at, run_provider,
    };
    use crate::extract::config::ExtractCommand;

    const TEST_DEADLINE: Duration = Duration::from_secs(5);
    const SECRET_STDERR_MARKER: &str = "secret-stderr-marker-do-not-log";
    const OUTPUT_LIMIT: usize = 1024 * 1024;

    fn fixture_command(
        mode: &str,
        additional_args: impl IntoIterator<Item = PathBuf>,
    ) -> ExtractCommand {
        let fixture =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-extractor.sh");
        let mut args = vec![
            fixture
                .into_os_string()
                .into_string()
                .expect("UTF-8 fixture path"),
            mode.into(),
        ];
        args.extend(additional_args.into_iter().map(|arg| {
            arg.into_os_string()
                .into_string()
                .expect("UTF-8 fixture argument")
        }));
        ExtractCommand {
            program: "/bin/sh".into(),
            args,
        }
    }

    async fn completes_within<T>(future: impl Future<Output = T>) -> T {
        tokio::time::timeout(TEST_DEADLINE, future)
            .await
            .expect("process test exceeded five-second deadline")
    }

    #[test]
    fn only_the_default_structured_ollama_shape_uses_the_http_transport() {
        let structured = ExtractCommand {
            program: "/usr/local/bin/ollama".into(),
            args: vec![
                "run".into(),
                "qwen3.6:35b-a3b".into(),
                "--think=false".into(),
                "--format".into(),
                "{}".into(),
            ],
        };
        assert!(is_ollama_structured_run(&structured));

        let generic = ExtractCommand {
            program: "custom-provider".into(),
            args: vec!["--json".into()],
        };
        assert!(!is_ollama_structured_run(&generic));
    }

    #[test]
    fn ollama_host_normalization_accepts_standard_host_and_url_shapes() {
        assert_eq!(
            normalize_ollama_host("127.0.0.1:11434").expect("loopback host"),
            "http://127.0.0.1:11434"
        );
        assert_eq!(
            normalize_ollama_host(" http://localhost:11434/ ").expect("localhost URL"),
            "http://localhost:11434"
        );
        assert_eq!(
            normalize_ollama_host("https://127.0.0.1:11434"),
            Err(ProcessError::ProviderRequest)
        );
        assert_eq!(
            normalize_ollama_host("http://example.com:11434"),
            Err(ProcessError::ProviderRequest)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn structured_ollama_uses_non_streaming_chat_api_and_returns_message_content() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fixture server");
        let address = listener.local_addr().expect("fixture address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept fixture request");
            let mut request = Vec::new();
            let (header_end, content_length) = loop {
                let mut chunk = [0_u8; 4096];
                let read = socket.read(&mut chunk).await.expect("read request");
                assert!(read > 0, "request ended before HTTP body");
                request.extend_from_slice(&chunk[..read]);
                if let Some(header_end) = request
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .map(|position| position + 4)
                {
                    let headers =
                        std::str::from_utf8(&request[..header_end]).expect("UTF-8 headers");
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().expect("content length"))
                        })
                        .expect("content-length header");
                    break (header_end, content_length);
                }
            };
            while request.len() < header_end + content_length {
                let mut chunk = [0_u8; 4096];
                let read = socket.read(&mut chunk).await.expect("read request body");
                assert!(read > 0, "request body ended early");
                request.extend_from_slice(&chunk[..read]);
            }
            let body = serde_json::from_slice::<serde_json::Value>(
                &request[header_end..header_end + content_length],
            )
            .expect("request JSON");
            let response_body = br#"{"message":{"content":"{\"items\":[],\"relations\":[]}"}}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                response_body.len()
            );
            socket
                .write_all(response.as_bytes())
                .await
                .expect("write fixture response headers");
            socket
                .write_all(response_body)
                .await
                .expect("write fixture response body");
            body
        });
        let command = ExtractCommand {
            program: "ollama".into(),
            args: vec![
                "run".into(),
                "qwen3.6:35b-a3b".into(),
                "--think=false".into(),
                "--format".into(),
                r#"{"type":"object"}"#.into(),
            ],
        };

        let output = run_ollama_provider_at(
            &format!("http://{address}"),
            &command,
            b"extract this",
            Duration::from_secs(1),
            OUTPUT_LIMIT,
        )
        .await
        .expect("structured response");
        let request = server.await.expect("fixture server task");

        assert_eq!(output.stdout, br#"{"items":[],"relations":[]}"#);
        assert_eq!(request["model"], "qwen3.6:35b-a3b");
        assert_eq!(request["stream"], false);
        assert_eq!(request["think"], false);
        assert_eq!(request["messages"][0]["content"], "extract this");
        assert_eq!(request["format"]["type"], "object");
    }

    async fn assert_process_is_gone(pid: i32) {
        tokio::time::timeout(TEST_DEADLINE, async {
            loop {
                // SAFETY: signal 0 only probes this positive fixture PID.
                if unsafe { libc::kill(pid, 0) } == -1 {
                    let error = std::io::Error::last_os_error();
                    if error.raw_os_error() == Some(libc::ESRCH) {
                        return;
                    }
                    panic!("could not probe child process {pid}: {error}");
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("timed-out process group was not fully reaped within five seconds");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn r2_process_passes_stdin_returns_json_and_records_duration() {
        let output = completes_within(run_provider(
            &fixture_command("valid", []),
            b"{\"message\":\"hello\"}\n",
            Duration::from_secs(1),
            OUTPUT_LIMIT,
        ))
        .await
        .expect("valid extractor should succeed");

        assert_eq!(output.stdout, b"{\"received\":{\"message\":\"hello\"}}\n");
        assert!(!output.duration.is_zero());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn r2_process_nonzero_exit_exposes_only_typed_metadata() {
        let error = completes_within(run_provider(
            &fixture_command("nonzero", []),
            b"{}\n",
            Duration::from_secs(1),
            OUTPUT_LIMIT,
        ))
        .await
        .expect_err("nonzero extractor should fail");

        assert_eq!(
            error,
            ProcessError::NonZero {
                code: Some(7),
                stderr_bytes: SECRET_STDERR_MARKER.len(),
            }
        );
        assert!(!format!("{error}").contains(SECRET_STDERR_MARKER));
        assert!(!format!("{error:?}").contains(SECRET_STDERR_MARKER));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn r2_process_rejects_each_stream_over_its_independent_limit() {
        completes_within(async {
            let over_limit = OUTPUT_LIMIT * 4;
            for (script, stream) in [
                (
                    format!("dd if=/dev/zero bs={over_limit} count=1 2>/dev/null; exit 7"),
                    OutputStream::Stdout,
                ),
                (
                    format!("dd if=/dev/zero bs={over_limit} count=1 >&2 2>/dev/null; exit 7"),
                    OutputStream::Stderr,
                ),
            ] {
                let command = ExtractCommand {
                    program: "/bin/sh".into(),
                    args: vec!["-c".into(), script],
                };
                let error = run_provider(&command, b"{}\n", Duration::from_secs(1), OUTPUT_LIMIT)
                    .await
                    .expect_err("over-limit extractor should fail");
                assert_eq!(error, ProcessError::OutputTooLarge { stream });
            }
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn r2_process_nonzero_exit_precedes_simultaneous_stdin_close_failure() {
        let command = ExtractCommand {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "sleep 0.05; exit 7".into()],
        };
        let prompt = vec![b'x'; 16 * 1024 * 1024];

        let error = completes_within(run_provider(
            &command,
            &prompt,
            Duration::from_secs(1),
            OUTPUT_LIMIT,
        ))
        .await
        .expect_err("early-close provider should fail");

        assert_eq!(
            error,
            ProcessError::NonZero {
                code: Some(7),
                stderr_bytes: 0,
            }
        );
    }
    #[tokio::test(flavor = "multi_thread")]
    async fn r2_process_timeout_kills_and_reaps_the_entire_process_group() {
        let tempdir = tempfile::tempdir().expect("temporary pidfile directory");
        let pidfile = tempdir.path().join("background.pid");
        completes_within(async {
            let error = run_provider(
                &fixture_command("timeout", [pidfile.clone()]),
                b"{}\n",
                Duration::from_millis(100),
                OUTPUT_LIMIT,
            )
            .await
            .expect_err("timeout extractor should fail");

            assert_eq!(error, ProcessError::Timeout);
            let pids = std::fs::read_to_string(&pidfile)
                .expect("timed-out fixture wrote parent and background PIDs")
                .lines()
                .map(|pid| pid.parse::<i32>().expect("fixture PID is an i32"))
                .collect::<Vec<_>>();
            assert_eq!(pids.len(), 2, "fixture wrote parent and background PIDs");
            assert!(
                pids.iter().all(|pid| *pid > 0),
                "fixture PIDs must be positive"
            );
            for pid in pids {
                assert_process_is_gone(pid).await;
            }
        })
        .await;
    }
}
