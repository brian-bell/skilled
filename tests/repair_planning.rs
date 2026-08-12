//! Repair planning over real temporary homes and registered Git sources.
#![cfg(unix)]

use std::{
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    process::Command,
};

use ratatui::{Terminal, backend::TestBackend};

use skilled::{
    Action, AgentKind, AppEnvironment, SkilledApp,
    cli::{self, ExitCodeKind},
    operations::{
        ReceiptOperation, RepairDisposition, RepairPrompt, RepairStatus, RepairStepOutcome,
        plan_repair, probe_repair,
    },
};

#[test]
fn an_owned_healthy_link_is_replanned_to_the_agent_specific_variant_selected_today() {
    let fixture = Fixture::new();
    let common = fixture.source("common", "skills", "portable");
    let mut app = fixture.registered(&common);
    fixture.create_root_parents();
    fixture.install(&mut app);
    let old_target = common.join("skills/portable").canonicalize().unwrap();
    let link = fixture.root(AgentKind::Codex).join("portable");

    let specific = fixture.source("specific", ".agents/skills", "portable");
    let preview = app
        .preview_source(&specific)
        .expect("preview specific source");
    app.confirm_source(preview)
        .expect("register specific source");
    let probe = probe_repair(
        app.agents(),
        app.sources(),
        "portable",
        AgentKind::Codex,
        app.home(),
    );
    let receipts = app.receipts().expect("read receipts");

    let plan = plan_repair(
        app.agents(),
        app.sources(),
        "portable",
        AgentKind::Codex,
        &probe,
        &receipts,
    );

    assert_eq!(
        plan.disposition(),
        &RepairDisposition::ReplaceLink { dangling: false }
    );
    assert_eq!(plan.link_path(), link);
    assert_eq!(plan.current_target(), old_target);
    assert_eq!(
        plan.new_target(),
        Some(
            specific
                .join(".agents/skills/portable")
                .canonicalize()
                .unwrap()
                .as_path()
        )
    );
    assert!(plan.source_changed());
    assert_eq!(fs::read_link(&link).unwrap(), old_target, "a plan is inert");
    let overlay = app
        .doctor_findings()
        .into_iter()
        .filter(|entry| entry.finding().code() == "install.wrong_managed_target")
        .collect::<Vec<_>>();
    assert_eq!(overlay.len(), 1);
    assert_eq!(overlay[0].observation().unwrap().path(), link);
    assert_eq!(app.finding_count(), app.doctor_findings().len());
    dispatch(&mut app, Action::OpenDoctor);
    assert!(app.can_repair_selection());
    let doctor = render_text(&app, 120, 60);
    assert!(doctor.contains("Repair: offered"), "{doctor}");
    assert!(doctor.contains("r Repair"), "{doctor}");
    dispatch(&mut app, Action::BeginRepair);
    assert!(matches!(
        app.pending_repair(),
        Some(RepairPrompt::Preview(plan))
            if plan.disposition() == &RepairDisposition::ReplaceLink { dangling: false }
    ));
    let preview = render_text(&app, 120, 60);
    assert!(preview.contains("Repair skill"), "{preview}");
    assert!(preview.contains("Old target:"), "{preview}");
    assert!(preview.contains("New target:"), "{preview}");
}

#[test]
fn repair_yes_keeps_the_same_guards_and_reports_a_cross_source_repair() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", "skills", "portable");
    let mut app = fixture.registered(&repository);
    fixture.create_root_parents();
    fixture.install(&mut app);
    let moved = fixture.directory.path().join("moved-library");
    fs::rename(&repository, &moved).unwrap();
    let preview = app.preview_source(&moved).unwrap();
    app.confirm_source(preview).unwrap();
    drop(app);
    let mut input = Cursor::new(Vec::<u8>::new());
    let mut output = Vec::new();

    let code = cli::run(
        &[
            "repair".to_owned(),
            "--yes".to_owned(),
            "--skill".to_owned(),
            "portable".to_owned(),
            "--agent".to_owned(),
            "codex".to_owned(),
        ],
        fixture.environment(),
        &mut input,
        &mut output,
    );
    let output = String::from_utf8(output).unwrap();

    assert_eq!(code, ExitCodeKind::Success, "{output}");
    assert!(!output.contains("Proceed?"), "{output}");
    assert!(output.contains("source changed"), "{output}");
    assert!(
        output.contains(&repository.display().to_string()),
        "{output}"
    );
    assert!(output.contains(&moved.display().to_string()), "{output}");
    assert!(output.contains("Verified"), "{output}");
}

