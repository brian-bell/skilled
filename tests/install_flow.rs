//! Installing a skill, end to end, through the application's own actions.
//!
//! Every fixture builds its own temporary home; no test may read the real user
//! home or a real agent skill root. Directory symbolic links are the managed
//! installation shape, so the whole suite is Unix-only.
#![cfg(unix)]

use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::Command,
};

use skilled::{
    Action, AgentKind, AppEnvironment, SkilledApp,
    inventory::{Finding, FindingSeverity, InstallationHealth},
    operations::{
        InstallPrompt, InstallStatus, OpenCodeOutlook, StepOutcome, TargetDisposition,
        VerifyFailure, VerifyWithheld, verify_install,
    },
    resolution::OpenCodeResolution,
};

const CLAUDE_CODE_ROOT: &str = ".claude/skills";
const CODEX_ROOT: &str = ".agents/skills";
const OPENCODE_ROOT: &str = ".config/opencode/skills";

const ROOTS: [(AgentKind, &str); 3] = [
    (AgentKind::ClaudeCode, CLAUDE_CODE_ROOT),
    (AgentKind::Codex, CODEX_ROOT),
    (AgentKind::OpenCode, OPENCODE_ROOT),
];

/// The acceptance criterion of this slice: one common variant reaches all three
/// agents as individual directory symbolic links, only after a preview the user
/// confirmed, with an ownership receipt for each and a verified postcondition.
#[test]
fn a_confirmed_plan_links_every_agent_records_receipts_and_verifies_itself() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    let mut app = fixture.registered(&repository);
    fixture.create_root_parents();
    focus_first_variant(&mut app);

    dispatch(&mut app, Action::BeginInstall);
    let Some(InstallPrompt::Preview(plan)) = app.pending_install() else {
        panic!(
            "a preview is shown before anything is written: {:?}",
            app.pending_install()
        );
    };
    assert!(plan.is_executable());
    // Nothing has been written while the preview is on screen.
    for (agent, root) in ROOTS {
        assert!(
            !fixture.home().join(root).join("portable").exists(),
            "{agent:?} was written to before confirmation"
        );
    }

    dispatch(&mut app, Action::ConfirmInstall);

    let Some(InstallPrompt::Report(outcome)) = app.pending_install() else {
        panic!("a report follows the apply: {:?}", app.pending_install());
    };
    assert_eq!(outcome.status(), InstallStatus::Installed);
    assert!(outcome.verification().is_verified());
    let variant = repository
        .join("skills/portable")
        .canonicalize()
        .expect("canonical variant directory");
    for (agent, root) in ROOTS {
        let link = fixture.home().join(root).join("portable");
        assert!(
            fs::symlink_metadata(&link)
                .expect("the link exists")
                .file_type()
                .is_symlink(),
            "{agent:?} should hold a symbolic link"
        );
        assert_eq!(fs::read_link(&link).expect("read the link"), variant);
        assert_eq!(
            outcome.step(agent).map(|step| step.outcome()),
            Some(&StepOutcome::Created)
        );
    }

    let receipts = app.receipts().expect("read receipts");
    assert_eq!(receipts.len(), 3);
    for (agent, root) in ROOTS {
        let receipt = receipts
            .iter()
            .find(|receipt| receipt.agent() == agent)
            .expect("one receipt per created link");
        assert_eq!(receipt.skill_name(), "portable");
        assert_eq!(
            receipt.link_path(),
            fixture.home().join(root).join("portable")
        );
        assert_eq!(receipt.link_target(), variant);
        assert_eq!(receipt.source_id(), Some(app.sources()[0].id()));
    }

    // The rescan the apply performed is what the report rests on, and it sees
    // three healthy installations of one name.
    let row = app.inventory().row("portable").expect("the installed row");
    assert_eq!(row.health(), InstallationHealth::Healthy);
    assert_eq!(row.observations().count(), 3);
    assert!(
        !fixture.an_agent_was_launched(),
        "no agent executable may be launched"
    );
}

