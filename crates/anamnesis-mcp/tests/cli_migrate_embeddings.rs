use std::path::Path;
use std::process::Command;

fn migration_command(db: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_anamnesis"));
    command.arg("migrate-embeddings").env("ANAMNESIS_DB", db);
    command
}

#[test]
fn migrate_embeddings_parses_optional_namespace() {
    let output = Command::new(env!("CARGO_BIN_EXE_anamnesis"))
        .args(["migrate-embeddings", "--namespace", "Team Memory", "--help"])
        .output()
        .expect("run migrate-embeddings help");

    assert!(
        output.status.success(),
        "migrate-embeddings with an optional namespace must parse: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn manual_migration_refuses_daemon_owned_lock_before_backup() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let db = dir.path().join("locked.db");
    let original = b"database bytes stay untouched";
    std::fs::write(&db, original).expect("create database path fixture");
    let lock_path = dir.path().join("locked.db.lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(lock_path)
        .expect("open fixture lock");
    fs4::FileExt::try_lock(&lock).expect("hold fixture lock");

    let output = migration_command(&db).output().expect("run migration");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("stop the anamnesis daemon"), "{stderr}");
    assert!(stderr.contains("retry"), "{stderr}");
    assert_eq!(std::fs::read(&db).expect("read database fixture"), original);
    assert!(
        std::fs::read_dir(dir.path())
            .expect("list fixture directory")
            .all(|entry| !entry
                .expect("directory entry")
                .file_name()
                .to_string_lossy()
                .contains(".bak-"))
    );
}

#[test]
fn progress_is_reported_only_for_committed_batches() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let db = dir.path().join("locked-progress.db");
    std::fs::write(&db, b"locked before any database commit").expect("create fixture");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(dir.path().join("locked-progress.db.lock"))
        .expect("open fixture lock");
    fs4::FileExt::try_lock(&lock).expect("hold fixture lock");

    let output = migration_command(&db).output().expect("run migration");

    assert!(!output.status.success());
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("embedding migration batch committed")
    );
}