#[test]
fn repair_yes_requires_both_the_skill_and_single_agent() {
    let fixture = Fixture::new();
    for (arguments, missing) in [
        (
            vec![
                "repair".to_owned(),
                "--yes".to_owned(),
                "--agent".to_owned(),
                "codex".to_owned(),
            ],
            "--yes requires --skill",
        ),
        (
            vec![
                "repair".to_owned(),
                "--yes".to_owned(),
                "--skill".to_owned(),
                "portable".to_owned(),
            ],
            "--yes requires --agent",
        ),
    ] {
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();

        let code = cli::run(&arguments, fixture.environment(), &mut input, &mut output);

        assert_eq!(code, ExitCodeKind::InvalidRequest);
        assert!(String::from_utf8(output).unwrap().contains(missing));
    }
}

#[test]
fn a_confirmed_dangling_link_is_atomically_repaired_rescanned_and_receipted() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", "skills", "portable");
    let mut app = fixture.registered(&repository);
    fixture.create_root_parents();
    fixture.install(&mut app);
    let old_target = repository.join("skills/portable").canonicalize().unwrap();
    let moved = fixture.directory.path().join("moved-library");
    fs::rename(&repository, &moved).expect("move checkout");
    let preview = app.preview_source(&moved).expect("preview moved checkout");
    app.confirm_source(preview)
        .expect("register moved checkout");
    let link = fixture.root(AgentKind::Codex).join("portable");
    assert_eq!(fs::read_link(&link).unwrap(), old_target);
    assert!(
        !link.exists(),
        "the installed link is dangling before repair"
    );

    dispatch(&mut app, Action::OpenDoctor);
    // The first dangling finding is Claude Code; move to Codex so the asserted
    // path is stable and proves the single-target contract.
    dispatch(&mut app, Action::MoveDoctorSelection(1));
    dispatch(&mut app, Action::BeginRepair);
    let Some(RepairPrompt::Preview(plan)) = app.pending_repair() else {
        panic!("repair should show a preview: {:?}", app.pending_repair());
    };
    assert!(plan.is_executable());
    assert_eq!(plan.link_path(), link);
    assert_eq!(plan.current_target(), old_target);
    assert_eq!(
        fs::read_link(&link).unwrap(),
        old_target,
        "preview is inert"
    );

    dispatch(&mut app, Action::ConfirmRepair);
    assert!(matches!(
        app.pending_repair(),
        Some(RepairPrompt::Preview(_))
    ));
    assert_eq!(
        fs::read_link(&link).unwrap(),
        old_target,
        "confirmation is gated until the full preview has been seen"
    );

    app.note_detail_max_scroll(Some(0));
    dispatch(&mut app, Action::ConfirmRepair);

    let Some(RepairPrompt::Report(outcome)) = app.pending_repair() else {
        panic!("repair should show a report: {:?}", app.pending_repair());
    };
    assert_eq!(outcome.status(), RepairStatus::Repaired);
    assert_eq!(
        outcome.applied().step().map(|step| step.outcome()),
        Some(&RepairStepOutcome::Repaired)
    );
    assert!(outcome.verification().is_verified());
    assert_eq!(
        fs::read_link(&link).unwrap(),
        moved.join("skills/portable").canonicalize().unwrap()
    );
    let receipts = app.receipts().unwrap();
    let newest = receipts.last().expect("repair receipt");
    assert_eq!(newest.operation(), ReceiptOperation::Repair);
    assert_eq!(newest.link_path(), link);
    assert!(
        fs::read_dir(fixture.root(AgentKind::Codex))
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".skilled-repair-")),
        "no temporary link remains"
    );
}

#[test]
fn a_repair_source_relocated_after_preview_is_refused_without_modifying_the_link() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", "skills", "portable");
    let mut app = fixture.registered(&repository);
    fixture.create_root_parents();
    fixture.install(&mut app);
    let old_target = repository.join("skills/portable").canonicalize().unwrap();
    let moved = fixture.directory.path().join("moved-library");
    fs::rename(&repository, &moved).unwrap();
    let preview = app.preview_source(&moved).unwrap();
    app.confirm_source(preview).unwrap();
    let link = fixture.root(AgentKind::Codex).join("portable");

    dispatch(&mut app, Action::OpenDoctor);
    dispatch(&mut app, Action::MoveDoctorSelection(1));
    dispatch(&mut app, Action::BeginRepair);
    assert!(matches!(
        app.pending_repair(),
        Some(RepairPrompt::Preview(plan)) if plan.is_executable()
    ));
    fs::rename(&moved, fixture.directory.path().join("moved-again")).unwrap();
    app.note_detail_max_scroll(Some(0));
    dispatch(&mut app, Action::ConfirmRepair);

    let Some(RepairPrompt::Report(outcome)) = app.pending_repair() else {
        panic!("repair should report its guarded refusal");
    };
    assert_eq!(outcome.status(), RepairStatus::NotApplied);
    assert!(matches!(
        outcome.applied().step().map(|step| step.outcome()),
        Some(RepairStepOutcome::Failed(reason))
            if reason.contains("no longer the directory the plan resolved")
    ));
    assert_eq!(fs::read_link(&link).unwrap(), old_target);
    assert_no_repair_temporary(&fixture.root(AgentKind::Codex));
}

