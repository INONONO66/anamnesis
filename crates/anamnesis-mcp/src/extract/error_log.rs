use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use sha2::{Digest, Sha256};

pub(crate) const EXTRACT_LOG_CAP_BYTES: usize = 256 * 1024;

/// The only daemon startup failure that may be recorded outside the daemon.
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ErrorLogKind {
    Connect,
}

#[derive(Serialize)]
struct ErrorLogEntry<'a> {
    timestamp: u64,
    kind: ErrorLogKind,
    socket_path_hash: &'a str,
}

/// Appends a redacted daemon connection failure record.
///
/// This is intentionally limited to failures that occur before a daemon can
/// accept `RecordExtractionFailure`; daemon-reported extraction failures must
/// be sent to the daemon instead.
pub(crate) fn append_connect_failure(
    db_dir: &Path,
    socket_path: &Path,
    kind: ErrorLogKind,
) -> io::Result<()> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| io::Error::other("system time precedes Unix epoch"))?
        .as_millis();
    let timestamp = u64::try_from(timestamp)
        .map_err(|_| io::Error::other("timestamp cannot be represented"))?;

    let socket_path_hash = format!(
        "{:x}",
        Sha256::digest(socket_path.as_os_str().as_encoded_bytes())
    );
    let entry = ErrorLogEntry {
        timestamp,
        kind,
        socket_path_hash: &socket_path_hash,
    };
    let mut line = serde_json::to_vec(&entry)
        .map_err(|_| io::Error::other("failed to serialize error log entry"))?;
    line.push(b'\n');

    rotate_before_append(db_dir, line.len())?;

    let log_path = db_dir.join("extract.log");
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    log.write_all(&line)
}

fn rotate_before_append(db_dir: &Path, append_len: usize) -> io::Result<()> {
    if append_len > EXTRACT_LOG_CAP_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "error log entry exceeds capacity",
        ));
    }

    let current = db_dir.join("extract.log");
    let current_len = match fs::metadata(&current) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let cap = u64::try_from(EXTRACT_LOG_CAP_BYTES)
        .map_err(|_| io::Error::other("error log capacity cannot be represented"))?;
    let append_len = u64::try_from(append_len)
        .map_err(|_| io::Error::other("error log entry length cannot be represented"))?;

    if current_len <= cap.saturating_sub(append_len) {
        return Ok(());
    }

    let rotated = db_dir.join("extract.log.1");
    match fs::remove_file(&rotated) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    fs::rename(current, rotated)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use serde_json::Value;

    use super::{EXTRACT_LOG_CAP_BYTES, ErrorLogKind, append_connect_failure};

    const SECRET: &str = "secret-marker-never-in-extract-log";
    const COMMAND: &str = "provider-command-never-in-extract-log";
    const SOURCE: &str = "source-text-never-in-extract-log";
    const RAW_ERROR: &str = "raw-anyhow-error-never-in-extract-log";

    fn log_path(db_dir: &Path) -> std::path::PathBuf {
        db_dir.join("extract.log")
    }

    fn assert_sanitized_json_line(line: &str, socket_path: &Path, expected_kind: &str) {
        let value: Value = serde_json::from_str(line).expect("error log is one JSON object");
        let object = value.as_object().expect("error log entry is an object");
        assert_eq!(object.len(), 3, "only the approved fields are present");
        assert!(
            object["timestamp"].is_u64(),
            "timestamp is a numeric instant"
        );
        assert_eq!(object["kind"], expected_kind);
        let socket_hash = object["socket_path_hash"]
            .as_str()
            .expect("socket path is represented by a hash");
        assert_eq!(socket_hash.len(), 64, "socket path hash is SHA-256 hex");
        assert!(socket_hash.bytes().all(|byte| byte.is_ascii_hexdigit()));

        let serialized = value.to_string();
        for forbidden in [
            socket_path.to_str().expect("UTF-8 socket path"),
            SECRET,
            COMMAND,
            SOURCE,
            RAW_ERROR,
        ] {
            assert!(!serialized.contains(forbidden), "log exposed {forbidden:?}");
        }
    }

    #[test]
    fn r2_error_log_writes_only_connect_failures_as_sanitized_json_lines() {
        let tempdir = tempfile::tempdir().expect("temporary database directory");
        let socket_path = tempdir
            .path()
            .join(format!("{SECRET}-{COMMAND}-{SOURCE}-{RAW_ERROR}.sock"));

        append_connect_failure(tempdir.path(), &socket_path, ErrorLogKind::Connect)
            .expect("connect failure is logged");

        let entries = fs::read_to_string(log_path(tempdir.path())).expect("current error log");
        let lines = entries.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 1);
        assert_sanitized_json_line(lines[0], &socket_path, "connect");
    }

    #[test]
    fn r2_error_log_rotates_once_at_256_kib_and_never_persists_sensitive_inputs() {
        let tempdir = tempfile::tempdir().expect("temporary database directory");
        let socket_path = tempdir
            .path()
            .join(format!("{SECRET}-{COMMAND}-{SOURCE}-{RAW_ERROR}.sock"));
        let current = log_path(tempdir.path());
        let rotated = tempdir.path().join("extract.log.1");
        fs::write(&rotated, b"obsolete rotation").expect("seed prior rotation");
        fs::write(&current, vec![b'x'; EXTRACT_LOG_CAP_BYTES]).expect("fill current log to cap");

        append_connect_failure(tempdir.path(), &socket_path, ErrorLogKind::Connect)
            .expect("append rotates before exceeding cap");

        let current_bytes = fs::read(&current).expect("new bounded current log");
        let rotated_bytes = fs::read(&rotated).expect("single retained rotation");
        assert!(current_bytes.len() <= EXTRACT_LOG_CAP_BYTES);
        assert_eq!(rotated_bytes.len(), EXTRACT_LOG_CAP_BYTES);
        assert!(
            tempdir
                .path()
                .join("extract.log.2")
                .try_exists()
                .is_ok_and(|exists| !exists)
        );

        let current_text =
            String::from_utf8(current_bytes).expect("current log is UTF-8 JSON lines");
        assert_sanitized_json_line(current_text.trim_end(), &socket_path, "connect");
        for text in [
            current_text,
            String::from_utf8(rotated_bytes).expect("rotated log is UTF-8"),
        ] {
            for forbidden in [
                SECRET,
                COMMAND,
                SOURCE,
                RAW_ERROR,
                socket_path.to_str().unwrap(),
            ] {
                assert!(!text.contains(forbidden), "log exposed {forbidden:?}");
            }
        }
    }
}
