//! Read-only inventory of the documented native agent skill roots.
//!
//! Every fixture builds its own temporary home; no test may read the real user
//! home or a real agent skill root. Symbolic links are the primary managed
//! installation shape, so the whole suite is Unix-only.
#![cfg(unix)]

use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::Command,
};

use skilled::{
    AgentKind, AppEnvironment, SkilledApp,
    inventory::{
        Finding, FindingSeverity, InstallationHealth, InstallationObject, Provenance, RootStatus,
        RowProvenance,
    },
};

/// The exact global roots documented by each agent adapter.
///
/// Spelled out literally rather than derived from the adapters, so a change to
/// a documented path has to be made deliberately in both places.
const CLAUDE_CODE_ROOT: &str = ".claude/skills";
const CODEX_ROOT: &str = ".agents/skills";
const OPENCODE_ROOT: &str = ".config/opencode/skills";

#[test]
fn a_symlink_to_a_registered_variant_resolves_to_its_source_catalog_and_variant() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    let app = fixture.registered(&repository);
    drop(app);
    fixture.install_symlink(
        AgentKind::ClaudeCode,
        "portable",
        &repository.join("skills/portable"),
    );

    let app = fixture.app();
    let inventory = app.inventory();

    let row = inventory.row("portable").expect("portable row");
    assert_eq!(row.health(), InstallationHealth::Healthy);
    assert_eq!(row.provenance(), RowProvenance::Source("library"));
    let observation = row
        .observation(AgentKind::ClaudeCode)
        .expect("Claude Code observation");
    assert_eq!(
        observation.path(),
        fixture.home().join(CLAUDE_CODE_ROOT).join("portable")
    );
    assert!(matches!(
        observation.object(),
        InstallationObject::Symlink { .. }
    ));
    assert!(observation.findings().is_empty());
    let resolution = observation.resolution().expect("resolved variant");
    assert_eq!(resolution.source_label(), "library");
    assert_eq!(resolution.catalog_relative_path(), Path::new("skills"));
    assert_eq!(
        resolution.variant_relative_path(),
        Path::new("skills/portable")
    );

    // The other two agents have no such installation, and say so by absence.
    assert!(row.observation(AgentKind::Codex).is_none());
    assert!(row.observation(AgentKind::OpenCode).is_none());
}

#[test]
fn every_documented_root_is_observed_independently_at_its_exact_path() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["alpha", "beta", "gamma"]);
    drop(fixture.registered(&repository));
    for (agent, skill) in [
        (AgentKind::ClaudeCode, "alpha"),
        (AgentKind::Codex, "beta"),
        (AgentKind::OpenCode, "gamma"),
    ] {
        fixture.install_symlink(agent, skill, &repository.join("skills").join(skill));
    }

    let app = fixture.app();
    let inventory = app.inventory();

    for (agent, skill, root) in [
        (AgentKind::ClaudeCode, "alpha", CLAUDE_CODE_ROOT),
        (AgentKind::Codex, "beta", CODEX_ROOT),
        (AgentKind::OpenCode, "gamma", OPENCODE_ROOT),
    ] {
        let row = inventory.row(skill).expect("installed row");
        assert_eq!(row.health(), InstallationHealth::Healthy);
        let observation = row.observation(agent).expect("observation for its agent");
        assert_eq!(observation.path(), fixture.home().join(root).join(skill));
        for other in AgentKind::ALL.into_iter().filter(|kind| *kind != agent) {
            assert!(
                row.observation(other).is_none(),
                "{skill} leaked into {other:?}"
            );
        }
        assert_eq!(
            inventory.root(agent).status(),
            &RootStatus::Scanned { installed: 1 }
        );
    }
}

#[test]
fn one_row_per_name_merges_the_agents_that_share_it() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["shared"]);
    drop(fixture.registered(&repository));
    let variant = repository.join("skills/shared");
    fixture.install_symlink(AgentKind::ClaudeCode, "shared", &variant);
    fixture.install_symlink(AgentKind::Codex, "shared", &variant);

    let app = fixture.app();
    let inventory = app.inventory();

    assert_eq!(inventory.rows().len(), 1);
    let row = &inventory.rows()[0];
    assert!(row.observation(AgentKind::ClaudeCode).is_some());
    assert!(row.observation(AgentKind::Codex).is_some());
    assert!(row.observation(AgentKind::OpenCode).is_none());
    assert_eq!(inventory.installation_count(), 2);
}

#[test]
fn rows_are_ordered_by_name_regardless_of_directory_order() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["alpha", "middle", "zulu"]);
    drop(fixture.registered(&repository));
    for skill in ["zulu", "alpha", "middle"] {
        fixture.install_symlink(
            AgentKind::Codex,
            skill,
            &repository.join("skills").join(skill),
        );
    }

    let names: Vec<_> = fixture
        .app()
        .inventory()
        .rows()
        .iter()
        .map(|row| row.name().to_owned())
        .collect();

    assert_eq!(names, ["alpha", "middle", "zulu"]);
}

