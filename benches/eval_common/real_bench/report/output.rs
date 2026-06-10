use std::ffi::CString;
#[cfg(unix)]
use std::fs::File;
use std::path::{Path, PathBuf};

use super::super::error::{BenchError, BenchResult};
use super::RealBenchReport;

#[cfg(not(unix))]
mod fallback;
#[cfg(unix)]
mod unix;

#[cfg(not(unix))]
use fallback::{open_output_parent, target_exists, write_json_bytes};
#[cfg(unix)]
use unix::{open_output_parent, target_exists, write_json_bytes};

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

pub struct PreparedReportOutput {
    pub(crate) path: PathBuf,
    pub(crate) file_name: CString,
    #[cfg(unix)]
    pub(crate) dir: File,
}

impl PreparedReportOutput {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

pub fn prepare_report_output(path: &Path, force: bool) -> BenchResult<PreparedReportOutput> {
    validate_report_path(path)?;
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    let file_name =
        cstring_path_part(path.file_name().ok_or_else(|| {
            BenchError::InvalidInput("output path has no file name".to_string())
        })?)?;
    let prepared = open_output_parent(path, parent, file_name)?;
    if !force && target_exists(&prepared)? {
        return Err(BenchError::InvalidInput(format!(
            "output {} already exists; pass --force to overwrite",
            path.display()
        )));
    }
    Ok(prepared)
}

pub fn write_report(report: &RealBenchReport, path: &Path, force: bool) -> BenchResult<()> {
    let output = prepare_report_output(path, force)?;
    write_prepared_report(report, &output, force)
}

pub fn write_prepared_report(
    report: &RealBenchReport,
    output: &PreparedReportOutput,
    force: bool,
) -> BenchResult<()> {
    let json = serde_json::to_string_pretty(report)
        .map_err(|err| BenchError::InvalidInput(err.to_string()))?;
    write_json_bytes(output, json.as_bytes(), force)
}

fn validate_report_path(path: &Path) -> BenchResult<()> {
    if path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(BenchError::InvalidInput(format!(
            "output path must be relative and must not contain ..: {}",
            path.display()
        )));
    }
    if !path.starts_with(".omo/evidence") && !path.starts_with("benches/eval/results") {
        return Err(BenchError::InvalidInput(format!(
            "output path must be under .omo/evidence or benches/eval/results: {}",
            path.display()
        )));
    }
    Ok(())
}

pub(super) fn cstring_path_part(part: &std::ffi::OsStr) -> BenchResult<CString> {
    #[cfg(unix)]
    let bytes = part.as_bytes();
    #[cfg(not(unix))]
    let bytes = part.to_string_lossy().as_bytes();
    CString::new(bytes).map_err(|_| {
        BenchError::InvalidInput(format!(
            "path component contains NUL byte: {}",
            part.to_string_lossy()
        ))
    })
}