/// A catalog that explicitly excludes OpenCode is still installable for the
/// compatible agents. OpenCode discovers those links through its documented
/// compatibility roots, and verification must compare that incompatible
/// exposure with the plan instead of treating it as an unknown outcome.
#[test]
fn incompatible_opencode_exposure_matches_the_confirmed_plan() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    let mut app = fixture.registered_without_opencode_compatibility(&repository);
    fixture.create_root_parents();
    focus_first_variant(&mut app);

    dispatch(&mut app, Action::BeginInstall);
    let Some(InstallPrompt::Preview(plan)) = app.pending_install() else {
        panic!(
            "a preview is shown before the apply: {:?}",
            app.pending_install()
        );
    };
    assert!(plan.is_executable());
    assert!(
        plan.warnings()
            .iter()
            .any(|warning| warning.contains("not registered for OpenCode")),
        "{:?}",
        plan.warnings()
    );
    assert_eq!(
        plan.opencode_outlook(),
        Some(&OpenCodeOutlook::Exposure {
            winner: fixture.root(AgentKind::Codex).join("portable")
        })
    );
    dispatch(&mut app, Action::ConfirmInstall);

    let Some(InstallPrompt::Report(outcome)) = app.pending_install() else {
        panic!("a report follows the apply: {:?}", app.pending_install());
    };
    assert_eq!(outcome.status(), InstallStatus::Installed);
    assert!(
        outcome.verification().is_verified(),
        "{:?}",
        outcome.verification()
    );
    assert!(outcome.verification().is_complete());
    assert!(outcome.step(AgentKind::OpenCode).is_none());
    assert_eq!(app.receipts().expect("read receipts").len(), 2);
    assert!(matches!(
        app.inventory()
            .row("portable")
            .and_then(|row| row.opencode_resolution()),
        Some(OpenCodeResolution::IncompatibleExposure { .. })
    ));
    assert!(!fixture.root(AgentKind::OpenCode).join("portable").exists());
}

/// A blocked plan is shown and refuses to be confirmed. Nothing is written
/// anywhere — not even to the targets that were free — and the object standing
/// in the way is left exactly as it was.
#[test]
fn a_blocked_plan_cannot_be_confirmed_and_writes_nothing_anywhere() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    let mut app = fixture.registered(&repository);
    fixture.create_root_parents();
    let occupied = fixture.create_root(AgentKind::Codex).join("portable");
    fs::write(&occupied, "someone else's file").expect("write the occupant");
    focus_first_variant(&mut app);

    dispatch(&mut app, Action::BeginInstall);
    let Some(InstallPrompt::Preview(plan)) = app.pending_install() else {
        panic!("a blocked plan is still previewed");
    };
    assert!(!plan.is_executable());
    assert_eq!(
        plan.blocking_findings()
            .map(|(_, finding)| finding.code())
            .collect::<Vec<_>>(),
        ["install.physical_path_collision"]
    );

    dispatch(&mut app, Action::ConfirmInstall);

    // Confirmation is refused, so the preview is still what is on screen.
    assert!(matches!(
        app.pending_install(),
        Some(InstallPrompt::Preview(_))
    ));
    assert_eq!(
        fs::read_to_string(&occupied).expect("the occupant survives"),
        "someone else's file"
    );
    for (agent, root) in [
        (AgentKind::ClaudeCode, CLAUDE_CODE_ROOT),
        (AgentKind::OpenCode, OPENCODE_ROOT),
    ] {
        assert!(
            !fixture.home().join(root).join("portable").exists(),
            "{agent:?} was written to despite a blocked plan"
        );
    }
    assert!(app.receipts().expect("read receipts").is_empty());
}