#[test]
fn a_dangling_symlink_is_broken_and_reports_a_critical_finding() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    drop(fixture.registered(&repository));
    fixture.install_symlink(
        AgentKind::ClaudeCode,
        "vanished",
        &fixture.path().join("nowhere"),
    );

    let app = fixture.app();
    let row = app.inventory().row("vanished").expect("dangling row");

    assert_eq!(row.health(), InstallationHealth::Broken);
    assert_eq!(row.provenance(), RowProvenance::Unregistered);
    let observation = row
        .observation(AgentKind::ClaudeCode)
        .expect("dangling observation");
    assert!(matches!(
        observation.object(),
        InstallationObject::Symlink { .. }
    ));
    let codes: Vec<_> = observation
        .findings()
        .iter()
        .map(|finding| finding.code())
        .collect();
    assert_eq!(codes, ["install.dangling_symlink"]);
    assert_eq!(
        observation.findings()[0].severity(),
        FindingSeverity::Critical
    );
}

#[test]
fn a_symlink_to_a_valid_directory_outside_every_registered_source_is_unmanaged() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    drop(fixture.registered(&repository));
    let foreign = fixture.path().join("elsewhere/foreign");
    write_skill(&foreign, "foreign");
    fixture.install_symlink(AgentKind::ClaudeCode, "foreign", &foreign);

    let app = fixture.app();
    let row = app.inventory().row("foreign").expect("foreign row");

    assert_eq!(row.health(), InstallationHealth::Unmanaged);
    assert_eq!(row.provenance(), RowProvenance::Unregistered);
    let observation = row
        .observation(AgentKind::ClaudeCode)
        .expect("foreign observation");
    assert!(observation.resolution().is_none());
    assert_eq!(observation.findings()[0].code(), "install.unmanaged");
    assert_eq!(observation.findings()[0].severity(), FindingSeverity::Info);
}

#[test]
fn a_symlink_into_a_registered_source_at_a_non_candidate_path_is_never_claimed_as_managed() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    // A valid skill inside the checkout but outside any confirmed catalog.
    let stray = repository.join("docs/examples/stray");
    write_skill(&stray, "stray");
    drop(fixture.registered(&repository));
    fixture.install_symlink(AgentKind::ClaudeCode, "stray", &stray);

    let app = fixture.app();
    let row = app.inventory().row("stray").expect("stray row");

    assert_eq!(row.health(), InstallationHealth::Unmanaged);
    assert_eq!(row.provenance(), RowProvenance::Unregistered);
    assert!(
        row.observation(AgentKind::ClaudeCode)
            .expect("stray observation")
            .resolution()
            .is_none()
    );
}

#[test]
fn a_valid_physical_directory_is_unmanaged_and_an_invalid_one_is_broken() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    drop(fixture.registered(&repository));
    let root = fixture.create_root(AgentKind::Codex);
    write_skill(&root.join("copied"), "copied");
    fs::create_dir_all(root.join("malformed")).expect("create malformed skill");
    fs::write(root.join("malformed/SKILL.md"), "no frontmatter here\n").expect("write malformed");

    let app = fixture.app();
    let inventory = app.inventory();

    let copied = inventory.row("copied").expect("copied row");
    assert_eq!(copied.health(), InstallationHealth::Unmanaged);
    let observation = copied
        .observation(AgentKind::Codex)
        .expect("copied observation");
    assert_eq!(observation.object(), &InstallationObject::Directory);
    assert_eq!(observation.findings()[0].code(), "install.unmanaged");

    let malformed = inventory.row("malformed").expect("malformed row");
    assert_eq!(malformed.health(), InstallationHealth::Broken);
    assert_eq!(
        malformed
            .observation(AgentKind::Codex)
            .expect("malformed observation")
            .findings()[0]
            .code(),
        "skill.invalid_frontmatter"
    );
}

#[test]
fn validation_compares_the_declared_name_against_the_installed_directory_name() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    drop(fixture.registered(&repository));
    // The link resolves to a registered variant, but the installed name differs
    // from the name the skill declares, so an agent would not load it.
    fixture.install_symlink(
        AgentKind::ClaudeCode,
        "renamed",
        &repository.join("skills/portable"),
    );

    let app = fixture.app();
    let row = app.inventory().row("renamed").expect("renamed row");

    assert_eq!(row.health(), InstallationHealth::Broken);
    let observation = row
        .observation(AgentKind::ClaudeCode)
        .expect("renamed observation");
    assert!(observation.resolution().is_some());
    assert_eq!(observation.findings()[0].code(), "skill.name_mismatch");
    let message = observation.findings()[0].evidence().to_owned();
    assert!(message.contains("renamed"), "{message}");
}

