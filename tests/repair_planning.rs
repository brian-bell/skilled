//! Repair planning over real temporary homes and registered Git sources.
#![cfg(unix)]

use std::{
    fs,
    io::{self, BufRead, Cursor, Read},
    path::{Path, PathBuf},
    process::Command,
};

use ratatui::{Terminal, backend::TestBackend};

use skilled::{
    Action, AgentKind, AppEnvironment, SkilledApp,
    cli::{self, ExitCodeKind},
    operations::{
        ReceiptOperation, RepairDisposition, RepairPrompt, RepairStatus, RepairStepOutcome,
        plan_repair, probe_repair, verify_repair,
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
    fs::write(
        specific.join(".agents/skills/portable/SKILL.md"),
        "---\nname: portable\ndescription: dirty repair target\n---\n# portable\n",
    )
    .expect("dirty the selected repair source");
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
    assert!(
        plan.warnings().iter().any(|warning| warning.contains(
            "after this repair, OpenCode would have more than one directory to choose between"
        )),
        "the consent surface must state the conflict this Codex repair would create: {plan:?}"
    );
    assert!(
        plan.warnings()
            .iter()
            .any(|warning| warning.contains("uncommitted changes")),
        "repair should carry the same dirty-checkout warning as install: {plan:?}"
    );
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
    assert!(
        preview.contains("OpenCode would have more than one directory to choose between"),
        "{preview}"
    );
    assert!(
        preview.contains("OpenCode after repair: conflict"),
        "{preview}"
    );
    assert!(preview.contains("uncommitted changes"), "{preview}");

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
    assert!(
        output.contains("OpenCode would have more than one directory to choose between"),
        "{output}"
    );
    assert!(
        output.contains("OpenCode after repair: conflict"),
        "{output}"
    );
    assert!(output.contains("uncommitted changes"), "{output}");
}

#[test]
fn a_removed_old_link_is_a_partial_apply_exit() {
    assert_eq!(
        cli::exit_code_for_repair(RepairStatus::PartiallyApplied),
        ExitCodeKind::PartialApply
    );
}

#[test]
fn an_opencode_only_repair_withholds_compatibility_roots_without_failing() {
    let fixture = Fixture::new();
    let common = fixture.source("common", "skills", "portable");
    let mut app = fixture.registered_with(&common, [false, false, true]);
    fixture.create_root_parents();
    fixture.install(&mut app);
    let specific = fixture.source("opencode-specific", ".config/opencode/skills", "portable");
    let preview = app
        .preview_source(&specific)
        .expect("preview OpenCode source");
    app.confirm_source(preview)
        .expect("register OpenCode source");

    dispatch(&mut app, Action::OpenDoctor);
    dispatch(&mut app, Action::BeginRepair);
    assert!(matches!(
        app.pending_repair(),
        Some(RepairPrompt::Preview(plan)) if plan.agent() == AgentKind::OpenCode
    ));
    app.note_detail_max_scroll(Some(0));
    dispatch(&mut app, Action::ConfirmRepair);

    let Some(RepairPrompt::Report(outcome)) = app.pending_repair() else {
        panic!("repair should report its result");
    };
    assert_eq!(outcome.status(), RepairStatus::Repaired);
    assert!(outcome.verification().is_verified());
    assert!(!outcome.verification().is_complete());
    assert_eq!(outcome.verification().withheld().len(), 1);
    assert_eq!(
        outcome.verification().withheld()[0].agent(),
        AgentKind::OpenCode
    );
}

#[test]
fn a_codex_repair_with_opencode_deselected_withholds_the_ancillary_check() {
    let fixture = Fixture::new();
    let common = fixture.source("common", "skills", "portable");
    let mut app = fixture.registered_with(&common, [false, true, false]);
    fixture.create_root_parents();
    fixture.install(&mut app);
    let specific = fixture.source("codex-specific", ".agents/skills", "portable");
    let preview = app.preview_source(&specific).expect("preview Codex source");
    app.confirm_source(preview).expect("register Codex source");

    dispatch(&mut app, Action::OpenDoctor);
    dispatch(&mut app, Action::BeginRepair);
    assert!(matches!(
        app.pending_repair(),
        Some(RepairPrompt::Preview(plan)) if plan.agent() == AgentKind::Codex
    ));
    app.note_detail_max_scroll(Some(0));
    dispatch(&mut app, Action::ConfirmRepair);

    let Some(RepairPrompt::Report(outcome)) = app.pending_repair() else {
        panic!("repair should report its result");
    };
    assert_eq!(outcome.status(), RepairStatus::Repaired);
    assert!(outcome.verification().is_verified());
    assert!(!outcome.verification().is_complete());
    assert_eq!(outcome.verification().withheld().len(), 1);
    assert_eq!(
        outcome.verification().withheld()[0].agent(),
        AgentKind::OpenCode
    );
}

#[test]
fn inventory_details_render_every_owned_incorrect_link_finding_by_agent() {
    let fixture = Fixture::new();
    let common = fixture.source("common", "skills", "portable");
    let mut app = fixture.registered(&common);
    fixture.create_root_parents();
    fixture.install(&mut app);
    for (repository, catalog) in [
        ("claude-specific", ".claude/skills"),
        ("codex-specific", ".agents/skills"),
    ] {
        let specific = fixture.source(repository, catalog, "portable");
        let preview = app
            .preview_source(&specific)
            .expect("preview specific source");
        app.confirm_source(preview)
            .expect("register specific source");
    }

    dispatch(&mut app, Action::OpenInventory);
    dispatch(&mut app, Action::AdvanceInventoryPane);
    let detail = render_text(&app, 120, 80);

    assert_eq!(
        detail.matches("install.wrong_managed_target").count(),
        2,
        "each incorrect link must render beside its own agent observation:\n{detail}"
    );
    let claude = detail.find("CLAUDE CODE").expect("Claude Code section");
    let codex = claude
        + detail[claude..]
            .find("CODEX  ")
            .expect("Codex section after Claude Code");
    let first_finding = detail
        .find("install.wrong_managed_target")
        .expect("first overlay finding");
    let second_finding = detail
        .rfind("install.wrong_managed_target")
        .expect("second overlay finding");
    assert!(claude < first_finding && first_finding < codex, "{detail}");
    assert!(codex < second_finding, "{detail}");
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
fn a_cli_guard_refusal_does_not_claim_the_repair_was_verified() {
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

    let moved_again = fixture.directory.path().join("moved-again");
    let source_to_move = moved.clone();
    let mut input = MutatingInput::new(b"yes\n", move || {
        fs::rename(source_to_move, moved_again).unwrap();
    });
    let mut output = Vec::new();

    let code = cli::run(
        &[
            "repair".to_owned(),
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

    assert_eq!(code, ExitCodeKind::Blocked, "{output}");
    assert!(output.contains("not written"), "{output}");
    assert!(
        output.contains("there was no repaired link to verify"),
        "{output}"
    );
    assert!(!output.contains("Verified:"), "{output}");
    assert!(!output.contains("Verified as far"), "{output}");
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
    app.note_detail_max_scroll(None);
    dispatch(&mut app, Action::ConfirmRepair);
    assert!(matches!(
        app.pending_repair(),
        Some(RepairPrompt::Preview(_))
    ));
    assert_eq!(
        fs::read_link(&link).unwrap(),
        old_target,
        "a frame that did not draw the preview invalidates an older measurement"
    );

    app.note_detail_max_scroll(Some(0));
    dispatch(&mut app, Action::ConfirmRepair);

    let Some(RepairPrompt::Report(outcome)) = app.pending_repair() else {
        panic!("repair should show a report: {:?}", app.pending_repair());
    };
    let verified_plan = outcome.plan().clone();
    let verified_apply = outcome.applied().clone();
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

    // Canonical resolution alone is insufficient: a differently spelled raw
    // target would no longer match the receipt the repair just recorded.
    dispatch(&mut app, Action::DismissRepair);
    let alias = fixture.directory.path().join("alias-to-repaired-variant");
    std::os::unix::fs::symlink(verified_plan.new_target().unwrap(), &alias).unwrap();
    fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(&alias, &link).unwrap();
    dispatch(&mut app, Action::OpenInventory);
    let verification = verify_repair(&verified_plan, &verified_apply, app.inventory());
    assert!(!verification.is_verified());
    assert!(
        verification.failures()[0]
            .observed()
            .contains("instead of the planned target"),
        "{:?}",
        verification.failures()
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
    assert!(
        !outcome.verification().is_verified(),
        "a refused apply did not observe a repaired link"
    );
    assert!(
        !outcome.verification().is_complete(),
        "a refused apply has no completed repair postcondition"
    );
    let report = render_text(&app, 120, 60);
    assert!(!report.contains("Repaired and verified"), "{report}");
    assert!(
        report.contains("there was no repaired link to verify"),
        "{report}"
    );
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

struct MutatingInput {
    input: Cursor<Vec<u8>>,
    mutation: Option<Box<dyn FnOnce()>>,
}

impl MutatingInput {
    fn new(input: &[u8], mutation: impl FnOnce() + 'static) -> Self {
        Self {
            input: Cursor::new(input.to_vec()),
            mutation: Some(Box::new(mutation)),
        }
    }

    fn mutate_once(&mut self) {
        if let Some(mutation) = self.mutation.take() {
            mutation();
        }
    }
}

impl Read for MutatingInput {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.mutate_once();
        self.input.read(buffer)
    }
}

impl BufRead for MutatingInput {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.mutate_once();
        self.input.fill_buf()
    }

    fn consume(&mut self, amount: usize) {
        self.input.consume(amount);
    }
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
        self.registered_with(repository, [true; 3])
    }
    fn registered_with(&self, repository: &Path, selections: [bool; 3]) -> SkilledApp {
        let mut app = self.app();
        let preview = app.preview_source(repository).unwrap();
        app.confirm_source(preview).unwrap();
        dispatch(&mut app, Action::Continue);
        for (index, selected) in selections.into_iter().enumerate() {
            if !selected {
                for _ in 0..index {
                    dispatch(&mut app, Action::MoveSelection(1));
                }
                dispatch(&mut app, Action::ToggleSelection);
                for _ in 0..index {
                    dispatch(&mut app, Action::MoveSelection(-1));
                }
            }
        }
        for _ in 0..6 {
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
