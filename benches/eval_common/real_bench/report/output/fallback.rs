use std::ffi::CString;
use std::path::Path;

use super::super::super::error::{BenchError, BenchResult};
use super::PreparedReportOutput;

pub(super) fn open_output_parent(
    path: &Path,
    parent: &Path,
    file_name: CString,
) -> BenchResult<PreparedReportOutput> {
    std::fs::create_dir_all(parent).map_err(|err| BenchError::InvalidInput(err.to_string()))?;
    Ok(PreparedReportOutput {
        path: path.to_path_buf(),
        file_name,
    })
}

pub(super) fn target_exists(output: &PreparedReportOutput) -> BenchResult<bool> {
    Ok(output.path.exists())
}

pub(super) fn write_json_bytes(
    output: &PreparedReportOutput,
    bytes: &[u8],
    force: bool,
) -> BenchResult<()> {
    use std::io::Write;

    if !force && output.path.exists() {
        return Err(BenchError::InvalidInput(format!(
            "output {} already exists; pass --force to overwrite",
            output.path.display()
        )));
    }
    let temp_path = output.path.with_extension("tmp");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)
        .map_err(|err| BenchError::InvalidInput(err.to_string()))?;
    file.write_all(bytes)
        .map_err(|err| BenchError::InvalidInput(err.to_string()))?;
    std::fs::rename(&temp_path, &output.path)
        .map_err(|err| BenchError::InvalidInput(err.to_string()))
}