#[test]
fn a_plain_file_child_is_listed_without_being_read_as_a_broken_skill() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    drop(fixture.registered(&repository));
    let root = fixture.create_root(AgentKind::OpenCode);
    fs::write(root.join("notes.md"), "not a skill").expect("write plain file");
    fs::write(root.join(".DS_Store"), "platform litter").expect("write hidden file");

    let app = fixture.app();
    let inventory = app.inventory();
    let row = inventory.row("notes.md").expect("plain file row");

    // An agent ignores a stray file, so calling it broken would manufacture an
    // alarm out of a README or a platform artefact.
    assert_eq!(row.health(), InstallationHealth::NotASkill);
    let observation = row
        .observation(AgentKind::OpenCode)
        .expect("plain file observation");
    assert_eq!(observation.object(), &InstallationObject::NotADirectory);
    assert_eq!(observation.findings()[0].code(), "install.not_a_skill");
    assert_eq!(observation.findings()[0].severity(), FindingSeverity::Info);

    // Neither file is an installation, so neither inflates a count — and
    // neither is described as a skill.
    assert_eq!(inventory.installation_count(), 0);
    assert_eq!(inventory.broken_count(), 0);
    assert_eq!(inventory.unmanaged_count(), 0);
    assert_eq!(inventory.skill_row_count(), 0);
    assert_eq!(inventory.rows().len(), 2);
    assert_eq!(
        inventory.root(AgentKind::OpenCode).status(),
        &RootStatus::Scanned { installed: 0 }
    );
}

#[test]
fn a_name_installed_from_two_sources_names_neither_as_the_row_source() {
    let fixture = Fixture::new();
    let first = fixture.source("library", &["shared"]);
    let second = fixture.source("archive", &["shared"]);
    let mut app = fixture.registered(&first);
    let preview = app.preview_source(&second).expect("preview second source");
    app.confirm_source(preview).expect("register second source");
    drop(app);
    // The same name, installed for two agents, from two different registered
    // sources. Both are healthy; neither is the row's source.
    fixture.install_symlink(
        AgentKind::ClaudeCode,
        "shared",
        &first.join("skills/shared"),
    );
    fixture.install_symlink(AgentKind::Codex, "shared", &second.join("skills/shared"));

    let app = fixture.app();
    let row = app.inventory().row("shared").expect("shared row");

    assert_eq!(row.provenance(), RowProvenance::Divergent);
    assert_eq!(row.provenance().label(), "multiple sources");
    assert_eq!(row.health(), InstallationHealth::Healthy);
    // Each agent still names the source it actually carries.
    assert_eq!(
        row.observation(AgentKind::ClaudeCode)
            .and_then(|observation| observation.resolution())
            .map(|resolution| resolution.source_label()),
        Some("library")
    );
    assert_eq!(
        row.observation(AgentKind::Codex)
            .and_then(|observation| observation.resolution())
            .map(|resolution| resolution.source_label()),
        Some("archive")
    );
}

#[test]
fn a_name_managed_for_one_agent_and_unmanaged_for_another_is_mixed() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["shared"]);
    drop(fixture.registered(&repository));
    fixture.install_symlink(
        AgentKind::ClaudeCode,
        "shared",
        &repository.join("skills/shared"),
    );
    // The same name, carried for Codex as a plain directory that resolves to
    // no registered source.
    let codex_root = fixture.create_root(AgentKind::Codex);
    write_skill(&codex_root.join("shared"), "shared");

    let app = fixture.app();
    let row = app.inventory().row("shared").expect("shared row");

    // Naming the source would claim the unmanaged installation too, and the
    // label must not say "library" for a row the library only half explains.
    assert_eq!(row.provenance(), RowProvenance::Mixed);
    assert_eq!(row.provenance().label(), "mixed");
    assert_eq!(row.health(), InstallationHealth::Unmanaged);
    // Each observation still says what is known about its own provenance.
    assert_eq!(
        row.observation(AgentKind::ClaudeCode)
            .and_then(|observation| observation.resolution())
            .map(|resolution| resolution.source_label()),
        Some("library")
    );
    assert_eq!(
        row.observation(AgentKind::Codex)
            .map(|observation| observation.provenance()),
        Some(&Provenance::Unregistered)
    );
}

#[test]
fn a_stray_file_beside_a_managed_installation_does_not_make_the_row_mixed() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["shared"]);
    drop(fixture.registered(&repository));
    fixture.install_symlink(
        AgentKind::ClaudeCode,
        "shared",
        &repository.join("skills/shared"),
    );
    // A stray file is not an installation, so it carries no provenance the
    // row summary has to answer for.
    let codex_root = fixture.create_root(AgentKind::Codex);
    fs::write(codex_root.join("shared"), "not a skill").expect("write plain file");

    let app = fixture.app();
    let row = app.inventory().row("shared").expect("shared row");

    assert_eq!(row.provenance(), RowProvenance::Source("library"));
}

