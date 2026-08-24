use std::{fs, path::Path, process::Command};

use skilled::{Action, AgentKind, AppEnvironment, SkilledApp, View, operations::ReceiptOperation};

#[test]
fn a_database_from_a_newer_skilled_version_opens_degraded_without_writing() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let data_dir = temporary.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data directory");
    let connection =
        rusqlite::Connection::open(data_dir.join("skilled.sqlite3")).expect("create database");
    connection
        .execute_batch("PRAGMA user_version = 99;")
        .expect("set future schema version");
    drop(connection);
    let database = data_dir.join("skilled.sqlite3");
    let before = fs::read(&database).expect("read future database");

    let app = SkilledApp::open(AppEnvironment::new(
        temporary.path().join("home"),
        &data_dir,
        "",
    ))
    .expect("open degraded application");

    assert_eq!(app.view(), View::Inventory);
    assert!(
        app.metadata_failure()
            .expect("metadata failure")
            .cause()
            .contains("schema 99")
    );
    assert_eq!(fs::read(&database).expect("reread future database"), before);
    // A refused schema is refused before the journal mode is changed, so the
    // sidecars a write-ahead log would leave behind are never created either.
    assert!(!data_dir.join("skilled.sqlite3-wal").exists());
    assert!(!data_dir.join("skilled.sqlite3-shm").exists());
}

/// The next schema after the one this build writes is still refused, so a
/// database a newer Skilled upgraded is never written through by an older one.
#[test]
fn the_schema_one_past_this_build_degrades_rather_than_writing_through() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let data_dir = temporary.path().join("data");
    fs::create_dir_all(&data_dir).expect("create data directory");
    let connection =
        rusqlite::Connection::open(data_dir.join("skilled.sqlite3")).expect("create database");
    connection
        .execute_batch("PRAGMA user_version = 11;")
        .expect("set the next schema version");
    drop(connection);
    let database = data_dir.join("skilled.sqlite3");
    let before = fs::read(&database).expect("read future database");

    let app = SkilledApp::open(AppEnvironment::new(
        temporary.path().join("home"),
        data_dir,
        "",
    ))
    .expect("open degraded application");

    assert_eq!(app.view(), View::Inventory);
    assert!(
        app.metadata_failure()
            .expect("metadata failure")
            .cause()
            .contains("schema 11")
    );
    assert_eq!(fs::read(database).expect("reread future database"), before);
}

/// The update branch reached schema nine before main introduced the monotonic
/// source-ID sequence in its own schema six. The join migration must repair a
/// database from that branch instead of assuming every version-nine database
/// passed through main's version six.
#[test]
fn version_nine_update_metadata_gains_the_source_id_sequence() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let data_dir = temporary.path().join("data");
    let environment = AppEnvironment::new(temporary.path().join("home"), &data_dir, "");
    drop(SkilledApp::open(environment.clone()).expect("create current database"));
    let database = data_dir.join("skilled.sqlite3");
    let connection = rusqlite::Connection::open(&database).expect("open current database");
    connection
        .execute_batch(
            "DROP TABLE source_id_sequence;
             PRAGMA user_version = 9;",
        )
        .expect("stage update-branch schema nine");
    drop(connection);

    drop(SkilledApp::open(environment).expect("migrate update-branch database"));

    let connection = rusqlite::Connection::open(database).expect("inspect migrated database");
    assert_eq!(
        connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .expect("schema version"),
        10
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT next_id FROM source_id_sequence WHERE singleton = 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("source ID sequence"),
        1
    );
}

