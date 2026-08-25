use std::fs;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use skilled::{
    Action, AgentKind, AppEnvironment, RegistryAvailability, SkilledApp, View,
    input::action_for_app_key,
    inventory::{InstallationHealth, Provenance},
};

fn complete_setup(home: &std::path::Path, data: &std::path::Path) {
    let mut app =
        SkilledApp::open(AppEnvironment::new(home, data, "")).expect("open application for setup");
    for _ in 0..7 {
        let update = app.update(Action::Continue);
        app.perform_effects(update.effects())
            .expect("complete setup");
    }
}

#[test]
fn a_missing_database_in_an_existing_data_directory_opens_read_only_inventory() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let home = temporary.path().join("home");
    let data = temporary.path().join("data");
    let skill = home.join(".claude/skills/portable");
    fs::create_dir_all(&skill).expect("create temporary skill root");
    fs::write(
        skill.join("SKILL.md"),
        "---\nname: portable\ndescription: portable fixture\n---\n",
    )
    .expect("write temporary skill");
    fs::create_dir_all(&data).expect("create established data directory");

    let app = SkilledApp::open(AppEnvironment::new(&home, &data, ""))
        .expect("open with unavailable metadata");

    assert_eq!(app.view(), View::Inventory);
    assert_eq!(
        app.registry_availability(),
        RegistryAvailability::Unavailable
    );
    let failure = app.metadata_failure().expect("metadata failure");
    assert_eq!(failure.database_path(), data.join("skilled.sqlite3"));
    assert!(!failure.cause().is_empty());
    assert!(app.inventory().row("portable").is_some());
    assert!(!data.join("skilled.sqlite3").exists());
}

#[test]
fn degraded_navigation_remains_available_while_mutation_keys_are_refused() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let data = temporary.path().join("data");
    fs::create_dir_all(&data).expect("create established data directory");
    let mut app = SkilledApp::open(AppEnvironment::new(
        temporary.path().join("home"),
        &data,
        "",
    ))
    .expect("open degraded application");

    let route = |character| KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE);
    assert_eq!(
        action_for_app_key(&app, route('2')),
        Some(Action::OpenSources)
    );
    app.update(Action::OpenSources);
    assert_eq!(app.view(), View::Sources);
    assert_eq!(action_for_app_key(&app, route('a')), None);
    assert!(app.update(Action::BeginAddSource).effects().is_empty());
    assert!(!app.source_path_input_active());

    app.update(Action::OpenInventory);
    assert_eq!(
        action_for_app_key(&app, route('4')),
        Some(Action::OpenDoctor)
    );
    app.update(Action::OpenDoctor);
    assert_eq!(app.view(), View::Doctor);
    app.update(Action::OpenInventory);
    app.update(Action::OpenSettings);
    assert_eq!(app.view(), View::Settings);
    assert_eq!(
        action_for_app_key(&app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        None
    );
    assert!(app.update(Action::RerunSetup).effects().is_empty());
    assert_eq!(app.view(), View::Settings);
    assert!(!data.join("skilled.sqlite3").exists());
}

#[cfg(unix)]
#[test]
fn degraded_inventory_withholds_uninstall_for_an_observed_link() {
    use std::os::unix::fs::symlink;

    let temporary = tempfile::tempdir().expect("temporary application directory");
    let home = temporary.path().join("home");
    let data = temporary.path().join("data");
    let target = temporary.path().join("portable");
    fs::create_dir_all(&target).expect("create link target");
    fs::write(
        target.join("SKILL.md"),
        "---\nname: portable\ndescription: portable fixture\n---\n",
    )
    .expect("write link target");
    let root = home.join(".claude/skills");
    fs::create_dir_all(&root).expect("create agent root");
    symlink(&target, root.join("portable")).expect("create observed link");
    fs::create_dir_all(&data).expect("create established data directory");
    let app =
        SkilledApp::open(AppEnvironment::new(&home, &data, "")).expect("open degraded application");

    let uninstall = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
    assert!(!app.can_uninstall_selection());
    assert_eq!(action_for_app_key(&app, uninstall), None);
}