#[test]
fn nothing_is_read_until_setup_reaches_the_step_that_reads_it() {
    let fixture = Fixture::new();
    let root = fixture.create_root(AgentKind::ClaudeCode);
    write_skill(&root.join("installed"), "installed");

    // A first run opens on Welcome, before the user has said which agents
    // Skilled should configure.
    let mut app = fixture.app();
    let inventory = app.inventory();
    for agent in AgentKind::ALL {
        assert_eq!(inventory.root(agent).status(), &RootStatus::NotScanned);
    }
    assert!(inventory.rows().is_empty());

    // Step six is where the roots are read.
    for _ in 0..5 {
        let update = app.update(skilled::Action::Continue);
        app.perform_effects(update.effects())
            .expect("setup effects");
    }
    assert_eq!(
        app.view(),
        skilled::View::Setup(skilled::SetupStep::ScanInstallations)
    );
    assert_eq!(
        app.inventory().root(AgentKind::ClaudeCode).status(),
        &RootStatus::Scanned { installed: 1 }
    );
}

#[test]
fn an_unreadable_registered_source_leaves_provenance_unverified_rather_than_denied() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    drop(fixture.registered(&repository));
    // Content installed from somewhere Skilled cannot see into.
    let elsewhere = fixture.path().join("elsewhere/portable");
    write_skill(&elsewhere, "portable");
    fixture.install_symlink(AgentKind::ClaudeCode, "portable", &elsewhere);

    // With every source readable, the link plainly comes from none of them.
    let app = fixture.app();
    let row = app.inventory().row("portable").expect("portable row");
    assert_eq!(row.health(), InstallationHealth::Unmanaged);
    assert_eq!(
        row.observation(AgentKind::ClaudeCode)
            .expect("observation")
            .findings()[0]
            .code(),
        "install.unmanaged"
    );
    drop(app);

    // Take the registered checkout away. Its candidates are now unknown, so
    // absence from the index is no longer proof the link came from elsewhere.
    fs::rename(&repository, fixture.path().join("moved-away")).expect("move the checkout away");

    let app = fixture.app();
    let row = app.inventory().row("portable").expect("portable row");

    assert_eq!(row.health(), InstallationHealth::Unverified);
    let finding = &row
        .observation(AgentKind::ClaudeCode)
        .expect("observation")
        .findings()[0];
    assert_eq!(finding.code(), "install.provenance_unverified");
    assert_eq!(finding.severity(), FindingSeverity::Warning);
    // Nothing claims the installation is unowned — including the row's own
    // provenance, which the Source column and the detail pane both read.
    assert!(
        !finding.evidence().contains("does not come from"),
        "{finding:?}"
    );
    assert_eq!(row.provenance(), RowProvenance::Unverified);
    assert_eq!(row.provenance().label(), "unverified");
}

#[test]
fn an_invalid_installation_keeps_the_provenance_uncertainty_of_a_valid_one() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    drop(fixture.registered(&repository));
    // Content that will not validate, installed from somewhere unknown.
    let elsewhere = fixture.path().join("elsewhere/malformed");
    fs::create_dir_all(&elsewhere).expect("create malformed content");
    fs::write(elsewhere.join("SKILL.md"), "no frontmatter here\n").expect("write malformed");
    fixture.install_symlink(AgentKind::ClaudeCode, "malformed", &elsewhere);
    fs::rename(&repository, fixture.path().join("moved-away")).expect("move the checkout away");

    let app = fixture.app();
    let row = app.inventory().row("malformed").expect("malformed row");

    // Failing validation says nothing about where the content came from, so
    // the row must not fall back to claiming Skilled does not manage it.
    assert_eq!(row.health(), InstallationHealth::Broken);
    assert_eq!(row.provenance(), RowProvenance::Unverified);
    assert_eq!(
        row.observation(AgentKind::ClaudeCode)
            .expect("observation")
            .provenance(),
        &Provenance::Unverified
    );
}

#[test]
fn one_unknown_provenance_stops_a_row_naming_the_source_of_the_others() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["shared"]);
    let vanishing = fixture.source("archive", &["shared"]);
    let mut app = fixture.registered(&repository);
    let preview = app.preview_source(&vanishing).expect("preview second");
    app.confirm_source(preview).expect("register second");
    drop(app);
    // One agent's installation resolves to a source that is still there; the
    // other's cannot be placed because a registered checkout is gone.
    fixture.install_symlink(
        AgentKind::ClaudeCode,
        "shared",
        &repository.join("skills/shared"),
    );
    let elsewhere = fixture.path().join("elsewhere/shared");
    write_skill(&elsewhere, "shared");
    fixture.install_symlink(AgentKind::Codex, "shared", &elsewhere);
    fs::rename(&vanishing, fixture.path().join("gone")).expect("move the second checkout away");

    let app = fixture.app();
    let row = app.inventory().row("shared").expect("shared row");

    // Naming the source that did resolve would imply the same source for the
    // installation that did not.
    assert_eq!(row.provenance(), RowProvenance::Unverified);
    assert_eq!(
        row.observation(AgentKind::ClaudeCode)
            .and_then(|observation| observation.resolution())
            .map(|resolution| resolution.source_label()),
        Some("library")
    );
}

