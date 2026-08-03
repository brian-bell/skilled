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
            supported: 4
        })
    ));
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
        4
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