#[test]
fn a_repointed_link_is_not_owned_by_a_path_only_receipt() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", "skills", "portable");
    let mut app = fixture.registered(&repository);
    fixture.create_root_parents();
    fixture.install(&mut app);
    let link = fixture.root(AgentKind::Codex).join("portable");
    fs::remove_file(&link).expect("remove installed link");
    std::os::unix::fs::symlink(repository.join("elsewhere"), &link).expect("repoint link by hand");
    let before = fs::read_link(&link).unwrap();
    let probe = probe_repair(
        app.agents(),
        app.sources(),
        "portable",
        AgentKind::Codex,
        app.home(),
    );

    let plan = plan_repair(
        app.agents(),
        app.sources(),
        "portable",
        AgentKind::Codex,
        &probe,
        &app.receipts().unwrap(),
    );

    assert_eq!(
        plan.blocking_finding().map(|finding| finding.code()),
        Some("repair.unproven_link")
    );
    assert_eq!(fs::read_link(&link).unwrap(), before);
}

#[test]
fn a_current_owned_link_needs_no_repair_and_an_absent_entry_is_never_recreated() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", "skills", "portable");
    let mut app = fixture.registered(&repository);
    fixture.create_root_parents();
    fixture.install(&mut app);
    let link = fixture.root(AgentKind::Codex).join("portable");
    let receipts = app.receipts().unwrap();

    let probe = probe_repair(
        app.agents(),
        app.sources(),
        "portable",
        AgentKind::Codex,
        app.home(),
    );
    let plan = plan_repair(
        app.agents(),
        app.sources(),
        "portable",
        AgentKind::Codex,
        &probe,
        &receipts,
    );
    assert_eq!(plan.disposition(), &RepairDisposition::NothingToRepair);

    fs::remove_file(&link).unwrap();
    let probe = probe_repair(
        app.agents(),
        app.sources(),
        "portable",
        AgentKind::Codex,
        app.home(),
    );
    let plan = plan_repair(
        app.agents(),
        app.sources(),
        "portable",
        AgentKind::Codex,
        &probe,
        &receipts,
    );
    assert_eq!(
        plan.blocking_finding().map(|finding| finding.code()),
        Some("repair.nothing_to_replace")
    );
    assert!(!link.exists(), "repair never recreates an absent entry");
}

#[test]
fn a_physical_entry_is_refused_without_modification() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", "skills", "portable");
    let mut app = fixture.registered(&repository);
    fixture.create_root_parents();
    fixture.install(&mut app);
    let link = fixture.root(AgentKind::Codex).join("portable");
    fs::remove_file(&link).unwrap();
    fs::write(&link, "do not replace").unwrap();

    let probe = probe_repair(
        app.agents(),
        app.sources(),
        "portable",
        AgentKind::Codex,
        app.home(),
    );
    let plan = plan_repair(
        app.agents(),
        app.sources(),
        "portable",
        AgentKind::Codex,
        &probe,
        &app.receipts().unwrap(),
    );

    assert_eq!(
        plan.blocking_finding().map(|finding| finding.code()),
        Some("install.physical_path_collision")
    );
    assert_eq!(fs::read_to_string(&link).unwrap(), "do not replace");
    assert_no_repair_temporary(&fixture.root(AgentKind::Codex));
}

#[test]
fn an_unresolvable_owned_link_is_not_flattened_into_a_dangling_link() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", "skills", "portable");
    let mut app = fixture.registered(&repository);
    fixture.create_root_parents();
    fixture.install(&mut app);
    let variant = repository.join("skills/portable");
    let link = fixture.root(AgentKind::Codex).join("portable");
    let recorded_target = fs::read_link(&link).unwrap();
    fs::remove_dir_all(&variant).unwrap();
    std::os::unix::fs::symlink("portable", &variant).unwrap();

    let probe = probe_repair(
        app.agents(),
        app.sources(),
        "portable",
        AgentKind::Codex,
        app.home(),
    );
    let plan = plan_repair(
        app.agents(),
        app.sources(),
        "portable",
        AgentKind::Codex,
        &probe,
        &app.receipts().unwrap(),
    );

    assert_eq!(
        plan.blocking_finding().map(|finding| finding.code()),
        Some("install.unresolvable_symlink")
    );
    assert_eq!(fs::read_link(&link).unwrap(), recorded_target);
    assert_no_repair_temporary(&fixture.root(AgentKind::Codex));
}

