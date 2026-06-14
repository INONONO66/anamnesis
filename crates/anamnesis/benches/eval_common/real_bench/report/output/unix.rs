use std::ffi::CString;
use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::Path;

use super::super::super::error::{BenchError, BenchResult};
use super::{PreparedReportOutput, cstring_path_part};

pub(super) fn open_output_parent(
    path: &Path,
    parent: &Path,
    file_name: CString,
) -> BenchResult<PreparedReportOutput> {
    let mut dir = open_cwd()?;
    for component in parent.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::Normal(name) => {
                let name = cstring_path_part(name)?;
                dir = open_or_create_dir_at(&dir, &name)?;
            }
            _ => {
                return Err(BenchError::InvalidInput(format!(
                    "unsupported output parent component: {}",
                    parent.display()
                )));
            }
        }
    }
    Ok(PreparedReportOutput {
        path: path.to_path_buf(),
        file_name,
        dir,
    })
}

pub(super) fn target_exists(output: &PreparedReportOutput) -> BenchResult<bool> {
    let result = unsafe {
        libc::faccessat(
            output.dir.as_raw_fd(),
            output.file_name.as_ptr(),
            libc::F_OK,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == 0 {
        return Ok(true);
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::ENOENT) {
        return Ok(false);
    }
    Err(BenchError::InvalidInput(err.to_string()))
}

pub(super) fn write_json_bytes(
    output: &PreparedReportOutput,
    bytes: &[u8],
    force: bool,
) -> BenchResult<()> {
    use std::io::Write;

    let (temp_name, mut temp_file) = create_temp_file(output)?;
    let result = temp_file
        .write_all(bytes)
        .map_err(|err| BenchError::InvalidInput(err.to_string()))
        .and_then(|()| finalize_temp(output, &temp_name, force));
    if result.is_err() {
        unlink_temp(output, &temp_name);
    }
    result
}

fn open_cwd() -> BenchResult<File> {
    let dot = CString::new(".").expect("literal has no NUL");
    let fd = unsafe {
        libc::open(
            dot.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    file_from_fd(fd, "open current directory")
}

fn open_or_create_dir_at(parent: &File, name: &CString) -> BenchResult<File> {
    match open_dir_at(parent, name) {
        Ok(file) => Ok(file),
        Err(err) if err.raw_os_error() == Some(libc::ENOENT) => {
            create_dir_at(parent, name)?;
            open_dir_at(parent, name)
                .map_err(|err| BenchError::InvalidInput(format!("open output directory: {err}")))
        }
        Err(err) if is_symlink_errno(&err) => Err(symlink_error(name)),
        Err(err) => Err(BenchError::InvalidInput(format!(
            "open output directory: {err}"
        ))),
    }
}

fn create_dir_at(parent: &File, name: &CString) -> BenchResult<()> {
    let created = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o755) };
    if created == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::EEXIST) {
        return Ok(());
    }
    Err(os_error(&format!(
        "create output directory {}",
        name.to_string_lossy()
    )))
}

fn open_dir_at(parent: &File, name: &CString) -> std::io::Result<File> {
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn create_temp_file(output: &PreparedReportOutput) -> BenchResult<(CString, File)> {
    for attempt in 0..128 {
        let temp_name = temp_name_for(&output.file_name, attempt)?;
        let fd = unsafe {
            libc::openat(
                output.dir.as_raw_fd(),
                temp_name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd >= 0 {
            return Ok((temp_name, unsafe { File::from_raw_fd(fd) }));
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::EEXIST) {
            return Err(BenchError::InvalidInput(err.to_string()));
        }
    }
    Err(BenchError::InvalidInput(format!(
        "could not create unique temp file for {}",
        output.path.display()
    )))
}

fn finalize_temp(
    output: &PreparedReportOutput,
    temp_name: &CString,
    force: bool,
) -> BenchResult<()> {
    let code = if force {
        rename_temp(output, temp_name)
    } else {
        link_temp(output, temp_name)
    };
    if code == 0 {
        if !force {
            unlink_temp(output, temp_name);
        }
        return Ok(());
    }
    finish_error(output)
}

fn rename_temp(output: &PreparedReportOutput, temp_name: &CString) -> libc::c_int {
    unsafe {
        libc::renameat(
            output.dir.as_raw_fd(),
            temp_name.as_ptr(),
            output.dir.as_raw_fd(),
            output.file_name.as_ptr(),
        )
    }
}

fn link_temp(output: &PreparedReportOutput, temp_name: &CString) -> libc::c_int {
    unsafe {
        libc::linkat(
            output.dir.as_raw_fd(),
            temp_name.as_ptr(),
            output.dir.as_raw_fd(),
            output.file_name.as_ptr(),
            0,
        )
    }
}

fn finish_error(output: &PreparedReportOutput) -> BenchResult<()> {
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::EEXIST) {
        return Err(BenchError::InvalidInput(format!(
            "output {} already exists; pass --force to overwrite",
            output.path.display()
        )));
    }
    Err(BenchError::InvalidInput(err.to_string()))
}

fn unlink_temp(output: &PreparedReportOutput, temp_name: &CString) {
    unsafe {
        libc::unlinkat(output.dir.as_raw_fd(), temp_name.as_ptr(), 0);
    }
}

fn temp_name_for(file_name: &CString, attempt: u32) -> BenchResult<CString> {
    let mut bytes = file_name.as_bytes().to_vec();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    bytes
        .extend_from_slice(format!(".{}.{}.{}.tmp", std::process::id(), nanos, attempt).as_bytes());
    CString::new(bytes).map_err(|err| BenchError::InvalidInput(err.to_string()))
}

fn file_from_fd(fd: libc::c_int, action: &str) -> BenchResult<File> {
    if fd < 0 {
        return Err(os_error(action));
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn os_error(action: &str) -> BenchError {
    BenchError::InvalidInput(format!("{action}: {}", std::io::Error::last_os_error()))
}

fn symlink_error(name: &CString) -> BenchError {
    BenchError::InvalidInput(format!(
        "output path contains symlink component: {}",
        name.to_string_lossy()
    ))
}

fn is_symlink_errno(err: &std::io::Error) -> bool {
    matches!(
        err.raw_os_error(),
        Some(code) if code == libc::ELOOP || code == libc::ENOTDIR
    )
}
