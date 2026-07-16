// Task 4 stages the process runner; Task 7 wires its sole worker call site.
#![cfg_attr(
    not(test),
    allow(dead_code, reason = "Task 4 staged runner is consumed by Task 7")
)]

use std::fmt;
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
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
    let started = Instant::now();
    let mut process = Command::new(&command.program);
    process
        .args(&command.args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .process_group(0);

    let mut child = process.spawn().map_err(|_| ProcessError::Spawn)?;
    let child_pid = child.id().ok_or(ProcessError::Wait)?;
    let process_group = i32::try_from(child_pid).map_err(|_| ProcessError::Wait)?;
    let stdin = child.stdin.take().ok_or(ProcessError::Wait)?;
    let stdout = child.stdout.take().ok_or(ProcessError::Wait)?;
    let stderr = child.stderr.take().ok_or(ProcessError::Wait)?;

    let result = tokio::time::timeout(timeout, async {
        let write_stdin = async {
            let mut stdin = stdin;
            stdin
                .write_all(prompt)
                .await
                .map_err(|_| ProcessError::Stdin)?;
            stdin.shutdown().await.map_err(|_| ProcessError::Stdin)
        };
        let read_stdout = read_capped(stdout, output_limit, OutputStream::Stdout);
        let read_stderr = read_capped(stderr, output_limit, OutputStream::Stderr);
        let wait = async { child.wait().await.map_err(|_| ProcessError::Wait) };
        let ((), stdout, stderr, status) =
            tokio::try_join!(write_stdin, read_stdout, read_stderr, wait)?;
        Ok::<_, ProcessError>((status, stdout, stderr))
    })
    .await;

    let (status, stdout, stderr) = match result {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            kill_process_group_and_reap(&mut child, process_group).await;
            return Err(error);
        }
        Err(_) => {
            kill_process_group_and_reap(&mut child, process_group).await;
            return Err(ProcessError::Timeout);
        }
    };

    if status.success() {
        Ok(ProcessOutput {
            stdout,
            duration: started.elapsed(),
        })
    } else {
        Err(ProcessError::NonZero {
            code: status.code(),
            stderr_bytes: stderr.len(),
        })
    }
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

async fn kill_process_group_and_reap(child: &mut Child, process_group: i32) {
    match killpg(Pid::from_raw(process_group), Signal::SIGKILL) {
        Ok(()) | Err(Errno::ESRCH) => {}
        Err(_) => {}
    }
    let _ = child.wait().await;
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::path::PathBuf;
    use std::time::Duration;

    use super::{OutputStream, ProcessError, run_provider};
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
            for (mode, stream) in [
                ("large-stdout", OutputStream::Stdout),
                ("large-stderr", OutputStream::Stderr),
            ] {
                let error = run_provider(
                    &fixture_command(mode, []),
                    b"{}\n",
                    Duration::from_secs(1),
                    OUTPUT_LIMIT,
                )
                .await
                .expect_err("large fixture should exceed its stream limit");
                assert_eq!(error, ProcessError::OutputTooLarge { stream });
            }
        })
        .await;
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