/// A link that is already exactly what the plan would create is left alone, and
/// no receipt is written for it: claiming ownership of a link Skilled did not
/// create is adoption, which this release does not do.
#[test]
fn an_existing_identical_link_is_neither_rewritten_nor_adopted() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    let mut app = fixture.registered(&repository);
    fixture.create_root_parents();
    fixture.install_symlink(
        AgentKind::Codex,
        "portable",
        &repository.join("skills/portable"),
    );
    focus_first_variant(&mut app);

    dispatch(&mut app, Action::BeginInstall);
    dispatch(&mut app, Action::ConfirmInstall);

    let Some(InstallPrompt::Report(outcome)) = app.pending_install() else {
        panic!("a report follows the apply");
    };
    assert_eq!(outcome.status(), InstallStatus::Installed);
    assert!(outcome.step(AgentKind::Codex).is_none());
    // The pre-existing link is untouched, and unclaimed.
    assert_eq!(
        fs::read_link(fixture.home().join(CODEX_ROOT).join("portable")).expect("read the link"),
        repository.join("skills/portable")
    );
    let receipts = app.receipts().expect("read receipts");
    assert_eq!(receipts.len(), 2);
    assert!(
        receipts
            .iter()
            .all(|receipt| receipt.agent() != AgentKind::Codex)
    );
}

/// Spec 15: the machine is read again immediately before each write. A target
/// that changed between the preview and the confirmation is not written to, and
/// the run stops there rather than carrying on into the targets behind it.
#[test]
fn a_target_that_changed_since_the_preview_stops_the_run_where_it_stands() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    let mut app = fixture.registered(&repository);
    fixture.create_root_parents();
    focus_first_variant(&mut app);

    dispatch(&mut app, Action::BeginInstall);
    // Codex is the second target, so Claude Code is written before the
    // precondition that fails is reached.
    let root = fixture.create_root(AgentKind::Codex);
    fs::write(root.join("portable"), "arrived after the preview").expect("occupy the slot");
    dispatch(&mut app, Action::ConfirmInstall);

    let Some(InstallPrompt::Report(outcome)) = app.pending_install() else {
        panic!("a report follows the apply");
    };
    assert_eq!(outcome.status(), InstallStatus::PartiallyApplied);
    assert_eq!(
        outcome
            .step(AgentKind::ClaudeCode)
            .map(|step| step.outcome()),
        Some(&StepOutcome::Created)
    );
    assert!(matches!(
        outcome.step(AgentKind::Codex).map(|step| step.outcome()),
        Some(StepOutcome::Failed(_))
    ));
    assert_eq!(
        outcome.step(AgentKind::OpenCode).map(|step| step.outcome()),
        Some(&StepOutcome::Unattempted)
    );

    // The link written before the failure is real, healthy, and receipted:
    // nothing is rolled back, and the report says exactly what happened.
    assert!(
        fixture
            .home()
            .join(CLAUDE_CODE_ROOT)
            .join("portable")
            .is_dir()
    );
    assert!(!fixture.home().join(OPENCODE_ROOT).join("portable").exists());
    let receipts = app.receipts().expect("read receipts");
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].agent(), AgentKind::ClaudeCode);
}

/// A plan whose targets are all already installed has nothing to write, and
/// says so rather than reporting an install it did not perform.
#[test]
fn a_plan_with_no_work_left_reports_that_there_was_nothing_to_do() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    let mut app = fixture.registered(&repository);
    fixture.create_root_parents();
    for (agent, _) in ROOTS {
        fixture.install_symlink(agent, "portable", &repository.join("skills/portable"));
    }
    focus_first_variant(&mut app);

    dispatch(&mut app, Action::BeginInstall);
    let Some(InstallPrompt::Preview(plan)) = app.pending_install() else {
        panic!("a preview is shown");
    };
    assert!(!plan.is_executable());
    assert!(!plan.is_blocked());
    assert!(plan.targets().iter().all(|target| matches!(
        target.disposition(),
        TargetDisposition::AlreadyInstalled { .. }
    )));

    dispatch(&mut app, Action::ConfirmInstall);
    assert!(matches!(
        app.pending_install(),
        Some(InstallPrompt::Preview(_))
    ));
}