/// Version six adds repair receipts without changing the chronological contract
/// callers use to choose the newest ownership evidence. The migration names
/// `id` in its copy so equal timestamps keep their original tiebreaker.
#[test]
fn version_five_receipts_gain_operations_and_keep_their_id_order() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let data_dir = temporary.path().join("data");
    fs::create_dir_all(&data_dir).expect("create data directory");
    let database = data_dir.join("skilled.sqlite3");
    let connection = rusqlite::Connection::open(&database).expect("create database");
    connection
        .execute_batch(
            "CREATE TABLE settings (key TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL);
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
             CREATE TABLE operation_receipts (
                id INTEGER PRIMARY KEY,
                created_at INTEGER NOT NULL,
                operation TEXT NOT NULL CHECK (operation IN ('install')),
                agent TEXT NOT NULL,
                skill_name TEXT NOT NULL,
                link_path TEXT NOT NULL,
                link_target TEXT NOT NULL,
                source_id INTEGER,
                catalog_relative_path TEXT,
                variant_relative_path TEXT,
                UNIQUE (agent, link_path, link_target, created_at)
             );
             INSERT INTO settings VALUES ('setup_complete', 'true');
             INSERT INTO configured_agents VALUES
                ('claude-code', 1), ('codex', 1), ('opencode', 1);
             INSERT INTO operation_receipts
                (id, created_at, operation, agent, skill_name, link_path, link_target)
             VALUES
                (9, 100, 'install', 'codex', 'portable', '/link/old', '/target/old'),
                (4, 100, 'install', 'codex', 'portable', '/link/new', '/target/new');
             PRAGMA user_version = 5;",
        )
        .expect("create version five schema");
    drop(connection);

    let environment = AppEnvironment::new(temporary.path().join("home"), &data_dir, "");
    let app = SkilledApp::open(environment).expect("migrate version five database");
    let receipts = app.receipts().expect("read migrated receipts");

    assert_eq!(
        receipts
            .iter()
            .map(|receipt| (receipt.link_path(), receipt.operation()))
            .collect::<Vec<_>>(),
        [
            (Path::new("/link/new"), ReceiptOperation::Install),
            (Path::new("/link/old"), ReceiptOperation::Install),
        ]
    );
    drop(app);

    // This deliberately uses raw SQL: `record_receipt` owns its clock, so a
    // deterministic same-second collision cannot be made through that API.
    let connection = rusqlite::Connection::open(&database).expect("inspect migrated database");
    connection
        .execute_batch(
            "INSERT INTO operation_receipts
                (created_at, operation, agent, skill_name, link_path, link_target)
             VALUES
                (200, 'install', 'claude-code', 'portable', '/same', '/target'),
                (200, 'repair', 'claude-code', 'portable', '/same', '/target');",
        )
        .expect("store distinct operations in one second");
    assert_eq!(
        connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        10
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM operation_receipts WHERE link_path = '/same'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        2
    );
    drop(connection);

    let app = SkilledApp::open(AppEnvironment::new(
        temporary.path().join("home"),
        &data_dir,
        "",
    ))
    .expect("reopen migrated database");
    assert_eq!(
        app.receipts()
            .unwrap()
            .into_iter()
            .filter(|receipt| receipt.link_path() == Path::new("/same"))
            .map(|receipt| receipt.operation())
            .collect::<Vec<_>>(),
        [ReceiptOperation::Install, ReceiptOperation::Repair],
        "same-second install and repair receipts read back in id order"
    );
}

#[test]
fn concurrent_openers_serialize_schema_discovery_and_migration() {
    use std::sync::{Arc, Barrier};

    let temporary = tempfile::tempdir().expect("temporary application directory");
    let data_dir = temporary.path().join("data");
    fs::create_dir_all(&data_dir).expect("create data directory");
    let database = data_dir.join("skilled.sqlite3");
    let connection = rusqlite::Connection::open(&database).expect("create database");
    connection
        .execute_batch(
            "CREATE TABLE settings (key TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL);
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
             CREATE TABLE operation_receipts (
                id INTEGER PRIMARY KEY,
                created_at INTEGER NOT NULL,
                operation TEXT NOT NULL CHECK (operation IN ('install')),
                agent TEXT NOT NULL,
                skill_name TEXT NOT NULL,
                link_path TEXT NOT NULL,
                link_target TEXT NOT NULL,
                source_id INTEGER,
                catalog_relative_path TEXT,
                variant_relative_path TEXT,
                UNIQUE (agent, link_path, link_target, created_at)
             );
             PRAGMA user_version = 5;",
        )
        .expect("create version five schema");
    drop(connection);
    let barrier = Arc::new(Barrier::new(2));
    let opens = (0..2)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            let data_dir = data_dir.clone();
            let home = temporary.path().join("home");
            std::thread::spawn(move || {
                barrier.wait();
                SkilledApp::open(AppEnvironment::new(home, data_dir, ""))
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            })
        })
        .collect::<Vec<_>>();

    for open in opens {
        open.join()
            .expect("opener thread")
            .expect("migrate database");
    }
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
        10
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
        10
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
