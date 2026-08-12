use std::fs;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use skilled::{
    Action, AppEnvironment, RegistryAvailability, SkilledApp, View,
    input::action_for_app_key,
    inventory::{InstallationHealth, Provenance},
};

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