#[test]
fn corrupt_metadata_is_unchanged_and_installed_content_has_unverified_provenance() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let home = temporary.path().join("home");
    let data = temporary.path().join("data");
    let database = data.join("skilled.sqlite3");
    let skill = home.join(".claude/skills/portable");
    fs::create_dir_all(&skill).expect("create temporary skill root");
    fs::write(
        skill.join("SKILL.md"),
        "---\nname: portable\ndescription: portable fixture\n---\n",
    )
    .expect("write temporary skill");
    fs::create_dir_all(&data).expect("create data directory");
    let corrupt = b"not a sqlite database\n";
    fs::write(&database, corrupt).expect("write corrupt database");

    let app = SkilledApp::open(AppEnvironment::new(&home, &data, ""))
        .expect("open with corrupt metadata");

    assert_eq!(fs::read(&database).expect("reread database"), corrupt);
    assert_eq!(app.view(), View::Inventory);
    assert!(!app.inventory().registry_is_complete());
    assert_eq!(app.inventory().stated_finding_count(), None);
    assert_eq!(app.inventory().stated_skill_count(), Some(1));
    let observation = app
        .inventory()
        .row("portable")
        .and_then(|row| row.observation(skilled::AgentKind::ClaudeCode))
        .expect("portable observation");
    assert_eq!(observation.health(), InstallationHealth::Unverified);
    assert_eq!(observation.provenance(), &Provenance::Unverified);
    let finding = observation
        .findings()
        .iter()
        .find(|finding| finding.code() == "install.provenance_unverified")
        .expect("unverified provenance finding");
    assert!(finding.evidence().contains("own metadata"));
    assert!(
        !finding
            .evidence()
            .contains("registered source could not be read")
    );
}

/// SQLite opens a write-protected database read-only rather than failing, and
/// a current schema leaves migration with nothing to write, so every startup
/// read succeeds on a store that can never be written to. The session has to
/// find that out at open rather than after an install has already linked
/// something it cannot receipt — and, having found it out, must keep every
/// value the store was still perfectly able to give it.
#[cfg(unix)]
#[test]
fn a_write_protected_database_is_read_whole_and_written_to_never() {
    use std::os::unix::fs::PermissionsExt;

    let temporary = tempfile::tempdir().expect("temporary application directory");
    let home = temporary.path().join("home");
    let data = temporary.path().join("data");
    let skill = home.join(".claude/skills/portable");
    fs::create_dir_all(&skill).expect("create temporary skill root");
    fs::write(
        skill.join("SKILL.md"),
        "---\nname: portable\ndescription: portable fixture\n---\n",
    )
    .expect("write temporary skill");
    let hidden = home.join(".codex/skills/deselected");
    fs::create_dir_all(&hidden).expect("create deselected skill root");
    fs::write(
        hidden.join("SKILL.md"),
        "---\nname: deselected\ndescription: must not be scanned\n---\n",
    )
    .expect("write deselected skill");
    complete_setup(&home, &data);
    let database = data.join("skilled.sqlite3");
    let connection = rusqlite::Connection::open(&database).expect("open metadata database");
    connection
        .execute_batch("UPDATE configured_agents SET selected = (agent = 'claude-code');")
        .expect("narrow the persisted agent selection");
    drop(connection);
    let original = fs::read(&database).expect("read metadata database");
    fs::set_permissions(&database, fs::Permissions::from_mode(0o444))
        .expect("write-protect metadata database");

    let app =
        SkilledApp::open(AppEnvironment::new(&home, &data, "")).expect("open degraded application");

    assert_eq!(app.view(), View::Inventory);
    assert!(!app.can_add_source());
    assert!(!app.can_rerun_setup());
    let failure = app.metadata_failure().expect("metadata failure");
    assert_eq!(failure.database_path(), database);
    assert!(failure.cause().contains("read-only"), "{}", failure.cause());
    // Refusing a write is not a reason to discard what was read. The stored
    // selection still decides the scan scope, and the registry still counts.
    assert!(app.agent(AgentKind::ClaudeCode).selected());
    assert!(!app.agent(AgentKind::Codex).selected());
    assert!(!app.agent(AgentKind::OpenCode).selected());
    assert_eq!(app.registry_availability(), RegistryAvailability::Readable);
    assert!(app.inventory().row("portable").is_some());
    assert!(app.inventory().row("deselected").is_none());
    assert_eq!(fs::read(&database).expect("reread database"), original);
}

