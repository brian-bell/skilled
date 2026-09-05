//! Repair planning over real temporary homes and registered Git sources.
#![cfg(unix)]

use std::{
    fs,
    io::{self, BufRead, Cursor, Read},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

use ratatui::{Terminal, backend::TestBackend};

use skilled::{
    Action, AgentKind, AppEnvironment, SkilledApp,
    cli::{self, ExitCodeKind},
    operations::{
        ReceiptOperation, RepairDisposition, RepairPrompt, RepairStatus, RepairStepOutcome,
        VerifyReport, plan_repair, probe_repair, verify_repair,
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
fn degraded_doctor_withholds_a_repair_offer_built_from_retained_receipts() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", "skills", "portable");
    let mut app = fixture.registered(&repository);
    fixture.create_root_parents();
    fixture.install(&mut app);
    let moved = fixture.directory.path().join("moved-library");
    fs::rename(&repository, &moved).expect("move checkout");
    let preview = app
        .preview_source(&moved)
        .expect("preview relocated source");
    app.confirm_source(preview)
        .expect("register relocated source");
    drop(app);
    let database = fixture.directory.path().join("data/skilled.sqlite3");
    let connection = rusqlite::Connection::open(database).expect("open metadata database");
    connection
        .execute(
            "UPDATE settings SET value = 'sometimes' WHERE key = 'setup_complete'",
            [],
        )
        .expect("malform setup metadata");
    drop(connection);

    let mut app = fixture.app();
    dispatch(&mut app, Action::OpenDoctor);
    let findings = app.doctor_findings();

    assert!(app.metadata_failure().is_some());
    assert!(
        findings.iter().all(|entry| !app.can_repair_finding(entry)),
        "degraded Doctor offered repair for {:?}",
        findings
            .iter()
            .filter(|entry| app.can_repair_finding(entry))
            .map(|entry| entry.finding().code())
            .collect::<Vec<_>>()
    );
    assert!(!app.can_repair_selection());
}

#[test]
fn the_tui_refreshes_registered_sources_before_planning_a_repair() {
    let fixture = Fixture::new();
    let common = fixture.source("common", "skills", "portable");
    let mut app = fixture.registered(&common);
    fixture.create_root_parents();
    fixture.install(&mut app);

    let selected = fixture.source("selected", ".agents/skills", "portable");
    let preview = app
        .preview_source(&selected)
        .expect("preview selected source");
    app.confirm_source(preview)
        .expect("register selected source");
    let competitor = fixture.source("competitor", ".agents/skills", "other");
    let preview = app
        .preview_source(&competitor)
        .expect("preview competing source");
    app.confirm_source(preview)
        .expect("register competing source");

    dispatch(&mut app, Action::OpenDoctor);
    let finding = app
        .doctor_findings()
        .iter()
        .position(|entry| {
            entry.agent() == Some(AgentKind::Codex)
                && entry.finding().code() == "install.wrong_managed_target"
        })
        .expect("Codex repairable finding");
    dispatch(
        &mut app,
        Action::MoveDoctorSelection(i8::try_from(finding).unwrap()),
    );
    assert!(app.can_repair_selection());

    // This candidate appeared after the source was registered. The Doctor
    // view still holds its earlier source snapshot when the repair key is
    // pressed, so planning has to refresh the registry before it chooses.
    write_skill(&competitor.join(".agents/skills/portable"), "portable");
    dispatch(&mut app, Action::BeginRepair);

    let Some(RepairPrompt::Preview(plan)) = app.pending_repair() else {
        panic!(
            "repair should show the refreshed plan: {:?}",
            app.pending_repair()
        );
    };
    assert!(!plan.is_executable(), "{plan:?}");
    assert_eq!(
        plan.blocking_finding().map(|finding| finding.code()),
        Some("variant.duplicate_for_agent")
    );
}

#[test]
fn a_removed_old_link_is_a_partial_apply_exit() {
    assert_eq!(
        cli::exit_code_for_repair(RepairStatus::PartiallyApplied, &VerifyReport::default()),
        ExitCodeKind::PartialApply
    );
}

#[test]
fn repair_refuses_a_target_root_that_cannot_be_enumerated() {
    let fixture = Fixture::new();
    let common = fixture.source("common", "skills", "portable");
    let mut app = fixture.registered(&common);
    fixture.create_root_parents();
    fixture.install(&mut app);
    let specific = fixture.source("specific", ".agents/skills", "portable");
    let preview = app
        .preview_source(&specific)
        .expect("preview specific source");
    app.confirm_source(preview)
        .expect("register specific source");
    let link = fixture.root(AgentKind::Codex).join("portable");
    let old_target = fs::read_link(&link).unwrap();
    drop(app);

    let root = fixture.root(AgentKind::Codex);
    fs::set_permissions(&root, fs::Permissions::from_mode(0o300)).expect("seal root listing");
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
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).expect("restore root");
    let output = String::from_utf8(output).unwrap();

    assert_eq!(code, ExitCodeKind::Blocked, "{output}");
    assert!(output.contains("install.unreadable_root"), "{output}");
    assert_eq!(fs::read_link(&link).unwrap(), old_target);
}

/// A repaired link whose ancillary OpenCode check was withheld because a root
/// could not be read exits as an incomplete verification, not a plain
/// success: a script reads only the status (skilled-exm). The deselected-root
/// withholdings in the two tests below stay exit zero — the user's own
/// selection is what precluded those checks.
#[test]
fn an_unreadable_root_makes_a_verified_repair_exit_as_incomplete() {
    if running_as_root() {
        // Permission bits decide nothing for the superuser, so the root this
        // case needs to be unreadable would be read.
        return;
    }
    let fixture = Fixture::new();
    let common = fixture.source("common", "skills", "portable");
    let mut app = fixture.registered(&common);
    fixture.create_root_parents();
    fixture.install(&mut app);
    let specific = fixture.source("specific", ".agents/skills", "portable");
    let preview = app
        .preview_source(&specific)
        .expect("preview specific source");
    app.confirm_source(preview)
        .expect("register specific source");
    drop(app);
    // Claude Code's root is one OpenCode reads too, so sealing it leaves what
    // OpenCode resolves the name to unestablishable while the repaired Codex
    // link itself is re-observed.
    let claude_root = fixture.root(AgentKind::ClaudeCode);
    fs::set_permissions(&claude_root, fs::Permissions::from_mode(0o000))
        .expect("seal Claude Code root");

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
    fs::set_permissions(&claude_root, fs::Permissions::from_mode(0o755))
        .expect("unseal Claude Code root");
    let output = String::from_utf8(output).unwrap();

    assert_eq!(code, ExitCodeKind::VerificationIncomplete, "{output}");
    // The link itself was repaired and re-observed: the third answer, not a
    // failure.
    assert_eq!(
        fs::read_link(fixture.root(AgentKind::Codex).join("portable")).unwrap(),
        specific
            .join(".agents/skills/portable")
            .canonicalize()
            .unwrap()
    );
    assert!(
        output.contains("Verified as far as it could be"),
        "{output}"
    );
    assert!(output.contains("Not established"), "{output}");
}

/// The same masking one level down: a selected root the scan *did* read can
/// hold an entry under the repaired name whose resolution could not be
/// established, and a deselected OpenCode must not turn that gap into a plain
/// success either.
#[test]
fn a_deselected_opencode_does_not_mask_an_unresolvable_entry_in_a_selected_root() {
    let fixture = Fixture::new();
    let common = fixture.source("common", "skills", "portable");
    let mut app = fixture.registered_with(&common, [true, true, false]);
    fixture.create_root_parents();
    fixture.install(&mut app);
    let specific = fixture.source("codex-specific", ".agents/skills", "portable");
    let preview = app.preview_source(&specific).expect("preview Codex source");
    app.confirm_source(preview).expect("register Codex source");
    drop(app);
    // A symbolic link that resolves through itself: the Claude Code root reads
    // fine, but what this entry holds under the name cannot be followed.
    let claude_root = fixture.root(AgentKind::ClaudeCode);
    let looped = claude_root.join("portable");
    fs::remove_file(&looped).expect("remove the installed Claude Code link");
    std::os::unix::fs::symlink("portable", &looped).expect("create a self-referencing link");

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

    assert_eq!(code, ExitCodeKind::VerificationIncomplete, "{output}");
    assert!(output.contains("Not established"), "{output}");
}

/// Deselecting OpenCode must not mask a selected root that could not be read:
/// the withheld OpenCode check is then not precluded by selection alone, and
/// the exit status still owes the incomplete answer.
#[test]
fn a_deselected_opencode_does_not_mask_an_unreadable_selected_root() {
    if running_as_root() {
        return;
    }
    let fixture = Fixture::new();
    let common = fixture.source("common", "skills", "portable");
    let mut app = fixture.registered_with(&common, [true, true, false]);
    fixture.create_root_parents();
    fixture.install(&mut app);
    let specific = fixture.source("codex-specific", ".agents/skills", "portable");
    let preview = app.preview_source(&specific).expect("preview Codex source");
    app.confirm_source(preview).expect("register Codex source");
    drop(app);
    let claude_root = fixture.root(AgentKind::ClaudeCode);
    fs::create_dir_all(&claude_root).expect("create Claude Code root");
    fs::set_permissions(&claude_root, fs::Permissions::from_mode(0o000))
        .expect("seal Claude Code root");

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
    fs::set_permissions(&claude_root, fs::Permissions::from_mode(0o755))
        .expect("unseal Claude Code root");
    let output = String::from_utf8(output).unwrap();

    assert_eq!(code, ExitCodeKind::VerificationIncomplete, "{output}");
    assert!(output.contains("Not established"), "{output}");
}

/// Probed once and cached: tests in one binary run concurrently, and two
/// probes sharing a process-id-derived path would race each other's create,
/// chmod, and remove.
fn running_as_root() -> bool {
    static ROOT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ROOT.get_or_init(|| {
        let probe = tempfile::tempdir().unwrap();
        let sealed = probe.path().join("sealed");
        fs::create_dir(&sealed).unwrap();
        fs::set_permissions(&sealed, fs::Permissions::from_mode(0o000)).unwrap();
        let readable = fs::read_dir(&sealed).is_ok();
        fs::set_permissions(&sealed, fs::Permissions::from_mode(0o755)).unwrap();
        readable
    })
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
    // The user's own selection is all that precluded the check, so the exit
    // status stays an ordinary success rather than an incomplete verification.
    assert!(outcome.verification().is_complete_for_selection());
    assert_eq!(
        cli::exit_code_for_repair(outcome.status(), outcome.verification()),
        ExitCodeKind::Success
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
    // Deselecting OpenCode is what withheld its resolution, so the exit status
    // stays an ordinary success rather than an incomplete verification.
    assert!(outcome.verification().is_complete_for_selection());
    assert_eq!(
        cli::exit_code_for_repair(outcome.status(), outcome.verification()),
        ExitCodeKind::Success
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
fn a_non_target_root_resolution_error_withholds_the_opencode_prediction() {
    let fixture = Fixture::new();
    let common = fixture.source("common", "skills", "portable");
    let mut app = fixture.registered(&common);
    fixture.create_root_parents();
    fixture.install(&mut app);
    let specific = fixture.source("codex-specific", ".agents/skills", "portable");
    let preview = app.preview_source(&specific).expect("preview Codex source");
    app.confirm_source(preview).expect("register Codex source");
    let claude_link = fixture.root(AgentKind::ClaudeCode).join("portable");
    fs::remove_file(&claude_link).unwrap();
    std::os::unix::fs::symlink(&claude_link, &claude_link).unwrap();

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

    assert!(plan.is_executable(), "{plan:?}");
    assert!(
        plan.warnings()
            .iter()
            .any(|warning| warning.contains("cannot be established")),
        "an ELOOP in a root OpenCode reads is unknown, not absent: {plan:?}"
    );
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

/// Forget Source proves every one of a source's receipted links inactive and
/// then deletes its metadata, all inside one mutation guard. A repair makes a
/// link active and records a receipt naming that same metadata, so it has to
/// take the guard too and recheck the registration under it. Without that,
/// this repair lands between Forget's liveness probe and its deletion, and
/// leaves an active link pointing into a source Skilled no longer knows.
#[test]
fn a_repair_whose_source_is_forgotten_after_the_preview_writes_nothing() {
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
    let Some(RepairPrompt::Preview(plan)) = app.pending_repair() else {
        panic!("repair preview expected: {:?}", app.pending_repair());
    };
    assert!(plan.is_executable());
    let replacement_id = plan.variant().expect("a repair source").source_id();

    // A second process forgets the relocated source the repair would point
    // this link at. It holds no active links of its own, so nothing blocks it.
    let mut forgetting = fixture.app();
    dispatch(&mut forgetting, Action::OpenSources);
    let forgotten = forgetting
        .sources()
        .iter()
        .position(|source| source.id() == replacement_id)
        .expect("the relocated source is registered");
    for _ in 0..forgotten {
        dispatch(&mut forgetting, Action::MoveSourcesSelection(1));
    }
    dispatch(&mut forgetting, Action::BeginForgetSource);
    forgetting.note_detail_max_scroll(Some(0));
    dispatch(&mut forgetting, Action::ConfirmOperation);
    assert!(
        forgetting
            .sources()
            .iter()
            .all(|source| source.id() != replacement_id),
        "the second process must have forgotten the repair's source"
    );

    app.note_detail_max_scroll(Some(0));
    dispatch(&mut app, Action::ConfirmRepair);

    let Some(RepairPrompt::Report(outcome)) = app.pending_repair() else {
        panic!("repair should report its guarded refusal");
    };
    assert_eq!(outcome.status(), RepairStatus::NotApplied);
    assert!(
        matches!(
            outcome.applied().step().map(|step| step.outcome()),
            Some(RepairStepOutcome::Failed(reason)) if reason.contains("forgotten")
        ),
        "{:?}",
        outcome.applied().step().map(|step| step.outcome())
    );
    assert_eq!(fs::read_link(&link).unwrap(), old_target);
    assert_no_repair_temporary(&fixture.root(AgentKind::Codex));
    assert!(
        app.receipts()
            .unwrap()
            .iter()
            .all(|receipt| receipt.operation() != ReceiptOperation::Repair),
        "no repair receipt may name a forgotten source"
    );
}

/// The plan's own registration surviving is not the selection surviving. A
/// competing agent-specific source registered after the preview makes the
/// replacement variant one of two candidates, which a fresh selection reports
/// as a spec 6.4 duplicate conflict rather than resolving — so the guard
/// compares the whole registry the plan chose from (skilled-g64).
#[test]
fn a_repair_whose_selection_became_a_duplicate_after_the_preview_writes_nothing() {
    let fixture = Fixture::new();
    let common = fixture.source("common", "skills", "portable");
    let mut app = fixture.registered(&common);
    fixture.create_root_parents();
    fixture.install(&mut app);
    let specific = fixture.source("specific", ".agents/skills", "portable");
    let preview = app
        .preview_source(&specific)
        .expect("preview specific source");
    app.confirm_source(preview)
        .expect("register specific source");
    let link = fixture.root(AgentKind::Codex).join("portable");
    let old_target = fs::read_link(&link).unwrap();

    dispatch(&mut app, Action::OpenDoctor);
    let finding = app
        .doctor_findings()
        .iter()
        .position(|entry| {
            entry.agent() == Some(AgentKind::Codex)
                && entry.finding().code() == "install.wrong_managed_target"
        })
        .expect("Codex repairable finding");
    dispatch(
        &mut app,
        Action::MoveDoctorSelection(i8::try_from(finding).unwrap()),
    );
    dispatch(&mut app, Action::BeginRepair);
    assert!(matches!(
        app.pending_repair(),
        Some(RepairPrompt::Preview(plan)) if plan.is_executable()
    ));

    // A second process registers another Codex edition of the same name, so
    // two agent-specific variants now answer to it.
    let rival = fixture.source("rival", ".agents/skills", "portable");
    let mut registering = fixture.app();
    let preview = registering.preview_source(&rival).expect("preview rival");
    registering
        .confirm_source(preview)
        .expect("register the competing source");
    assert_eq!(registering.sources().len(), 3);

    app.note_detail_max_scroll(Some(0));
    dispatch(&mut app, Action::ConfirmRepair);

    let Some(RepairPrompt::Report(outcome)) = app.pending_repair() else {
        panic!("repair should report its guarded refusal");
    };
    assert_eq!(outcome.status(), RepairStatus::NotApplied);
    assert!(
        matches!(
            outcome.applied().step().map(|step| step.outcome()),
            Some(RepairStepOutcome::Failed(reason))
                if reason.contains("registered sources changed")
        ),
        "{:?}",
        outcome.applied().step().map(|step| step.outcome())
    );
    assert_eq!(fs::read_link(&link).unwrap(), old_target);
    assert_no_repair_temporary(&fixture.root(AgentKind::Codex));
    assert!(
        app.receipts()
            .unwrap()
            .iter()
            .all(|receipt| receipt.operation() != ReceiptOperation::Repair),
        "no repair receipt may name a selection the registry no longer makes"
    );
    let rendered = render_text(&app, 100, 30);
    assert!(rendered.contains("nothing written"), "{rendered}");
    assert!(!rendered.contains("already applied"), "{rendered}");
    insta::assert_snapshot!(
        "refused_repair_heading",
        rendered
            .lines()
            .find(|line| line.contains("Repair result"))
            .unwrap()
    );
}

#[test]
fn a_root_that_becomes_unreadable_after_preview_is_refused_without_modifying_the_link() {
    let fixture = Fixture::new();
    let common = fixture.source("common", "skills", "portable");
    let mut app = fixture.registered(&common);
    fixture.create_root_parents();
    fixture.install(&mut app);
    let specific = fixture.source("specific", ".agents/skills", "portable");
    let preview = app
        .preview_source(&specific)
        .expect("preview specific source");
    app.confirm_source(preview)
        .expect("register specific source");
    let link = fixture.root(AgentKind::Codex).join("portable");
    let old_target = fs::read_link(&link).unwrap();

    dispatch(&mut app, Action::OpenDoctor);
    let finding = app
        .doctor_findings()
        .iter()
        .position(|entry| {
            entry.agent() == Some(AgentKind::Codex)
                && entry.finding().code() == "install.wrong_managed_target"
        })
        .expect("Codex repairable finding");
    dispatch(
        &mut app,
        Action::MoveDoctorSelection(i8::try_from(finding).unwrap()),
    );
    dispatch(&mut app, Action::BeginRepair);
    assert!(matches!(
        app.pending_repair(),
        Some(RepairPrompt::Preview(plan)) if plan.is_executable()
    ));

    let root = fixture.root(AgentKind::Codex);
    fs::set_permissions(&root, fs::Permissions::from_mode(0o300)).expect("seal root listing");
    app.note_detail_max_scroll(Some(0));
    dispatch(&mut app, Action::ConfirmRepair);
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).expect("restore root");

    let Some(RepairPrompt::Report(outcome)) = app.pending_repair() else {
        panic!("repair should report its guarded refusal");
    };
    assert_eq!(outcome.status(), RepairStatus::NotApplied);
    assert!(matches!(
        outcome.applied().step().map(|step| step.outcome()),
        Some(RepairStepOutcome::Failed(reason))
            if reason.contains("skill root changed after the plan was shown")
    ));
    assert_eq!(fs::read_link(&link).unwrap(), old_target);
}

#[test]
fn a_link_that_becomes_unresolvable_after_preview_is_refused_without_replacement() {
    let fixture = Fixture::new();
    let common = fixture.source("common", "skills", "portable");
    let mut app = fixture.registered(&common);
    fixture.create_root_parents();
    fixture.install(&mut app);
    let specific = fixture.source("specific", ".agents/skills", "portable");
    let preview = app
        .preview_source(&specific)
        .expect("preview specific source");
    app.confirm_source(preview)
        .expect("register specific source");
    let link = fixture.root(AgentKind::Codex).join("portable");
    let old_target = fs::read_link(&link).unwrap();

    dispatch(&mut app, Action::OpenDoctor);
    let finding = app
        .doctor_findings()
        .iter()
        .position(|entry| {
            entry.agent() == Some(AgentKind::Codex)
                && entry.finding().code() == "install.wrong_managed_target"
        })
        .expect("Codex repairable finding");
    dispatch(
        &mut app,
        Action::MoveDoctorSelection(i8::try_from(finding).unwrap()),
    );
    dispatch(&mut app, Action::BeginRepair);
    assert!(matches!(
        app.pending_repair(),
        Some(RepairPrompt::Preview(plan))
            if plan.disposition() == &RepairDisposition::ReplaceLink { dangling: false }
    ));

    // The installed link keeps the exact raw target the receipt proves, while
    // that target changes from a resolvable directory into an ELOOP. A fresh
    // plan would refuse this state as unresolvable, so confirmation must too.
    fs::remove_dir_all(&old_target).unwrap();
    std::os::unix::fs::symlink(&old_target, &old_target).unwrap();
    app.note_detail_max_scroll(Some(0));
    dispatch(&mut app, Action::ConfirmRepair);

    let Some(RepairPrompt::Report(outcome)) = app.pending_repair() else {
        panic!("repair should report its guarded refusal");
    };
    assert_eq!(outcome.status(), RepairStatus::NotApplied);
    assert!(matches!(
        outcome.applied().step().map(|step| step.outcome()),
        Some(RepairStepOutcome::Failed(reason))
            if reason.contains("entry or its resolution changed")
    ));
    assert_eq!(fs::read_link(&link).unwrap(), old_target);
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
        dispatch(app, Action::ConfirmOperation);
        dispatch(app, Action::DismissOperation);
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
