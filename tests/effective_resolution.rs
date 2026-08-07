//! Multi-source variant identity and OpenCode effective resolution.
//!
//! Every fixture builds its own temporary home and its own checkouts; no test
//! may read the real user home or a real agent skill root. Symbolic links are
//! the primary managed installation shape, so the whole suite is Unix-only.
#![cfg(unix)]

use std::{
    collections::BTreeSet,
    fs,
    os::unix::fs::symlink,
    path::{Path, PathBuf},
    process::Command,
};

use skilled::{
    Action, AgentKind, AppEnvironment, SkilledApp,
    inventory::{Finding, FindingSeverity, InventoryRow, InventorySnapshot, RowVerdict},
    resolution::{CandidateSelection, OpenCodeResolution},
};

const CLAUDE_CODE_ROOT: &str = ".claude/skills";
const CODEX_ROOT: &str = ".agents/skills";
const OPENCODE_ROOT: &str = ".config/opencode/skills";

/// Acceptance 1: two registered repositories are inventoried accurately, and
/// nothing under any agent root moves while it happens.
#[test]
fn two_registered_repositories_are_inventoried_without_changing_agent_paths() {
    let fixture = Fixture::new();
    let first = fixture.source("alpha", &[("skills", &["review"])]);
    let second = fixture.source("beta", &[("skills", &["release"])]);
    drop(fixture.registered(&[&first, &second]));
    fixture.install_symlink(
        AgentKind::ClaudeCode,
        "review",
        &first.join("skills/review"),
    );
    fixture.install_symlink(AgentKind::Codex, "release", &second.join("skills/release"));
    let before = fixture.root_contents();

    let app = fixture.app();
    let inventory = app.inventory();

    assert_eq!(app.sources().len(), 2);
    let review = inventory.row("review").expect("review row");
    assert_eq!(
        review
            .observation(AgentKind::ClaudeCode)
            .and_then(|observation| observation.resolution())
            .map(|resolution| resolution.source_label().to_owned()),
        Some("alpha".to_owned())
    );
    let release = inventory.row("release").expect("release row");
    assert_eq!(
        release
            .observation(AgentKind::Codex)
            .and_then(|observation| observation.resolution())
            .map(|resolution| resolution.source_label().to_owned()),
        Some("beta".to_owned())
    );
    assert_eq!(fixture.root_contents(), before, "the scan changed a root");
}

/// Acceptance 2: same-named Claude Code and Codex editions keep their own
/// identity — source, catalog, and variant path — and each agent selects its
/// own. OpenCode sees both roots, so the same fixture is also the conflicting
/// duplicate of spec 5.1, and says so rather than picking one silently.
#[test]
fn same_named_claude_and_codex_variants_stay_independently_identifiable() {
    let fixture = Fixture::new();
    let repository = fixture.source(
        "library",
        &[
            ("claude/skills", &["review"]),
            ("codex/skills", &["review"]),
        ],
    );
    drop(fixture.registered(&[&repository]));
    fixture.install_symlink(
        AgentKind::ClaudeCode,
        "review",
        &repository.join("claude/skills/review"),
    );
    fixture.install_symlink(
        AgentKind::Codex,
        "review",
        &repository.join("codex/skills/review"),
    );

    let app = fixture.app();
    let row = app.inventory().row("review").expect("review row");

    for (agent, catalog) in [
        (AgentKind::ClaudeCode, "claude/skills"),
        (AgentKind::Codex, "codex/skills"),
    ] {
        let resolution = row
            .observation(agent)
            .and_then(|observation| observation.resolution())
            .expect("each edition resolves to its own variant");
        assert_eq!(resolution.catalog_relative_path(), Path::new(catalog));
        assert_eq!(
            resolution.variant_relative_path(),
            Path::new(catalog).join("review")
        );
        // Each agent selects the edition meant for it, and only that one.
        let CandidateSelection::Selected(variant) =
            skilled::resolution::select_candidates(app.sources(), agent, "review")
        else {
            panic!("{agent:?} has exactly one compatible variant");
        };
        assert_eq!(variant.catalog_relative_path(), Path::new(catalog));
    }

    // Two different directories answer to one name in the roots OpenCode reads.
    assert!(matches!(
        row.opencode_resolution(),
        Some(OpenCodeResolution::Conflict { .. })
    ));
    assert_eq!(row.verdict(), RowVerdict::Conflict);
}