/// The prompt owns the keyboard while it is open, and dismissing it leaves the
/// inventory the rescan produced rather than one taken before the install.
#[test]
fn the_prompt_swallows_other_actions_and_dismissal_keeps_the_fresh_inventory() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    let mut app = fixture.registered(&repository);
    fixture.create_root_parents();
    focus_first_variant(&mut app);

    dispatch(&mut app, Action::BeginInstall);
    dispatch(&mut app, Action::OpenInventory);
    dispatch(&mut app, Action::MoveSourcesSelection(1));
    assert!(
        matches!(app.pending_install(), Some(InstallPrompt::Preview(_))),
        "the preview should have swallowed both"
    );
    assert_eq!(app.view(), skilled::View::Sources);

    dispatch(&mut app, Action::ConfirmInstall);
    dispatch(&mut app, Action::DismissInstall);

    assert!(app.pending_install().is_none());
    assert_eq!(
        app.inventory()
            .row("portable")
            .expect("the installed row")
            .observations()
            .count(),
        3
    );
}

/// Spec 11.4: a zero exit is not evidence. Verification re-observes what was
/// written, so a link that stopped being what the plan called for is reported
/// against the plan rather than passed over.
#[test]
fn verification_reports_a_link_that_stopped_matching_the_plan() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    let mut app = fixture.registered(&repository);
    fixture.create_root_parents();
    focus_first_variant(&mut app);
    dispatch(&mut app, Action::BeginInstall);
    dispatch(&mut app, Action::ConfirmInstall);
    let Some(InstallPrompt::Report(outcome)) = app.pending_install() else {
        panic!("a report follows the apply");
    };
    assert!(outcome.verification().is_verified());
    let plan = outcome.plan().clone();
    let applied = outcome.applied().clone();

    // Something moves the link after the install, and the next scan sees it.
    let link = fixture.home().join(CODEX_ROOT).join("portable");
    fs::remove_file(&link).expect("remove the link");
    let elsewhere = fixture.directory.path().join("elsewhere/portable");
    write_skill(&elsewhere, "portable");
    symlink(&elsewhere, &link).expect("point it elsewhere");
    dispatch(&mut app, Action::DismissInstall);
    dispatch(&mut app, Action::OpenInventory);

    let report = verify_install(&plan, &applied, app.inventory());

    assert!(!report.is_verified());
    assert_eq!(
        report
            .failures()
            .iter()
            .map(VerifyFailure::agent)
            .collect::<Vec<_>>(),
        // OpenCode reads Codex's root, so the same swap leaves it choosing
        // between two directories rather than loading the link it was given.
        [AgentKind::Codex, AgentKind::OpenCode]
    );
    assert!(
        report.failures()[1].observed().contains("OpenCode"),
        "{:?}",
        report.failures()[1]
    );
}

/// The variant directory is a precondition too. A checkout that moved between
/// the preview and the confirmation would otherwise leave Skilled owning links
/// it created that resolve to nothing, in a release with no repair.
#[test]
fn a_variant_directory_that_moved_since_the_preview_stops_the_run() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    let mut app = fixture.registered(&repository);
    fixture.create_root_parents();
    focus_first_variant(&mut app);

    dispatch(&mut app, Action::BeginInstall);
    fs::rename(&repository, fixture.path().join("moved")).expect("move the checkout");
    dispatch(&mut app, Action::ConfirmInstall);

    let Some(InstallPrompt::Report(outcome)) = app.pending_install() else {
        panic!("a report follows the apply");
    };
    assert_eq!(outcome.status(), InstallStatus::NotApplied);
    assert!(
        matches!(
            outcome.step(AgentKind::ClaudeCode).map(|step| step.outcome()),
            Some(StepOutcome::Failed(reason)) if reason.contains("no longer the directory")
        ),
        "{:?}",
        outcome.step(AgentKind::ClaudeCode)
    );
    for (_, root) in ROOTS {
        assert!(!fixture.home().join(root).join("portable").exists());
    }
    assert!(app.receipts().expect("receipts").is_empty());
}