#[test]
fn a_link_into_a_checkout_that_moved_away_is_not_declared_unregistered() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    drop(fixture.registered(&repository));
    fixture.install_symlink(
        AgentKind::ClaudeCode,
        "portable",
        &repository.join("skills/portable"),
    );
    // The link now dangles and its source cannot be read: two things Skilled
    // does not know, not one it does.
    fs::rename(&repository, fixture.path().join("moved-away")).expect("move the checkout away");

    let app = fixture.app();
    let row = app.inventory().row("portable").expect("portable row");

    assert_eq!(row.health(), InstallationHealth::Broken);
    let observation = row
        .observation(AgentKind::ClaudeCode)
        .expect("dangling observation");
    assert_eq!(observation.findings()[0].code(), "install.dangling_symlink");
    assert_eq!(observation.provenance(), &Provenance::Unverified);
    assert_eq!(row.provenance(), RowProvenance::Unverified);
}

#[test]
fn counts_are_withheld_until_some_root_has_actually_been_looked_at() {
    let fixture = Fixture::new();
    let root = fixture.create_root(AgentKind::ClaudeCode);
    write_skill(&root.join("installed"), "installed");

    // Nothing read yet: a zero would be a result the scan never produced.
    let mut app = fixture.app();
    assert!(!app.inventory().counts_are_complete());

    // Every agent deselected: the roots were deliberately not looked at.
    app.update(skilled::Action::Continue);
    for _ in 0..3 {
        app.update(skilled::Action::ToggleSelection);
        app.update(skilled::Action::MoveSelection(1));
    }
    for _ in 0..4 {
        let update = app.update(skilled::Action::Continue);
        app.perform_effects(update.effects())
            .expect("setup effects");
    }
    assert!(!app.inventory().counts_are_complete());

    // A root that exists and one that does not are both observations.
    let fixture = Fixture::new();
    fixture.create_root(AgentKind::ClaudeCode);
    let mut app = fixture.app();
    for _ in 0..5 {
        let update = app.update(skilled::Action::Continue);
        app.perform_effects(update.effects())
            .expect("setup effects");
    }
    assert!(app.inventory().counts_are_complete());
}

#[test]
fn a_row_takes_the_worst_state_of_the_agents_that_carry_it() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["shared"]);
    drop(fixture.registered(&repository));
    // Healthy in one root, dangling in another, under the same name.
    fixture.install_symlink(
        AgentKind::ClaudeCode,
        "shared",
        &repository.join("skills/shared"),
    );
    fixture.install_symlink(AgentKind::Codex, "shared", &fixture.path().join("nowhere"));

    let app = fixture.app();
    let row = app.inventory().row("shared").expect("shared row");

    assert_eq!(
        row.observation(AgentKind::ClaudeCode).map(|o| o.health()),
        Some(InstallationHealth::Healthy)
    );
    assert_eq!(
        row.observation(AgentKind::Codex).map(|o| o.health()),
        Some(InstallationHealth::Broken)
    );
    assert_eq!(row.health(), InstallationHealth::Broken);
    // The healthy installation still names its source; the dangling one has
    // none, and one resolved source is not a disagreement.
    assert_eq!(row.provenance(), RowProvenance::Source("library"));
}

#[test]
fn a_missing_root_reports_absence_without_raising_a_finding() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    drop(fixture.registered(&repository));

    let app = fixture.app();
    let inventory = app.inventory();

    assert!(inventory.rows().is_empty());
    assert_eq!(findings(inventory).count(), 0);
    for agent in AgentKind::ALL {
        assert_eq!(inventory.root(agent).status(), &RootStatus::Missing);
        assert_eq!(
            inventory.root(agent).path(),
            fixture.home().join(match agent {
                AgentKind::ClaudeCode => CLAUDE_CODE_ROOT,
                AgentKind::Codex => CODEX_ROOT,
                AgentKind::OpenCode => OPENCODE_ROOT,
            })
        );
    }
}