/// Acceptance 3, benign alias: one directory reached through all three roots.
/// The native installation wins, the others are informational, and the row is
/// not degraded by them.
#[test]
fn one_variant_linked_from_every_root_is_a_benign_alias() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &[("skills", &["review"])]);
    drop(fixture.registered(&[&repository]));
    let variant = repository.join("skills/review");
    for agent in AgentKind::ALL {
        fixture.install_symlink(agent, "review", &variant);
    }

    let app = fixture.app();
    let row = app.inventory().row("review").expect("review row");

    let Some(OpenCodeResolution::Selected { winner, aliases }) = row.opencode_resolution() else {
        panic!("one directory through three roots is one selection");
    };
    assert_eq!(winner.root(), AgentKind::OpenCode);
    assert_eq!(
        aliases.iter().map(|alias| alias.root()).collect::<Vec<_>>(),
        [AgentKind::Codex, AgentKind::ClaudeCode]
    );
    assert_eq!(row.verdict(), RowVerdict::Healthy);

    // The alias is one fact about the row, stated once, and no installation is
    // accused of being the alias it merely is.
    let finding = resolution_finding(row, "variant.benign_alias");
    assert_eq!(finding.severity(), FindingSeverity::Info);
    assert!(
        finding.evidence().contains(CODEX_ROOT) && finding.evidence().contains(CLAUDE_CODE_ROOT),
        "the alias evidence names every path: {finding:?}"
    );
    for agent in AgentKind::ALL {
        assert!(
            codes(row, agent).is_empty(),
            "{agent:?} carries a finding about the row rather than about itself"
        );
    }
}

/// Acceptance 3, conflicting duplicate: different directories behind one name.
/// Every path is named, and so is the rule that decides which one wins.
#[test]
fn different_directories_behind_one_name_are_a_conflicting_duplicate() {
    let fixture = Fixture::new();
    let first = fixture.source("alpha", &[("skills", &["review"])]);
    let second = fixture.source("beta", &[("skills", &["review"])]);
    drop(fixture.registered(&[&first, &second]));
    fixture.install_symlink(AgentKind::OpenCode, "review", &first.join("skills/review"));
    fixture.install_symlink(
        AgentKind::ClaudeCode,
        "review",
        &second.join("skills/review"),
    );

    let app = fixture.app();
    let row = app.inventory().row("review").expect("review row");

    assert!(matches!(
        row.opencode_resolution(),
        Some(OpenCodeResolution::Conflict { .. })
    ));
    assert_eq!(row.verdict(), RowVerdict::Conflict);
    // One conflict, however many roots are party to it, naming them all.
    assert_eq!(row.resolution_findings().len(), 1);
    let finding = resolution_finding(row, "variant.duplicate_for_agent");
    assert_eq!(finding.severity(), FindingSeverity::Critical);
    assert!(
        finding.evidence().contains(OPENCODE_ROOT) && finding.evidence().contains(CLAUDE_CODE_ROOT),
        "the conflict names every path: {finding:?}"
    );
    assert!(
        finding.evidence().contains("OpenCode"),
        "the conflict names the agent it is a conflict for: {finding:?}"
    );
}

/// Acceptance 3, foreign exposure: a Claude Code edition reaches OpenCode
/// through a compatibility root, and no OpenCode-compatible variant is visible.
/// Skilled reports the exposure rather than claiming OpenCode usability.
#[test]
fn a_claude_only_variant_seen_through_a_compatibility_root_is_foreign_exposure() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &[("claude/skills", &["review"])]);
    drop(fixture.registered(&[&repository]));
    fixture.install_symlink(
        AgentKind::ClaudeCode,
        "review",
        &repository.join("claude/skills/review"),
    );

    let app = fixture.app();
    let row = app.inventory().row("review").expect("review row");

    assert!(matches!(
        row.opencode_resolution(),
        Some(OpenCodeResolution::ForeignExposure { .. })
    ));
    assert_eq!(row.verdict(), RowVerdict::ForeignVariant);
    let finding = resolution_finding(row, "variant.foreign_opencode_exposure");
    assert_eq!(finding.severity(), FindingSeverity::Warning);
    assert!(
        finding.evidence().contains("library") && finding.evidence().contains("claude/skills"),
        "the exposure names the source and the variant: {finding:?}"
    );
}