#[test]
fn malformed_setup_completion_forces_degraded_mode() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let home = temporary.path().join("home");
    let data = temporary.path().join("data");
    complete_setup(&home, &data);
    let hidden = home.join(".codex/skills/deselected");
    fs::create_dir_all(&hidden).expect("create deselected skill root");
    fs::write(
        hidden.join("SKILL.md"),
        "---\nname: deselected\ndescription: must not be scanned\n---\n",
    )
    .expect("write deselected skill");
    let database = data.join("skilled.sqlite3");
    let connection = rusqlite::Connection::open(&database).expect("open metadata database");
    connection
        .execute_batch(
            "UPDATE configured_agents SET selected = (agent = 'claude-code');
             UPDATE settings SET value = 'sometimes' WHERE key = 'setup_complete';",
        )
        .expect("corrupt setup completion");
    drop(connection);

    let app =
        SkilledApp::open(AppEnvironment::new(&home, &data, "")).expect("open degraded application");

    assert_eq!(app.view(), View::Inventory);
    assert!(!app.can_add_source());
    assert!(app.agent(AgentKind::ClaudeCode).selected());
    assert!(!app.agent(AgentKind::Codex).selected());
    assert!(!app.agent(AgentKind::OpenCode).selected());
    assert!(app.inventory().row("deselected").is_none());
    assert!(
        app.metadata_failure()
            .expect("metadata failure")
            .cause()
            .contains("setup_complete")
    );
}

#[test]
fn degraded_sources_withhold_forget_for_a_recovered_registration() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let home = temporary.path().join("home");
    let data = temporary.path().join("data");
    complete_setup(&home, &data);
    let database = data.join("skilled.sqlite3");
    let connection = rusqlite::Connection::open(&database).expect("open metadata database");
    connection
        .execute_batch(
            "INSERT INTO source_repositories
                (id, label, canonical_path, head_revision, dirty, last_scan_at, dirty_known)
             VALUES (1, 'library', '/fixture/library', 'abcdef0', 0, 0, 1);
             UPDATE settings SET value = 'sometimes' WHERE key = 'setup_complete';",
        )
        .expect("create a readable source beside malformed setup metadata");
    drop(connection);
    let mut app =
        SkilledApp::open(AppEnvironment::new(&home, &data, "")).expect("open degraded application");
    app.update(Action::OpenSources);

    let forget = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
    assert!(!app.can_forget_source());
    assert_eq!(action_for_app_key(&app, forget), None);
}

#[test]
fn invalid_integer_agent_selection_forces_degraded_mode() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let home = temporary.path().join("home");
    let data = temporary.path().join("data");
    complete_setup(&home, &data);
    let database = data.join("skilled.sqlite3");
    let connection = rusqlite::Connection::open(&database).expect("open metadata database");
    connection
        .execute_batch(
            "PRAGMA ignore_check_constraints = ON;
             UPDATE configured_agents SET selected = 2 WHERE agent = 'codex';",
        )
        .expect("corrupt configured agent boolean");
    drop(connection);

    let app =
        SkilledApp::open(AppEnvironment::new(&home, &data, "")).expect("open degraded application");

    assert_eq!(app.view(), View::Inventory);
    assert!(!app.can_add_source());
    assert!(
        app.metadata_failure()
            .expect("metadata failure")
            .cause()
            .contains("configured_agents.selected")
    );
}

#[test]
fn an_undecodable_receipt_forces_degraded_mode_without_discarding_other_metadata() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let home = temporary.path().join("home");
    let data = temporary.path().join("data");
    complete_setup(&home, &data);
    let database = data.join("skilled.sqlite3");
    let connection = rusqlite::Connection::open(&database).expect("open metadata database");
    connection
        .execute(
            "INSERT INTO operation_receipts
                (created_at, operation, agent, skill_name, link_path, link_target)
             VALUES (1, 'install', 'future-agent', 'portable', '/link', '/target')",
            [],
        )
        .expect("insert receipt from an unknown agent");
    drop(connection);

    let app =
        SkilledApp::open(AppEnvironment::new(&home, &data, "")).expect("open degraded application");

    assert_eq!(app.view(), View::Inventory);
    assert!(!app.can_add_source());
    assert!(!app.can_rerun_setup());
    assert!(app.agent(AgentKind::ClaudeCode).selected());
    assert_eq!(app.registry_availability(), RegistryAvailability::Readable);
    assert!(
        app.metadata_failure()
            .expect("metadata failure")
            .cause()
            .contains("future-agent")
    );
    assert!(app.cached_receipts().is_none());
}

#[test]
fn incomplete_completed_agent_selection_forces_degraded_mode() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let home = temporary.path().join("home");
    let data = temporary.path().join("data");
    complete_setup(&home, &data);
    let database = data.join("skilled.sqlite3");
    let connection = rusqlite::Connection::open(&database).expect("open metadata database");
    connection
        .execute("DELETE FROM configured_agents WHERE agent = 'codex'", [])
        .expect("remove one configured agent");
    drop(connection);

    let app =
        SkilledApp::open(AppEnvironment::new(&home, &data, "")).expect("open degraded application");

    assert_eq!(app.view(), View::Inventory);
    assert!(!app.can_add_source());
    assert!(
        app.metadata_failure()
            .expect("metadata failure")
            .cause()
            .contains("configured_agents")
    );
}