/// Deselecting an agent is an ordinary configuration, not a broken install.
///
/// A root Skilled was told to leave alone is one it never read, so it can say
/// nothing about what OpenCode would resolve through it. That gap is stated as
/// a gap — before the write and after it — rather than turning a correct
/// install into a verification failure.
#[test]
fn an_unread_root_leaves_opencode_unstated_rather_than_unverified() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    let mut app = fixture.registered(&repository);
    fixture.create_root_parents();
    deselect_codex(&mut app);
    focus_first_variant(&mut app);

    dispatch(&mut app, Action::BeginInstall);
    let Some(InstallPrompt::Preview(plan)) = app.pending_install() else {
        panic!("a preview is shown: {:?}", app.pending_install());
    };
    assert!(plan.is_executable(), "{:?}", plan.targets());
    assert!(!plan.is_blocked());
    // The gap is said out loud rather than passed over.
    assert!(
        plan.warnings()
            .iter()
            .any(|warning| warning.contains("Codex")),
        "{:?}",
        plan.warnings()
    );

    dispatch(&mut app, Action::ConfirmInstall);

    let Some(InstallPrompt::Report(outcome)) = app.pending_install() else {
        panic!("a report follows the apply");
    };
    assert_eq!(
        outcome.status(),
        InstallStatus::Installed,
        "{:?}",
        outcome.verification()
    );
    // Nothing disagreed with the plan — and the one postcondition Skilled could
    // not check is reported as withheld rather than folded into the pass.
    assert!(outcome.verification().is_verified());
    assert!(!outcome.verification().is_complete());
    assert_eq!(
        outcome
            .verification()
            .withheld()
            .iter()
            .map(VerifyWithheld::agent)
            .collect::<Vec<_>>(),
        [AgentKind::OpenCode]
    );
    let reason = outcome.verification().withheld()[0].reason();
    assert!(reason.contains("Codex"), "{reason}");
    // The two kinds of unknown are never flattened: this root was never read,
    // which is not the same as one whose entry could not be followed.
    assert!(reason.contains("did not read"), "{reason}");
    assert!(fixture.home().join(OPENCODE_ROOT).join("portable").is_dir());
    // Codex was left entirely alone: no step, no link, no receipt.
    assert!(outcome.step(AgentKind::Codex).is_none());
    assert!(!fixture.home().join(CODEX_ROOT).join("portable").exists());
    assert_eq!(app.receipts().expect("receipts").len(), 2);
}

/// The postcondition is the directory OpenCode loads, not the shape of the
/// answer. Content that changes under a name while the classification stays the
/// same is still not what the plan described.
#[test]
fn a_different_winner_under_the_same_classification_is_a_verification_failure() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    let mut app = fixture.registered(&repository);
    fixture.create_root_parents();
    // OpenCode already holds the very link the plan would create, so it is not
    // a step this install writes — which is the case where the plan's own
    // expectation is what the check afterwards compares against.
    fixture.install_symlink(
        AgentKind::OpenCode,
        "portable",
        &repository.join("skills/portable"),
    );
    focus_first_variant(&mut app);
    dispatch(&mut app, Action::BeginInstall);
    dispatch(&mut app, Action::ConfirmInstall);
    let Some(InstallPrompt::Report(outcome)) = app.pending_install() else {
        panic!("a report follows the apply");
    };
    assert!(outcome.step(AgentKind::OpenCode).is_none());
    let plan = outcome.plan().clone();
    let applied = outcome.applied().clone();
    // The plan expected OpenCode to load through its own root. Take that link
    // away and Codex's remains: still one directory selected, reached through a
    // different slot from the one the plan named.
    fs::remove_file(fixture.home().join(OPENCODE_ROOT).join("portable"))
        .expect("remove OpenCode's link");
    dispatch(&mut app, Action::DismissInstall);
    dispatch(&mut app, Action::OpenInventory);

    let report = verify_install(&plan, &applied, app.inventory());

    assert!(!report.is_verified(), "{report:?}");
    assert_eq!(
        report
            .failures()
            .iter()
            .map(VerifyFailure::agent)
            .collect::<Vec<_>>(),
        [AgentKind::OpenCode]
    );
    assert!(
        report.failures()[0]
            .observed()
            .contains("not what the plan described"),
        "{:?}",
        report.failures()[0]
    );
}

