use skilled::{Action, AgentKind, AppEnvironment, Error, SkilledApp};

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
            supported: 3
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

fn dispatch(app: &mut SkilledApp, action: Action) {
    let update = app.update(action);
    app.perform_effects(update.effects())
        .expect("perform effects");
}