#[test]
fn corrupt_registry_retains_valid_agent_selection_and_scan_scope() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let home = temporary.path().join("home");
    let data = temporary.path().join("data");
    complete_setup(&home, &data);
    let hidden = home.join(".codex/skills/deselected");
    fs::create_dir_all(&hidden).expect("create deselected skill root");
    fs::write(
        hidden.join("SKILL.md"),
        "---\nname: deselected\ndescription: must not be scanned\n---\n",
    )
    .expect("write deselected skill");

    let database = data.join("skilled.sqlite3");
    let connection = rusqlite::Connection::open(&database).expect("open metadata database");
    connection
        .execute_batch(
            "UPDATE configured_agents SET selected = (agent = 'claude-code');
             INSERT INTO source_repositories
                (id, label, canonical_path, head_revision, dirty, last_scan_at, dirty_known)
             VALUES (1, 'corrupt', '/fixture/corrupt', 'abcdef0', 0, 0, 1);
             INSERT INTO catalog_roots
                (source_id, relative_path, classification, claude_code, codex, opencode)
             VALUES (1, 'skills', 'common', 1, 1, 1);
             PRAGMA ignore_check_constraints = ON;
             UPDATE catalog_roots SET classification = 'invalid';",
        )
        .expect("corrupt source registry");
    drop(connection);

    let app =
        SkilledApp::open(AppEnvironment::new(&home, &data, "")).expect("open degraded application");

    assert_eq!(app.view(), View::Inventory);
    assert!(app.metadata_failure().is_some());
    assert_eq!(
        app.registry_availability(),
        RegistryAvailability::Unavailable
    );
    assert!(app.agent(AgentKind::ClaudeCode).selected());
    assert!(!app.agent(AgentKind::Codex).selected());
    assert!(!app.agent(AgentKind::OpenCode).selected());
    assert!(app.inventory().row("deselected").is_none());
}

#[test]
fn invalid_integer_registry_booleans_force_degraded_mode() {
    for corrupt in [
        "UPDATE source_repositories SET dirty = 2 WHERE id = 1;",
        "UPDATE catalog_roots SET codex = 2 WHERE source_id = 1;",
    ] {
        let temporary = tempfile::tempdir().expect("temporary application directory");
        let home = temporary.path().join("home");
        let data = temporary.path().join("data");
        complete_setup(&home, &data);
        let database = data.join("skilled.sqlite3");
        let connection = rusqlite::Connection::open(&database).expect("open metadata database");
        connection
            .execute_batch(
                "INSERT INTO source_repositories
                    (id, label, canonical_path, head_revision, dirty, last_scan_at, dirty_known)
                 VALUES (1, 'corrupt', '/fixture/corrupt', 'abcdef0', 0, 0, 1);
                 INSERT INTO catalog_roots
                    (source_id, relative_path, classification, claude_code, codex, opencode)
                 VALUES (1, 'skills', 'common', 1, 1, 1);
                 PRAGMA ignore_check_constraints = ON;",
            )
            .expect("create source registry fixture");
        connection
            .execute_batch(corrupt)
            .expect("corrupt registry boolean");
        drop(connection);

        let app = SkilledApp::open(AppEnvironment::new(&home, &data, ""))
            .expect("open degraded application");

        assert_eq!(app.view(), View::Inventory, "{corrupt}");
        assert!(!app.can_add_source(), "{corrupt}");
        assert!(
            app.metadata_failure()
                .expect("metadata failure")
                .cause()
                .contains("rather than 0 or 1"),
            "{corrupt}"
        );
    }
}

