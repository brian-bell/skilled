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
    inventory::{FindingSeverity, InstallationHealth, InstallationObject, RootStatus},
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
    assert_eq!(row.source_label(), Some("library"));
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
    assert_eq!(row.source_label(), None);
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
    assert_eq!(row.source_label(), None);
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
    assert_eq!(row.source_label(), None);
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
fn a_plain_file_child_is_broken_without_being_read_as_a_skill() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    drop(fixture.registered(&repository));
    let root = fixture.create_root(AgentKind::OpenCode);
    fs::write(root.join("notes.md"), "not a skill").expect("write plain file");

    let app = fixture.app();
    let row = app.inventory().row("notes.md").expect("plain file row");

    assert_eq!(row.health(), InstallationHealth::Broken);
    let observation = row
        .observation(AgentKind::OpenCode)
        .expect("plain file observation");
    assert_eq!(observation.object(), &InstallationObject::File);
    assert_eq!(observation.findings()[0].code(), "skill.missing_skill_md");
}

#[test]
fn a_missing_root_reports_absence_without_raising_a_finding() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    drop(fixture.registered(&repository));

    let app = fixture.app();
    let inventory = app.inventory();

    assert!(inventory.rows().is_empty());
    assert_eq!(inventory.finding_count(), 0);
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

    assert_eq!(inventory.installation_count(), 3);
    assert_eq!(inventory.unmanaged_count(), 1);
    // One informational unmanaged finding and one critical missing-SKILL.md.
    assert_eq!(inventory.finding_count(), 2);
    assert_eq!(inventory.critical_finding_count(), 1);
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
fn a_root_beyond_the_child_limit_is_reported_unreadable_rather_than_partially_scanned() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    drop(fixture.registered(&repository));
    let root = fixture.create_root(AgentKind::ClaudeCode);
    for index in 0..=skilled::inventory::MAX_ROOT_CHILDREN {
        fs::create_dir(root.join(format!("skill-{index:05}"))).expect("create bounded fixture");
    }

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