#[test]
fn an_empty_root_is_scanned_and_distinguished_from_a_missing_one() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    drop(fixture.registered(&repository));
    fixture.create_root(AgentKind::ClaudeCode);

    let inventory_owner = fixture.app();
    let inventory = inventory_owner.inventory();

    assert_eq!(
        inventory.root(AgentKind::ClaudeCode).status(),
        &RootStatus::Scanned { installed: 0 }
    );
    assert_eq!(
        inventory.root(AgentKind::Codex).status(),
        &RootStatus::Missing
    );
}

#[test]
fn counts_separate_installations_unmanaged_content_and_findings() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["managed"]);
    drop(fixture.registered(&repository));
    fixture.install_symlink(
        AgentKind::ClaudeCode,
        "managed",
        &repository.join("skills/managed"),
    );
    let root = fixture.create_root(AgentKind::Codex);
    write_skill(&root.join("copied"), "copied");
    fs::write(root.join("stray.txt"), "not a skill").expect("write plain file");

    let app = fixture.app();
    let inventory = app.inventory();

    // Two links and a physical copy are installations; the stray file is not.
    assert_eq!(inventory.installation_count(), 2);
    assert_eq!(inventory.unmanaged_count(), 1);
    assert_eq!(inventory.broken_count(), 0);
    assert_eq!(inventory.skill_row_count(), 2);
    assert_eq!(inventory.rows().len(), 3);
}

#[test]
fn scanning_never_launches_a_coding_agent_executable() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    drop(fixture.registered(&repository));
    fixture.install_symlink(
        AgentKind::ClaudeCode,
        "portable",
        &repository.join("skills/portable"),
    );
    let (executables, witness) = fixture.trap_agent_executables();

    let app = SkilledApp::open(AppEnvironment::new(
        fixture.home(),
        fixture.data(),
        executables.as_os_str(),
    ))
    .expect("open application");

    assert_eq!(
        app.inventory().row("portable").map(|row| row.health()),
        Some(InstallationHealth::Healthy)
    );
    // Detection found the executables, so the scan had every opportunity to run
    // one; the untouched witness is the proof that it never did.
    assert!(
        app.agent(AgentKind::ClaudeCode).executable_path().is_some(),
        "the trap must be on the search path for this test to mean anything"
    );
    assert!(
        !witness.exists(),
        "an agent executable ran during the installation scan"
    );
}

#[test]
fn a_deselected_agent_root_is_left_alone_rather_than_reported_absent() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    let root = fixture.create_root(AgentKind::ClaudeCode);
    write_skill(&root.join("copied"), "copied");
    let mut app = fixture.registered(&repository);
    assert_eq!(app.inventory().rows().len(), 1);

    // Deselect Claude Code and rerun setup so the scan sees the new selection.
    app.update(skilled::Action::OpenSettings);
    let update = app.update(skilled::Action::RerunSetup);
    app.perform_effects(update.effects()).expect("reset setup");
    app.update(skilled::Action::Continue);
    app.update(skilled::Action::ToggleSelection);
    for _ in 0..6 {
        let update = app.update(skilled::Action::Continue);
        app.perform_effects(update.effects())
            .expect("setup effects");
    }

    let inventory = app.inventory();
    assert_eq!(
        inventory.root(AgentKind::ClaudeCode).status(),
        &RootStatus::NotSelected
    );
    // The directory is still there; Skilled simply did not look in it, and so
    // reports nothing about it either way.
    assert!(root.join("copied").is_dir());
    assert!(inventory.rows().is_empty());
    assert_eq!(findings(inventory).count(), 0);
}

#[test]
fn one_unreadable_entry_does_not_discard_the_rest_of_its_root() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    drop(fixture.registered(&repository));
    let root = fixture.create_root(AgentKind::ClaudeCode);
    write_skill(&root.join("readable"), "readable");
    fs::create_dir(root.join("opaque")).expect("create an entry to make unreadable");
    if running_as_root() {
        // Permission bits do not bind the superuser, so the condition this
        // test needs cannot be created.
        return;
    }
    // Readable but not searchable: the listing succeeds while stat on each
    // child fails, which is what an agent writing into its own root looks like.
    fs::set_permissions(&root, fs::Permissions::from_mode(0o444))
        .expect("drop search permission on the root");

    let inventory_owner = fixture.app();
    let inventory = inventory_owner.inventory();
    // Restore before any assertion can leave the fixture undeletable.
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).expect("restore permissions");

    let RootStatus::Scanned { installed } = inventory.root(AgentKind::ClaudeCode).status() else {
        panic!(
            "one unreadable entry must not fail the root: {:?}",
            inventory.root(AgentKind::ClaudeCode).status()
        );
    };
    assert_eq!(*installed, 2);
    for name in ["opaque", "readable"] {
        let row = inventory.row(name).expect("row for every entry");
        let observation = row
            .observation(AgentKind::ClaudeCode)
            .expect("observation for every entry");
        assert_eq!(row.health(), InstallationHealth::Broken);
        assert_eq!(observation.findings()[0].code(), "install.unreadable_entry");
        assert_eq!(
            observation.findings()[0].severity(),
            FindingSeverity::Critical
        );
        // The type was never read, so nothing may claim to know it.
        assert_eq!(observation.object(), &InstallationObject::Unknown);
        assert_eq!(observation.object().description(), "could not be read");
        assert!(observation.validation().is_none());
    }
    // An entry whose type is unknown might be an installation, so it is
    // counted: understating what is installed is the worse error.
    assert_eq!(inventory.broken_count(), 2);
}