/// A writable database inside a directory that denies file creation opens
/// read-write and reads perfectly: SQLite needs the directory only when it
/// creates the rollback journal or the write-ahead log, which happens at the
/// first transaction and not at the open. Unprobed, the failure lands on
/// whichever write runs first: the `journal_mode` pragma at open, which raises
/// a bare SQLite write error and throws away every value the store was
/// perfectly able to give up, or — where that pragma has nothing to change —
/// the first receipt, recorded after `apply_install` has already created the
/// link. This asserts the third answer: degraded, named as read-only, with
/// everything readable kept and nothing written anywhere.
#[cfg(unix)]
#[test]
fn a_data_directory_that_denies_journal_creation_degrades_read_only() {
    use std::os::unix::fs::PermissionsExt;

    let temporary = tempfile::tempdir().expect("temporary application directory");
    let home = temporary.path().join("home");
    let data = temporary.path().join("data");
    let skill = home.join(".claude/skills/portable");
    fs::create_dir_all(&skill).expect("create temporary skill root");
    fs::write(
        skill.join("SKILL.md"),
        "---\nname: portable\ndescription: portable fixture\n---\n",
    )
    .expect("write temporary skill");
    complete_setup(&home, &data);
    let database = data.join("skilled.sqlite3");
    // A rollback-journal store is the case the probe exists for: it reads
    // perfectly out of a sealed directory and only its first *write* needs the
    // journal. A write-ahead store cannot be read there at all — SQLite needs
    // its shared-memory sidecar to read one — and that failure already reaches
    // the session as a metadata failure at open.
    let connection = rusqlite::Connection::open(&database).expect("open metadata database");
    let mode: String = connection
        .query_row("PRAGMA journal_mode = DELETE", [], |row| row.get(0))
        .expect("set the rollback journal mode");
    assert_eq!(mode, "delete");
    drop(connection);
    let original = fs::read(&database).expect("read metadata database");
    let before = data_directory_entries(&data);
    fs::set_permissions(&data, fs::Permissions::from_mode(0o555))
        .expect("deny creation in the data directory");

    let opened = SkilledApp::open(AppEnvironment::new(&home, &data, ""));

    let after = data_directory_entries(&data);
    fs::set_permissions(&data, fs::Permissions::from_mode(0o755))
        .expect("restore the data directory");
    let app = opened.expect("open degraded application");
    assert_eq!(app.view(), View::Inventory);
    assert!(!app.can_add_source());
    assert!(!app.can_rerun_setup());
    let failure = app.metadata_failure().expect("metadata failure");
    assert_eq!(failure.database_path(), database);
    assert!(failure.cause().contains("read-only"), "{}", failure.cause());
    // Everything the store can still be read for is kept, exactly as it is for
    // a write-protected database file.
    assert!(app.agent(AgentKind::ClaudeCode).selected());
    assert_eq!(app.registry_availability(), RegistryAvailability::Readable);
    assert!(app.inventory().row("portable").is_some());
    assert_eq!(fs::read(&database).expect("reread database"), original);
    assert_eq!(after, before, "the sealed data directory changed");
}

/// The probe proves a capability by exercising it, so a data directory that
/// does allow creation must be left exactly as it was found — no probe file,
/// and no change to the database the session goes on to write normally.
#[test]
fn a_writable_data_directory_keeps_write_access_and_no_probe_residue() {
    let temporary = tempfile::tempdir().expect("temporary application directory");
    let home = temporary.path().join("home");
    let data = temporary.path().join("data");
    complete_setup(&home, &data);

    let app =
        SkilledApp::open(AppEnvironment::new(&home, &data, "")).expect("open ready application");

    assert!(app.metadata_failure().is_none());
    assert!(app.can_add_source());
    assert!(
        data_directory_entries(&data)
            .iter()
            .all(|name| !name.contains("write-probe")),
        "the probe left its file behind"
    );
}

fn data_directory_entries(data: &std::path::Path) -> Vec<String> {
    let mut entries: Vec<String> = fs::read_dir(data)
        .expect("list data directory")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    entries.sort();
    entries
}

#[cfg(unix)]
#[test]
fn metadata_leaf_symlinks_are_never_followed_or_modified() {
    use std::os::unix::fs::symlink;

    let temporary = tempfile::tempdir().expect("temporary application directory");
    let home = temporary.path().join("home");
    let target = temporary.path().join("target.sqlite3");
    let original = b"target must stay untouched\n";
    fs::write(&target, original).expect("write symlink target");

    let data = temporary.path().join("data");
    fs::create_dir_all(&data).expect("create data directory");
    symlink(&target, data.join("skilled.sqlite3")).expect("link database leaf");
    let app =
        SkilledApp::open(AppEnvironment::new(&home, &data, "")).expect("open degraded application");
    assert_eq!(app.view(), View::Inventory);
    assert_eq!(fs::read(&target).expect("reread target"), original);
    assert!(
        fs::symlink_metadata(data.join("skilled.sqlite3"))
            .expect("read database link")
            .file_type()
            .is_symlink()
    );

    let linked_data = temporary.path().join("linked-data");
    let real_data = temporary.path().join("real-data");
    fs::create_dir_all(&real_data).expect("create real data directory");
    symlink(&real_data, &linked_data).expect("link data directory leaf");
    let app = SkilledApp::open(AppEnvironment::new(&home, &linked_data, ""))
        .expect("open degraded linked data directory");
    assert_eq!(app.view(), View::Inventory);
    assert!(!real_data.join("skilled.sqlite3").exists());
}