/// The non-case beside it: content Skilled did not place is not claimed to be a
/// foreign variant, because compatibility cannot be checked for content that
/// resolved to no registered variant.
#[test]
fn unregistered_content_in_a_compatibility_root_is_not_foreign_exposure() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &[("skills", &["other"])]);
    drop(fixture.registered(&[&repository]));
    write_skill(
        &fixture.root(AgentKind::ClaudeCode).join("review"),
        "review",
    );

    let app = fixture.app();
    let row = app.inventory().row("review").expect("review row");

    assert!(matches!(
        row.opencode_resolution(),
        Some(OpenCodeResolution::Selected { .. })
    ));
    assert!(
        row.resolution_findings().is_empty(),
        "unregistered content cannot be shown to be foreign"
    );
}

/// D3: a root OpenCode consults that Skilled was asked to leave alone was never
/// read, so no effective resolution is stated over it — and no classification
/// is invented for the roots that were read.
#[test]
fn a_deselected_compatibility_root_leaves_the_resolution_incomplete() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &[("skills", &["review"])]);
    drop(fixture.registered_with(&[&repository], [false, true, true]));
    fixture.install_symlink(
        AgentKind::OpenCode,
        "review",
        &repository.join("skills/review"),
    );

    let app = fixture.app();
    let row = app.inventory().row("review").expect("review row");

    let Some(OpenCodeResolution::Incomplete { roots }) = row.opencode_resolution() else {
        panic!("an unread root cannot be classified over");
    };
    assert_eq!(roots, &[AgentKind::ClaudeCode]);
    assert!(
        row.resolution_findings().is_empty(),
        "no classification is stated over a root that was not read"
    );
}

/// Two registered repositories offering the same common name leave the agent
/// with no unambiguous variant. Nothing is installed, so usability is uncertain
/// rather than broken, and the finding carries the evidence contract: skill,
/// agent, and every competing variant identity.
#[test]
fn competing_registered_variants_are_a_selection_conflict() {
    let fixture = Fixture::new();
    let first = fixture.source("alpha", &[("skills", &["review"])]);
    let second = fixture.source("beta", &[("skills", &["review"])]);
    drop(fixture.registered(&[&first, &second]));

    let app = fixture.app();
    let findings = app.inventory().selection_findings();

    let claude = findings
        .iter()
        .find(|finding| finding.agent() == AgentKind::ClaudeCode)
        .expect("Claude Code has no unambiguous variant");
    assert_eq!(claude.skill_name(), "review");
    assert_eq!(claude.finding().code(), "variant.duplicate_for_agent");
    assert_eq!(claude.finding().severity(), FindingSeverity::Warning);
    assert_eq!(
        claude
            .variants()
            .iter()
            .map(|variant| variant.source_label().to_owned())
            .collect::<Vec<_>>(),
        ["alpha", "beta"]
    );
    let evidence = claude.finding().evidence();
    assert!(
        evidence.contains("alpha") && evidence.contains("beta") && evidence.contains("Claude Code"),
        "the evidence names the agent and every competing variant: {evidence}"
    );
    // One finding per agent and skill, not one per competing variant.
    assert_eq!(findings.len(), AgentKind::ALL.len());
}

/// The same ambiguity over a name that is installed is critical rather than
/// uncertain: an installation is already resolving to one of them.
#[test]
fn a_selection_conflict_over_an_installed_name_is_critical() {
    let fixture = Fixture::new();
    let first = fixture.source("alpha", &[("skills", &["review"])]);
    let second = fixture.source("beta", &[("skills", &["review"])]);
    drop(fixture.registered(&[&first, &second]));
    fixture.install_symlink(
        AgentKind::ClaudeCode,
        "review",
        &first.join("skills/review"),
    );

    let app = fixture.app();
    let findings = app.inventory().selection_findings();

    let claude = findings
        .iter()
        .find(|finding| finding.agent() == AgentKind::ClaudeCode)
        .expect("Claude Code has no unambiguous variant");
    assert_eq!(claude.finding().severity(), FindingSeverity::Critical);
    let codex = findings
        .iter()
        .find(|finding| finding.agent() == AgentKind::Codex)
        .expect("Codex has no unambiguous variant either");
    assert_eq!(codex.finding().severity(), FindingSeverity::Warning);
}

