use std::{fs, path::Path, process::Command};

use skilled::{Action, AgentKind, AppEnvironment, Error, SkilledApp, View};

#[test]
fn a_database_from_a_newer_skilled_version_is_rejected_before_use() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let data_dir = temporary.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data directory");
    let connection =
        rusqlite::Connection::open(data_dir.join("skilled.sqlite3")).expect("create database");
    connection
        .execute_batch("PRAGMA user_version = 99;")
        .expect("set future schema version");
    drop(connection);

    let result = SkilledApp::open(AppEnvironment::new(
        temporary.path().join("home"),
        data_dir,
        "",
    ));

    assert!(matches!(
        result,
        Err(Error::UnsupportedSchema {
            found: 99,
            supported: 5
        })
    ));
}

/// The next schema after the one this build writes is still refused, so a
/// database a newer Skilled upgraded is never written through by an older one.
#[test]
fn the_schema_one_past_this_build_is_refused_rather_than_written_through() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let data_dir = temporary.path().join("data");
    fs::create_dir_all(&data_dir).expect("create data directory");
    let connection =
        rusqlite::Connection::open(data_dir.join("skilled.sqlite3")).expect("create database");
    connection
        .execute_batch("PRAGMA user_version = 6;")
        .expect("set the next schema version");
    drop(connection);

    let result = SkilledApp::open(AppEnvironment::new(
        temporary.path().join("home"),
        data_dir,
        "",
    ));

    assert!(matches!(
        result,
        Err(Error::UnsupportedSchema {
            found: 6,
            supported: 5
        })
    ));
}

/// Spec 7: an ownership receipt is evidence that Skilled created a particular
/// link, and it has to outlive the source it came from — a forgotten source
/// must not take the record of what Skilled put on disk with it. The upgrade
/// therefore adds the table without disturbing what version four held.
#[test]
fn version_four_metadata_gains_receipt_storage_that_outlives_its_source() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let data_dir = temporary.path().join("data");
    fs::create_dir_all(&data_dir).expect("create data directory");
    let database = data_dir.join("skilled.sqlite3");
    let connection = rusqlite::Connection::open(&database).expect("create database");
    connection
        .execute_batch(
            "CREATE TABLE settings (
                key TEXT PRIMARY KEY NOT NULL,
                value TEXT NOT NULL
             );
             CREATE TABLE configured_agents (
                agent TEXT PRIMARY KEY NOT NULL,
                selected INTEGER NOT NULL CHECK (selected IN (0, 1))
             );
             CREATE TABLE source_repositories (
                id INTEGER PRIMARY KEY,
                label TEXT NOT NULL,
                canonical_path TEXT NOT NULL UNIQUE,
                remote_url TEXT,
                branch TEXT,
                head_revision TEXT NOT NULL,
                dirty INTEGER NOT NULL CHECK (dirty IN (0, 1)),
                last_scan_at INTEGER NOT NULL,
                dirty_known INTEGER NOT NULL DEFAULT 1 CHECK (dirty_known IN (0, 1))
             );
             CREATE TABLE catalog_roots (
                id INTEGER PRIMARY KEY,
                source_id INTEGER NOT NULL REFERENCES source_repositories(id) ON DELETE CASCADE,
                relative_path TEXT NOT NULL,
                classification TEXT NOT NULL CHECK (classification IN ('common', 'agent-specific')),
                claude_code INTEGER NOT NULL CHECK (claude_code IN (0, 1)),
                codex INTEGER NOT NULL CHECK (codex IN (0, 1)),
                opencode INTEGER NOT NULL CHECK (opencode IN (0, 1)),
                UNIQUE(source_id, relative_path)
             );
             INSERT INTO settings VALUES ('setup_complete', 'true');
             INSERT INTO configured_agents VALUES
                ('claude-code', 1), ('codex', 1), ('opencode', 1);
             INSERT INTO source_repositories
                (id, label, canonical_path, head_revision, dirty, last_scan_at, dirty_known)
             VALUES (1, 'library', '/fixture/library', 'abcdef0', 0, 0, 1);
             INSERT INTO catalog_roots
                (source_id, relative_path, classification, claude_code, codex, opencode)
             VALUES (1, 'skills', 'common', 1, 1, 1);
             PRAGMA user_version = 4;",
        )
        .expect("create version four schema");
    drop(connection);

    let environment = AppEnvironment::new(temporary.path().join("home"), &data_dir, "");
    let app = SkilledApp::open(environment).expect("migrate version four database");
    // The registered source survives the upgrade, and is still readable through
    // the application rather than only present in the table.
    assert_eq!(app.sources().len(), 1);
    assert_eq!(app.sources()[0].label(), "library");
    drop(app);

    let connection = rusqlite::Connection::open(&database).expect("inspect migrated database");
    connection
        .execute_batch("PRAGMA foreign_keys = ON;")
        .expect("enable constraint checks");
    assert_eq!(
        connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        5
    );
    connection
        .execute_batch(
            "INSERT INTO operation_receipts
                (created_at, operation, agent, skill_name, link_path, link_target,
                 source_id, catalog_relative_path, variant_relative_path)
             VALUES (0, 'install', 'claude-code', 'portable',
                     '/home/.claude/skills/portable', '/fixture/library/skills/portable',
                     1, 'skills', 'skills/portable');",
        )
        .expect("record an ownership receipt");
    assert!(
        connection
            .execute(
                "INSERT INTO operation_receipts
                    (created_at, operation, agent, skill_name, link_path, link_target)
                 VALUES (0, 'uninstall', 'codex', 'portable', '/link', '/target')",
                [],
            )
            .is_err(),
        "an operation this release does not perform should violate its check"
    );

    // Forgetting a source must not erase the evidence of what Skilled wrote:
    // the receipt is what a later repair or uninstall has to work from.
    connection
        .execute("DELETE FROM source_repositories WHERE id = 1", [])
        .expect("forget the source");
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM operation_receipts", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn version_one_metadata_migrates_and_persists_agent_selection() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let data_dir = temporary.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data directory");
    let connection =
        rusqlite::Connection::open(data_dir.join("skilled.sqlite3")).expect("create database");
    connection
        .execute_batch(
            "CREATE TABLE settings (
                key TEXT PRIMARY KEY NOT NULL,
                value TEXT NOT NULL
             );
             PRAGMA user_version = 1;",
        )
        .expect("create version one schema");
    drop(connection);

    let environment = AppEnvironment::new(temporary.path().join("home"), data_dir, "");
    let mut app = SkilledApp::open(environment.clone()).expect("migrate version one database");
    dispatch(&mut app, Action::Continue);
    dispatch(&mut app, Action::MoveSelection(1));
    dispatch(&mut app, Action::ToggleSelection);
    for _ in 0..6 {
        dispatch(&mut app, Action::Continue);
    }
    drop(app);

    let reopened = SkilledApp::open(environment).expect("reopen migrated database");
    assert!(!reopened.agent(AgentKind::Codex).selected());
}