#[test]
fn a_link_to_a_file_is_a_broken_installation_while_a_bare_file_is_not_one() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    drop(fixture.registered(&repository));
    let elsewhere = fixture.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).expect("create elsewhere");
    fs::write(elsewhere.join("notes.md"), "prose").expect("write a file to link at");
    fixture.install_symlink(AgentKind::ClaudeCode, "linked", &elsewhere.join("notes.md"));
    let root = fixture.create_root(AgentKind::ClaudeCode);
    fs::write(root.join("bare.md"), "prose").expect("write a bare file");

    let app = fixture.app();
    let inventory = app.inventory();

    // A link is an installation someone meant to make, so a link to something
    // that cannot be a skill is broken. A file that was simply left in the
    // root was never an installation, so it is not broken — it is not a skill.
    let linked = inventory.row("linked").expect("linked row");
    assert_eq!(linked.health(), InstallationHealth::Broken);
    let bare = inventory.row("bare.md").expect("bare file row");
    assert_eq!(bare.health(), InstallationHealth::NotASkill);
    assert_eq!(inventory.broken_count(), 1);
    assert_eq!(inventory.skill_row_count(), 1);
}

#[test]
fn a_root_behind_a_denied_parent_is_unreadable_rather_than_reported_missing() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    drop(fixture.registered(&repository));
    // The root exists, but its parent cannot be searched, so Skilled cannot
    // observe it at all — which is not the same as observing that it is absent.
    let root = fixture.create_root(AgentKind::ClaudeCode);
    let parent = root.parent().expect("root parent").to_path_buf();
    if running_as_root() {
        return;
    }
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o444))
        .expect("drop search permission on the parent");

    let inventory_owner = fixture.app();
    let status = inventory_owner
        .inventory()
        .root(AgentKind::ClaudeCode)
        .status()
        .clone();
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).expect("restore permissions");

    assert!(
        matches!(status, RootStatus::Unreadable { .. }),
        "a denied root must not be reported as missing: {status:?}"
    );
    assert_ne!(status, RootStatus::Missing);
}

#[test]
fn a_root_that_is_not_a_directory_is_unreadable_and_names_the_reason() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    drop(fixture.registered(&repository));
    let root = fixture.root(AgentKind::Codex);
    fs::create_dir_all(root.parent().expect("root parent")).expect("create root parent");
    fs::write(&root, "not a directory").expect("write a file where the root belongs");

    let app = fixture.app();
    let inventory = app.inventory();

    let RootStatus::Unreadable { message } = inventory.root(AgentKind::Codex).status() else {
        panic!(
            "expected an unreadable root, got {:?}",
            inventory.root(AgentKind::Codex).status()
        );
    };
    assert!(message.contains("not a directory"), "{message}");
    // The reason is reachable for display; a root reported as merely missing
    // would hide a real obstruction.
    assert_eq!(
        inventory
            .unreadable_roots()
            .map(|(root, _)| root.agent())
            .collect::<Vec<_>>(),
        [AgentKind::Codex]
    );
    assert!(inventory.rows().is_empty());
}

#[test]
fn a_symlink_that_cannot_be_resolved_is_not_reported_as_a_dangling_one() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    drop(fixture.registered(&repository));
    // A link to itself resolves to neither a target nor a missing file.
    let root = fixture.create_root(AgentKind::ClaudeCode);
    symlink(root.join("looping"), root.join("looping")).expect("install a looping link");

    let app = fixture.app();
    let row = app.inventory().row("looping").expect("looping row");

    assert_eq!(row.health(), InstallationHealth::Broken);
    let finding = &row
        .observation(AgentKind::ClaudeCode)
        .expect("looping observation")
        .findings()[0];
    assert_eq!(finding.code(), "install.unresolvable_symlink");
    assert_eq!(finding.severity(), FindingSeverity::Critical);
    // The cause is reported, not guessed at.
    assert!(
        !finding.evidence().contains("does not exist"),
        "{finding:?}"
    );
}