#[test]
fn a_redirected_skill_root_is_refused_without_following_it() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", "skills", "portable");
    let mut app = fixture.registered(&repository);
    fixture.create_root_parents();
    fixture.install(&mut app);
    let root = fixture.root(AgentKind::Codex);
    let link = root.join("portable");
    let recorded_target = fs::read_link(&link).unwrap();
    fs::remove_file(&link).unwrap();
    fs::remove_dir(&root).unwrap();
    let redirected = fixture.directory.path().join("redirected-root");
    fs::create_dir(&redirected).unwrap();
    std::os::unix::fs::symlink(&recorded_target, redirected.join("portable")).unwrap();
    std::os::unix::fs::symlink(&redirected, &root).unwrap();

    let probe = probe_repair(
        app.agents(),
        app.sources(),
        "portable",
        AgentKind::Codex,
        app.home(),
    );
    let plan = plan_repair(
        app.agents(),
        app.sources(),
        "portable",
        AgentKind::Codex,
        &probe,
        &app.receipts().unwrap(),
    );

    assert_eq!(
        plan.blocking_finding().map(|finding| finding.code()),
        Some("install.redirected_root")
    );
    assert_eq!(
        fs::read_link(redirected.join("portable")).unwrap(),
        recorded_target
    );
    assert_no_repair_temporary(&redirected);
}

struct Fixture {
    directory: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self {
            directory: tempfile::tempdir().unwrap(),
        }
    }
    fn home(&self) -> PathBuf {
        self.directory.path().join("home")
    }
    fn root(&self, agent: AgentKind) -> PathBuf {
        self.home().join(match agent {
            AgentKind::ClaudeCode => ".claude/skills",
            AgentKind::Codex => ".agents/skills",
            AgentKind::OpenCode => ".config/opencode/skills",
        })
    }
    fn app(&self) -> SkilledApp {
        SkilledApp::open(self.environment()).unwrap()
    }
    fn environment(&self) -> AppEnvironment {
        AppEnvironment::new(self.home(), self.directory.path().join("data"), "")
    }
    fn registered(&self, repository: &Path) -> SkilledApp {
        let mut app = self.app();
        let preview = app.preview_source(repository).unwrap();
        app.confirm_source(preview).unwrap();
        for _ in 0..7 {
            dispatch(&mut app, Action::Continue);
        }
        app
    }
    fn source(&self, repository_name: &str, catalog: &str, skill: &str) -> PathBuf {
        let repository = self.directory.path().join(repository_name);
        write_skill(&repository.join(catalog).join(skill), skill);
        initialize_repository(&repository);
        repository
    }
    fn create_root_parents(&self) {
        for agent in AgentKind::ALL {
            fs::create_dir_all(self.root(agent).parent().unwrap()).unwrap();
        }
    }
    fn install(&self, app: &mut SkilledApp) {
        dispatch(app, Action::OpenSources);
        dispatch(app, Action::AdvanceSourcesPane);
        dispatch(app, Action::BeginInstall);
        app.note_detail_max_scroll(Some(0));
        dispatch(app, Action::ConfirmInstall);
        dispatch(app, Action::DismissInstall);
    }
}

fn dispatch(app: &mut SkilledApp, action: Action) {
    let update = app.update(action);
    app.perform_effects(update.effects()).unwrap();
}

fn write_skill(directory: &Path, name: &str) {
    fs::create_dir_all(directory).unwrap();
    fs::write(
        directory.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: fixture\n---\n# {name}\n"),
    )
    .unwrap();
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
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }
}

fn assert_no_repair_temporary(directory: &Path) {
    assert!(
        fs::read_dir(directory).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".skilled-repair-")),
        "no repair temporary entry should be present in {}",
        directory.display()
    );
}

fn render_text(app: &SkilledApp, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| {
            skilled::tui::render(frame, app);
        })
        .unwrap();
    let buffer = terminal.backend().buffer();
    let mut text = String::new();
    for y in buffer.area.y..buffer.area.y + buffer.area.height {
        for x in buffer.area.x..buffer.area.x + buffer.area.width {
            text.push_str(buffer[(x, y)].symbol());
        }
        text.push('\n');
    }
    text
}
