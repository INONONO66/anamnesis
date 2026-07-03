//! Bug #6: `migrate_schema` used to stamp `schema_version` only ONCE, after the
//! whole hop chain for a given match arm finished — every hop's DDL/data change
//! ran as a bare (or self-contained but unstamped) statement in between. If the
//! process crashed mid-chain, an already-applied hop's change landed durably
//! (SQLite commits each statement outside an explicit transaction immediately)
//! but the recorded `schema_version` stayed at the pre-crash number. Reopening
//! then replayed the already-completed hop and collided with its own DDL (e.g.
//! `ALTER TABLE ... ADD COLUMN` on a column that already exists), permanently
//! bricking the database.
//!
//! This fixture simulates exactly that intermediate state without needing a
//! real crash: a database whose schema is already fully current (so the
//! v4->v5 payload — the `nodes.evidence_prior` column — is already present) but
//! whose recorded `schema_version` was left stale at 4, as if the process died
//! right after the v4->v5 `ALTER TABLE` committed and before the (pre-fix)
//! end-of-chain version stamp ran.

use anamnesis::storage::SqliteStorage;
use rusqlite::Connection;

fn schema_version(conn: &Connection) -> u32 {
    conn.query_row("SELECT version FROM schema_version LIMIT 1", [], |row| {
        row.get(0)
    })
    .expect("schema_version")
}

#[test]
fn replay_after_a_stale_version_stamp_does_not_brick_an_already_migrated_db() {
    let tmp = std::env::temp_dir().join(format!(
        "anamnesis_test_migration_replay_{}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&tmp);

    // 1. A fully current database — its schema already carries every hop's
    //    change, including the v4->v5 `evidence_prior` column.
    SqliteStorage::open(&tmp).expect("open fresh db");

    // 2. Simulate the crash: schema is current, but the recorded version is
    //    stale at 4.
    {
        let conn = Connection::open(&tmp).expect("raw conn opens");
        conn.execute_batch("UPDATE schema_version SET version = 4;")
            .expect("simulate stale post-crash version stamp");
        assert_eq!(
            schema_version(&conn),
            4,
            "fixture must be at the stale version"
        );
    }

    // 3. Reopen: replaying the v4->v5 hop against an already-migrated schema
    //    must NOT fail with a duplicate-column error, and the chain must reach
    //    the current schema version.
    let reopened = SqliteStorage::open(&tmp);
    assert!(
        reopened.is_ok(),
        "reopening a stale-version-but-current-schema DB must not brick: {:?}",
        reopened.err()
    );

    let conn = Connection::open(&tmp).expect("raw conn opens");
    assert_eq!(
        schema_version(&conn),
        10,
        "replay must reach the current schema version"
    );

    let _ = std::fs::remove_file(&tmp);
}

/// Same defect, one hop earlier: the v1->v2 hop (peers/peer_aliases tables +
/// `nodes.peer_id`/`source_kind` columns) is also bare today. A stale version
/// of 1 against an already-current schema must replay cleanly too.
#[test]
fn replay_from_stale_v1_against_current_schema_does_not_brick() {
    let tmp = std::env::temp_dir().join(format!(
        "anamnesis_test_migration_replay_v1_{}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&tmp);

    SqliteStorage::open(&tmp).expect("open fresh db");
    {
        let conn = Connection::open(&tmp).expect("raw conn opens");
        conn.execute_batch("UPDATE schema_version SET version = 1;")
            .expect("simulate stale post-crash version stamp");
    }

    let reopened = SqliteStorage::open(&tmp);
    assert!(
        reopened.is_ok(),
        "reopening a stale-v1-but-current-schema DB must not brick: {:?}",
        reopened.err()
    );
    let conn = Connection::open(&tmp).expect("raw conn opens");
    assert_eq!(
        schema_version(&conn),
        10,
        "replay must reach the current schema version"
    );

    let _ = std::fs::remove_file(&tmp);
}