/// Spec 20.5: what an install leaves behind survives the process that made it.
///
/// A fresh application, opened over the same home and the same metadata, sees
/// the installation as managed and healthy, and still holds the receipts that
/// say Skilled put it there.
#[test]
fn an_installation_is_still_managed_and_healthy_after_a_restart() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    let mut app = fixture.registered(&repository);
    fixture.create_root_parents();
    focus_first_variant(&mut app);
    dispatch(&mut app, Action::BeginInstall);
    dispatch(&mut app, Action::ConfirmInstall);
    drop(app);

    let reopened = fixture.app();

    let row = reopened.inventory().row("portable").expect("installed row");
    assert_eq!(row.health(), InstallationHealth::Healthy);
    assert_eq!(row.observations().count(), 3);
    for observation in row.observations() {
        let resolution = observation.resolution().expect("a resolved variant");
        assert_eq!(resolution.source_label(), "library");
        assert_eq!(resolution.skill_name(), "portable");
    }
    // Reaching one directory through all three roots is a benign alias, which
    // the scanner notes and nothing more: nothing about it needs attention.
    assert_eq!(
        row.findings().map(Finding::code).collect::<Vec<_>>(),
        ["variant.benign_alias"]
    );
    assert!(
        row.findings()
            .all(|finding| finding.severity() == FindingSeverity::Info)
    );
    assert_eq!(reopened.receipts().expect("receipts").len(), 3);
}

/// Rerun setup and leave Codex deselected, through the steps a user would take.
fn deselect_codex(app: &mut SkilledApp) {
    dispatch(app, Action::OpenSettings);
    dispatch(app, Action::RerunSetup);
    dispatch(app, Action::Continue);
    dispatch(app, Action::MoveSelection(1));
    dispatch(app, Action::ToggleSelection);
    for _ in 0..6 {
        dispatch(app, Action::Continue);
    }
    assert!(!app.agent(AgentKind::Codex).selected());
}

/// Apply one action the way the runner does.
///
/// The runner draws a frame before every key and hands the application what it
/// measured, which is what a confirmation waits on. These tests do not render,
/// so they stand in the measurement a terminal large enough to hold the dialog
/// would report: the whole plan on screen, nothing left to scroll to.
fn dispatch(app: &mut SkilledApp, action: Action) {
    if app.pending_install().is_some() {
        app.note_detail_max_scroll(Some(0));
    }
    let update = app.update(action);
    app.perform_effects(update.effects())
        .expect("perform effects");
}

/// Stand on the first skill variant of the first registered source, the way a
/// user reaches it: open Sources, then drill into the variants pane.
fn focus_first_variant(app: &mut SkilledApp) {
    dispatch(app, Action::OpenSources);
    dispatch(app, Action::AdvanceSourcesPane);
}