/// Doctor orders findings by the spec 9.5 groups before severity, so a broken
/// installation leads an informational alias whatever the codes are called.
#[test]
fn doctor_orders_findings_by_the_documented_groups() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &[("skills", &["review"])]);
    drop(fixture.registered(&[&repository]));
    let variant = repository.join("skills/review");
    for agent in AgentKind::ALL {
        fixture.install_symlink(agent, "review", &variant);
    }
    fixture.install_symlink(
        AgentKind::ClaudeCode,
        "dangling",
        &repository.join("skills/absent"),
    );

    let app = fixture.app();
    let ordered: Vec<&str> = app
        .inventory()
        .doctor_findings()
        .map(|entry| entry.finding().code())
        .collect();

    assert_eq!(
        ordered,
        ["install.dangling_symlink", "variant.benign_alias"],
        "broken installations lead informational aliases"
    );
}

fn codes(row: &InventoryRow, agent: AgentKind) -> Vec<&'static str> {
    row.observation(agent)
        .map(|observation| {
            observation
                .findings()
                .iter()
                .map(Finding::code)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn resolution_finding<'a>(row: &'a InventoryRow, code: &str) -> &'a Finding {
    row.resolution_findings()
        .iter()
        .find(|finding| finding.code() == code)
        .unwrap_or_else(|| panic!("no {code} on {}", row.name()))
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

    fn home(&self) -> PathBuf {
        self.directory.path().join("home")
    }

    fn environment(&self) -> AppEnvironment {
        AppEnvironment::new(self.home(), self.directory.path().join("data"), "")
    }

    fn app(&self) -> SkilledApp {
        SkilledApp::open(self.environment()).expect("open application")
    }

    fn registered(&self, repositories: &[&Path]) -> SkilledApp {
        self.registered_with(repositories, [true; 3])
    }

    /// An application whose setup is complete, with these repositories
    /// registered and these agents selected.
    fn registered_with(&self, repositories: &[&Path], selections: [bool; 3]) -> SkilledApp {
        let mut app = self.app();
        for repository in repositories {
            let preview = app.preview_source(repository).expect("preview source");
            app.confirm_source(preview).expect("register source");
        }
        // Welcome, then the agent selection step, where anything the fixture
        // asked to be left alone is toggled off before setup moves on.
        self.advance(&mut app);
        for (index, selected) in selections.into_iter().enumerate() {
            if !selected {
                for _ in 0..index {
                    app.update(Action::MoveSelection(1));
                }
                app.update(Action::ToggleSelection);
                for _ in 0..index {
                    app.update(Action::MoveSelection(-1));
                }
            }
        }
        for _ in 0..6 {
            self.advance(&mut app);
        }
        app
    }

    fn advance(&self, app: &mut SkilledApp) {
        let update = app.update(Action::Continue);
        app.perform_effects(update.effects())
            .expect("perform setup effects");
    }

    /// A checkout holding the named catalogs, each with the named skills.
    fn source(&self, name: &str, catalogs: &[(&str, &[&str])]) -> PathBuf {
        let repository = self.directory.path().join(name);
        for (catalog, skills) in catalogs {
            for skill in *skills {
                write_skill(&repository.join(catalog).join(skill), skill);
            }
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

    fn install_symlink(&self, agent: AgentKind, name: &str, target: &Path) {
        let root = self.root(agent);
        fs::create_dir_all(&root).expect("create agent skill root");
        symlink(target, root.join(name)).expect("install symbolic link");
    }

    /// Every entry of every agent root, as a set a scan must not disturb.
    fn root_contents(&self) -> BTreeSet<PathBuf> {
        AgentKind::ALL
            .into_iter()
            .flat_map(|agent| fs::read_dir(self.root(agent)).into_iter().flatten())
            .map(|entry| entry.expect("read a root entry").path())
            .collect()
    }
}

/// Every finding the snapshot holds, however it is filed.
#[allow(dead_code)]
fn all_findings(inventory: &InventorySnapshot) -> impl Iterator<Item = &Finding> {
    inventory
        .rows()
        .iter()
        .flat_map(InventoryRow::findings)
        .chain(
            inventory
                .selection_findings()
                .iter()
                .map(|selection| selection.finding()),
        )
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