#[test]
fn a_root_beyond_the_child_limit_is_reported_unreadable_rather_than_partially_scanned() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    drop(fixture.registered(&repository));
    let root = fixture.create_root(AgentKind::ClaudeCode);
    for index in 0..skilled::inventory::MAX_ROOT_CHILDREN {
        fs::create_dir(root.join(format!("skill-{index:05}"))).expect("create bounded fixture");
    }

    // Exactly at the limit is still readable; the cap is a ceiling, not a wall
    // one short of it.
    assert_eq!(
        fixture
            .app()
            .inventory()
            .root(AgentKind::ClaudeCode)
            .status(),
        &RootStatus::Scanned {
            installed: skilled::inventory::MAX_ROOT_CHILDREN
        }
    );
    fs::create_dir(root.join("one-too-many")).expect("exceed the bounded fixture");

    let app = fixture.app();
    let inventory = app.inventory();

    assert!(matches!(
        inventory.root(AgentKind::ClaudeCode).status(),
        RootStatus::Unreadable { .. }
    ));
    assert!(
        inventory.rows().is_empty(),
        "a root that exceeded its limit must not contribute partial rows"
    );
}

struct Fixture {
    directory: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self {
            directory: tempfile::tempdir().expect("temporary application directory"),
        }
    }

    fn path(&self) -> &Path {
        self.directory.path()
    }

    fn home(&self) -> PathBuf {
        self.directory.path().join("home")
    }

    fn data(&self) -> PathBuf {
        self.directory.path().join("data")
    }

    fn environment(&self) -> AppEnvironment {
        AppEnvironment::new(self.home(), self.data(), "")
    }

    fn app(&self) -> SkilledApp {
        SkilledApp::open(self.environment()).expect("open application")
    }

    /// An application whose setup is complete and whose only source is
    /// `repository`, registered with its default catalogs.
    fn registered(&self, repository: &Path) -> SkilledApp {
        let mut app = self.app();
        let preview = app.preview_source(repository).expect("preview source");
        app.confirm_source(preview).expect("register source");
        for _ in 0..7 {
            let update = app.update(skilled::Action::Continue);
            app.perform_effects(update.effects())
                .expect("perform setup effects");
        }
        app
    }

    fn source(&self, name: &str, skills: &[&str]) -> PathBuf {
        let repository = self.directory.path().join(name);
        for skill in skills {
            write_skill(&repository.join("skills").join(skill), skill);
        }
        git(&repository, &["init", "-b", "main"]);
        git(&repository, &["config", "user.name", "Skilled Test"]);
        git(
            &repository,
            &["config", "user.email", "skilled@example.test"],
        );
        git(&repository, &["add", "."]);
        git(&repository, &["commit", "-m", "fixture"]);
        repository
    }

    fn root(&self, agent: AgentKind) -> PathBuf {
        self.home().join(match agent {
            AgentKind::ClaudeCode => CLAUDE_CODE_ROOT,
            AgentKind::Codex => CODEX_ROOT,
            AgentKind::OpenCode => OPENCODE_ROOT,
        })
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

    /// Put an executable named after each agent on the search path that records
    /// having been run, and return the search path and the witness file.
    fn trap_agent_executables(&self) -> (PathBuf, PathBuf) {
        let directory = self.directory.path().join("trap");
        fs::create_dir_all(&directory).expect("create trap directory");
        let witness = self.directory.path().join("agent-was-launched");
        for name in ["claude", "codex", "opencode"] {
            let executable = directory.join(name);
            fs::write(
                &executable,
                format!("#!/bin/sh\ntouch {}\n", witness.display()),
            )
            .expect("write trap executable");
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
                .expect("mark trap executable");
        }
        (directory, witness)
    }
}

/// Whether permission bits will be enforced against this process.
fn running_as_root() -> bool {
    // Reading a mode-0 directory succeeds only for the superuser.
    let probe = std::env::temp_dir().join(format!("skilled-root-probe-{}", std::process::id()));
    let _ = fs::remove_dir_all(&probe);
    fs::create_dir(&probe).expect("create permission probe");
    fs::set_permissions(&probe, fs::Permissions::from_mode(0o000)).expect("seal probe");
    let readable = fs::read_dir(&probe).is_ok();
    fs::set_permissions(&probe, fs::Permissions::from_mode(0o755)).expect("unseal probe");
    fs::remove_dir_all(&probe).expect("remove probe");
    readable
}

/// Every finding the snapshot holds, across every row and agent.
fn findings(inventory: &skilled::inventory::InventorySnapshot) -> impl Iterator<Item = &Finding> {
    inventory
        .rows()
        .iter()
        .flat_map(skilled::inventory::InventoryRow::findings)
}

fn write_skill(directory: &Path, name: &str) {
    fs::create_dir_all(directory).expect("create skill directory");
    fs::write(
        directory.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {name} fixture\n---\n# {name}\n"),
    )
    .expect("write SKILL.md");
}

fn git(repository: &Path, arguments: &[&str]) {
    fs::create_dir_all(repository).expect("create repository directory");
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .expect("run Git fixture command");
    assert!(output.status.success(), "Git command failed: {output:?}");
}