struct Fixture {
    directory: tempfile::TempDir,
    /// A search path holding an executable named after each agent, which
    /// records having been run.
    ///
    /// Every application in this file is built with it, so acceptance 18 —
    /// that Skilled never launches a coding agent — is proved by every test
    /// here rather than by whichever one remembered to ask.
    executables: PathBuf,
    witness: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("temporary application directory");
        let executables = directory.path().join("trap");
        let witness = directory.path().join("agent-was-launched");
        fs::create_dir_all(&executables).expect("create trap directory");
        for name in ["claude", "codex", "opencode"] {
            let executable = executables.join(name);
            fs::write(
                &executable,
                format!("#!/bin/sh\ntouch {}\n", witness.display()),
            )
            .expect("write trap executable");
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
                .expect("mark trap executable");
        }
        Self {
            directory,
            executables,
            witness,
        }
    }

    /// Whether any agent executable on the search path was run.
    fn an_agent_was_launched(&self) -> bool {
        self.witness.exists()
    }

    fn path(&self) -> &Path {
        self.directory.path()
    }

    fn home(&self) -> PathBuf {
        self.directory.path().join("home")
    }

    fn environment(&self) -> AppEnvironment {
        AppEnvironment::new(
            self.home(),
            self.directory.path().join("data"),
            &self.executables,
        )
    }

    fn app(&self) -> SkilledApp {
        SkilledApp::open(self.environment()).expect("open application")
    }

    fn registered(&self, repository: &Path) -> SkilledApp {
        let mut app = self.app();
        let preview = app.preview_source(repository).expect("preview source");
        app.confirm_source(preview).expect("register source");
        for _ in 0..7 {
            let update = app.update(Action::Continue);
            app.perform_effects(update.effects())
                .expect("perform setup effects");
        }
        app
    }

    /// Complete setup after explicitly removing OpenCode from a common
    /// catalog's stored compatibility declaration.
    fn registered_without_opencode_compatibility(&self, repository: &Path) -> SkilledApp {
        let mut app = self.app();
        for _ in 0..3 {
            dispatch(&mut app, Action::Continue);
        }
        dispatch(&mut app, Action::BeginAddSource);
        for character in repository.to_string_lossy().chars() {
            dispatch(&mut app, Action::AppendSourcePath(character));
        }
        dispatch(&mut app, Action::SubmitSourcePath);
        dispatch(
            &mut app,
            Action::ToggleCatalogCompatibility(AgentKind::OpenCode),
        );
        for _ in 0..3 {
            dispatch(&mut app, Action::Continue);
        }
        app
    }

    fn source(&self, name: &str, skills: &[&str]) -> PathBuf {
        let repository = self.directory.path().join(name);
        for skill in skills {
            write_skill(&repository.join("skills").join(skill), skill);
        }
        initialize_repository(&repository);
        repository
    }

    fn root(&self, agent: AgentKind) -> PathBuf {
        self.home().join(match agent {
            AgentKind::ClaudeCode => CLAUDE_CODE_ROOT,
            AgentKind::Codex => CODEX_ROOT,
            AgentKind::OpenCode => OPENCODE_ROOT,
        })
    }

    fn create_root_parents(&self) {
        for (agent, _) in ROOTS {
            let root = self.root(agent);
            fs::create_dir_all(root.parent().expect("every root has a parent"))
                .expect("create the root's parent");
        }
    }

    fn create_root(&self, agent: AgentKind) -> PathBuf {
        let root = self.root(agent);
        fs::create_dir_all(&root).expect("create agent skill root");
        root
    }

    fn install_symlink(&self, agent: AgentKind, name: &str, target: &Path) {
        let root = self.create_root(agent);
        symlink(target, root.join(name)).expect("install symbolic link");
    }
}

fn write_skill(directory: &Path, name: &str) {
    fs::create_dir_all(directory).expect("create skill directory");
    fs::write(
        directory.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {name} fixture\n---\n# {name}\n"),
    )
    .expect("write SKILL.md");
}

fn initialize_repository(repository: &Path) {
    fs::create_dir_all(repository).expect("create repository directory");
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
        assert!(output.status.success(), "Git command failed: {output:?}");
    }
}