#[test]
fn version_two_metadata_migrates_to_constrained_source_catalog_storage() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let data_dir = temporary.path().join("data");
    fs::create_dir_all(&data_dir).expect("create data directory");
    let database = data_dir.join("skilled.sqlite3");
    let connection = rusqlite::Connection::open(&database).expect("create database");
    connection
        .execute_batch(
            "CREATE TABLE settings (
                key TEXT PRIMARY KEY NOT NULL,
                value TEXT NOT NULL
             );
             CREATE TABLE configured_agents (
                agent TEXT PRIMARY KEY NOT NULL,
                selected INTEGER NOT NULL CHECK (selected IN (0, 1))
             );
             INSERT INTO settings VALUES ('setup_complete', 'true');
             INSERT INTO configured_agents VALUES
                ('claude-code', 1), ('codex', 0), ('opencode', 1);
             PRAGMA user_version = 2;",
        )
        .expect("create version two schema");
    drop(connection);
    let repository = temporary.path().join("source");
    fs::create_dir_all(repository.join("skills/portable")).expect("create source fixture");
    fs::write(
        repository.join("skills/portable/SKILL.md"),
        "---\nname: portable\ndescription: fixture\n---\n# Portable\n",
    )
    .expect("write skill fixture");
    initialize_repository(&repository);
    let environment = AppEnvironment::new(temporary.path().join("home"), &data_dir, "");
    let mut app = SkilledApp::open(environment.clone()).expect("migrate version two database");
    assert_eq!(app.view(), View::Inventory);
    assert!(!app.agent(AgentKind::Codex).selected());
    let preview = app.preview_source(&repository).expect("preview source");
    app.confirm_source(preview).expect("register source");
    drop(app);

    let connection = rusqlite::Connection::open(database).expect("inspect migrated database");
    connection
        .execute_batch("PRAGMA foreign_keys = ON;")
        .expect("enable constraint checks");
    assert_eq!(
        connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        5
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM source_repositories", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        1
    );
    assert!(
        connection
            .execute(
                "INSERT INTO catalog_roots
                    (source_id, relative_path, classification, claude_code, codex, opencode)
                 VALUES (1, 'skills', 'common', 1, 1, 1)",
                [],
            )
            .is_err(),
        "duplicate source catalog should violate its unique constraint"
    );
    assert!(
        connection
            .execute(
                "INSERT INTO catalog_roots
                    (source_id, relative_path, classification, claude_code, codex, opencode)
                 VALUES (999, 'missing', 'common', 1, 1, 1)",
                [],
            )
            .is_err(),
        "orphaned catalog should violate its foreign key"
    );
    assert!(
        connection
            .execute("UPDATE catalog_roots SET classification = 'invalid'", [],)
            .is_err(),
        "invalid classification should violate its check"
    );
}

fn dispatch(app: &mut SkilledApp, action: Action) {
    let update = app.update(action);
    app.perform_effects(update.effects())
        .expect("perform effects");
}

fn initialize_repository(repository: &Path) {
    for arguments in [
        &["init", "-b", "main"][..],
        &["config", "user.name", "Skilled Test"][..],
        &["config", "user.email", "skilled@example.test"][..],
        &["add", "."][..],
        &["commit", "-m", "fixture"][..],
    ] {
        let output = Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(arguments)
            .output()
            .expect("run Git fixture command");
        assert!(output.status.success());
    }
}
