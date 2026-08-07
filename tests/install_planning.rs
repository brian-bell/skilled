//! The pure install planner, over real directories.
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
    AgentKind, AppEnvironment, SkilledApp,
    operations::{
        ExcludedReason, InstallPlan, TargetDisposition, locate_variant, plan_install, probe_install,
    },
    resolution::VariantRef,
};

/// The exact global roots documented by each agent adapter.
///
/// Spelled out literally rather than derived from the adapters, so a change to
/// a documented path has to be made deliberately in both places.
const CLAUDE_CODE_ROOT: &str = ".claude/skills";
const CODEX_ROOT: &str = ".agents/skills";
const OPENCODE_ROOT: &str = ".config/opencode/skills";

const ROOTS: [(AgentKind, &str); 3] = [
    (AgentKind::ClaudeCode, CLAUDE_CODE_ROOT),
    (AgentKind::Codex, CODEX_ROOT),
    (AgentKind::OpenCode, OPENCODE_ROOT),
];

/// Spec 11.3: one individual directory symbolic link per agent, at that
/// agent's own documented global root, named for the skill. The plan states
/// every path in full — a preview a user is asked to confirm may not abbreviate
/// what is about to be written.
#[test]
fn a_common_variant_plans_one_link_per_configured_agent_at_its_documented_root() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    let app = fixture.registered(&repository);
    fixture.create_root_parents();

    let plan = fixture.plan(&app, "portable");

    assert_eq!(plan.skill_name(), "portable");
    assert_eq!(
        plan.source_dir(),
        repository
            .join("skills/portable")
            .canonicalize()
            .expect("canonical variant directory")
    );
    for (agent, root) in ROOTS {
        let target = plan.target(agent).expect("a target for every agent");
        assert_eq!(
            target.link_path(),
            fixture.home().join(root).join("portable")
        );
        assert!(target.link_path().is_absolute());
        assert_eq!(target.disposition(), &TargetDisposition::CreateRootAndLink);
    }
    assert!(plan.is_executable());
    assert!(plan.blocking_findings().next().is_none());
}

/// A root that already exists needs no creating, and the plan says so rather
/// than naming a step it would not take.
#[test]
fn an_existing_root_yields_a_link_step_alone() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    let app = fixture.registered(&repository);
    fixture.create_root_parents();
    fixture.create_root(AgentKind::Codex);

    let plan = fixture.plan(&app, "portable");

    assert_eq!(
        plan.target(AgentKind::Codex).unwrap().disposition(),
        &TargetDisposition::CreateLink
    );
    assert_eq!(
        plan.target(AgentKind::ClaudeCode).unwrap().disposition(),
        &TargetDisposition::CreateRootAndLink
    );
}

/// Spec 15 has Skilled create the documented skill root and nothing above it.
/// An agent whose own directory is absent is therefore left alone, and the
/// evidence names the directory that is missing rather than the link.
#[test]
fn a_root_whose_own_parent_is_missing_blocks_instead_of_creating_a_tree() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    let app = fixture.registered(&repository);

    let plan = fixture.plan(&app, "portable");

    for (agent, _) in ROOTS {
        assert_eq!(
            blocking_code(&plan, agent),
            Some("install.missing_root_parent"),
            "{agent:?} should refuse to create a tree"
        );
    }
    assert!(!plan.is_executable());
    let TargetDisposition::Blocked { finding } =
        plan.target(AgentKind::ClaudeCode).unwrap().disposition()
    else {
        unreachable!("just asserted");
    };
    assert!(
        finding
            .evidence()
            .contains(&fixture.home().join(".claude").display().to_string()),
        "the evidence names the missing directory: {}",
        finding.evidence()
    );
}

/// A link that already resolves to this very directory is nothing to do. It is
/// only reported as managed when Skilled holds a receipt for it: an identical
/// link someone else made stays unowned, because adopting one is a separate
/// decision.
#[test]
fn a_link_already_pointing_at_the_variant_is_no_work_and_is_not_adopted() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    let app = fixture.registered(&repository);
    fixture.create_root_parents();
    fixture.install_symlink(
        AgentKind::Codex,
        "portable",
        &repository.join("skills/portable"),
    );

    let plan = fixture.plan(&app, "portable");

    assert_eq!(
        plan.target(AgentKind::Codex).unwrap().disposition(),
        &TargetDisposition::AlreadyInstalled { managed: false }
    );
    // The other two are still work, so the plan as a whole remains executable.
    assert!(plan.is_executable());
}

/// Spec 11.3: every occupied slot is a refusal. A physical file and a physical
/// directory are both refused, whatever they hold — this release never
/// overwrites, so it never has to decide whether a copy is identical.
#[test]
fn a_physical_file_or_directory_blocks_the_target_it_occupies() {
    for occupant in ["file", "directory"] {
        let fixture = Fixture::new();
        let repository = fixture.source("library", &["portable"]);
        let app = fixture.registered(&repository);
        fixture.create_root_parents();
        let root = fixture.create_root(AgentKind::Codex);
        if occupant == "file" {
            fs::write(root.join("portable"), "not a skill").expect("write occupant");
        } else {
            write_skill(&root.join("portable"), "portable");
        }

        let plan = fixture.plan(&app, "portable");

        assert_eq!(
            blocking_code(&plan, AgentKind::Codex),
            Some("install.physical_path_collision"),
            "a physical {occupant} should block its target"
        );
        // A plan blocks whole: the other two targets are work, and none of it
        // happens while one target is blocked.
        assert!(!plan.is_executable());
        assert!(plan.is_blocked());
    }
}

/// A symbolic link Skilled does not own, pointing somewhere else, is a
/// collision rather than something to repair.
#[test]
fn an_unowned_symlink_pointing_elsewhere_blocks_its_target() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable", "other"]);
    let app = fixture.registered(&repository);
    fixture.create_root_parents();
    fixture.install_symlink(
        AgentKind::Codex,
        "portable",
        &repository.join("skills/other"),
    );

    let plan = fixture.plan(&app, "portable");

    assert_eq!(
        blocking_code(&plan, AgentKind::Codex),
        Some("install.unknown_symlink_collision")
    );
}

/// A link that resolves to nothing is broken, and repair does not exist yet, so
/// it blocks rather than being quietly replaced.
#[test]
fn a_dangling_symlink_blocks_rather_than_being_replaced() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    let app = fixture.registered(&repository);
    fixture.create_root_parents();
    fixture.install_symlink(
        AgentKind::Codex,
        "portable",
        &fixture.path().join("nowhere"),
    );

    let plan = fixture.plan(&app, "portable");

    assert_eq!(
        blocking_code(&plan, AgentKind::Codex),
        Some("install.dangling_symlink")
    );
    let TargetDisposition::Blocked { finding } =
        plan.target(AgentKind::Codex).unwrap().disposition()
    else {
        unreachable!("just asserted");
    };
    assert!(
        finding.evidence().contains("does not repair or replace"),
        "the evidence says no repair exists: {}",
        finding.evidence()
    );
}

/// Spec 6.4: two registered sources offering one name leave the agent's variant
/// ambiguous, and installing either would be picking for the user. The plan
/// blocks and names both.
#[test]
fn an_ambiguous_variant_blocks_every_agent_it_is_ambiguous_for() {
    let fixture = Fixture::new();
    let first = fixture.source("alpha", &["portable"]);
    let second = fixture.source("beta", &["portable"]);
    let mut app = fixture.registered(&first);
    let preview = app.preview_source(&second).expect("preview second source");
    app.confirm_source(preview).expect("register second source");
    fixture.create_root_parents();

    let variant = locate_variant(app.sources(), app.sources()[0].id(), None, "portable")
        .expect("one variant inside the first source");
    let plan = fixture.plan_variant(&app, &variant, [true; 3]);

    for (agent, _) in ROOTS {
        assert_eq!(
            blocking_code(&plan, agent),
            Some("variant.duplicate_for_agent"),
            "{agent:?} should refuse an ambiguous name"
        );
    }
    let TargetDisposition::Blocked { finding } =
        plan.target(AgentKind::Codex).unwrap().disposition()
    else {
        unreachable!("just asserted");
    };
    assert!(finding.evidence().contains("alpha"));
    assert!(finding.evidence().contains("beta"));
}

/// Spec 11.1: an exact agent-specific edition outranks a common one, so
/// installing the common variant for that agent would leave it loading
/// something else. The target is excluded and names what the agent prefers.
#[test]
fn an_agent_specific_edition_excludes_the_common_variant_for_that_agent_alone() {
    let fixture = Fixture::new();
    let repository = fixture.directory.path().join("library");
    write_skill(&repository.join("skills/portable"), "portable");
    write_skill(&repository.join(".claude/skills/portable"), "portable");
    initialize_repository(&repository);
    let app = fixture.registered(&repository);
    fixture.create_root_parents();

    let variant = locate_variant(
        app.sources(),
        app.sources()[0].id(),
        Some(Path::new("skills")),
        "portable",
    )
    .expect("the common variant");
    let plan = fixture.plan_variant(&app, &variant, [true; 3]);

    let TargetDisposition::Excluded {
        reason: ExcludedReason::AgentSpecificOverride { selected },
    } = plan.target(AgentKind::ClaudeCode).unwrap().disposition()
    else {
        panic!(
            "Claude Code prefers its own edition: {:?}",
            plan.target(AgentKind::ClaudeCode).unwrap().disposition()
        );
    };
    assert_eq!(
        selected.catalog_relative_path(),
        Path::new(".claude/skills")
    );
    // Codex has no edition of its own, so the common variant is still its work.
    assert_eq!(
        plan.target(AgentKind::Codex).unwrap().disposition(),
        &TargetDisposition::CreateRootAndLink
    );
    assert!(plan.is_executable());
}

/// A deselected agent is left alone, and an agent the request did not name is
/// left alone for a different reason. Neither is flattened into the other: one
/// is a standing configuration and the other is this request's scope.
#[test]
fn deselected_and_unrequested_agents_are_excluded_for_their_own_reasons() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    let mut app = fixture.registered(&repository);
    fixture.create_root_parents();
    deselect_last_agent(&mut app);

    let variant = fixture.variant(&app, "portable");
    let plan = fixture.plan_variant(&app, &variant, [true, false, true]);

    assert_eq!(
        plan.target(AgentKind::OpenCode).unwrap().disposition(),
        &TargetDisposition::Excluded {
            reason: ExcludedReason::NotConfigured
        }
    );
    assert_eq!(
        plan.target(AgentKind::Codex).unwrap().disposition(),
        &TargetDisposition::Excluded {
            reason: ExcludedReason::NotRequested
        }
    );
    assert_eq!(
        plan.target(AgentKind::ClaudeCode).unwrap().disposition(),
        &TargetDisposition::CreateRootAndLink
    );
}

/// A variant the agent could not use is excluded rather than blocked: nothing
/// is wrong, it is simply another agent's edition.
#[test]
fn an_agent_that_cannot_use_the_variant_is_excluded() {
    let fixture = Fixture::new();
    let repository = fixture.directory.path().join("library");
    write_skill(&repository.join(".claude/skills/portable"), "portable");
    initialize_repository(&repository);
    let app = fixture.registered(&repository);
    fixture.create_root_parents();

    let plan = fixture.plan(&app, "portable");

    assert_eq!(
        plan.target(AgentKind::ClaudeCode).unwrap().disposition(),
        &TargetDisposition::CreateRootAndLink
    );
    for agent in [AgentKind::Codex, AgentKind::OpenCode] {
        assert_eq!(
            plan.target(agent).unwrap().disposition(),
            &TargetDisposition::Excluded {
                reason: ExcludedReason::Incompatible
            },
            "{agent:?} cannot use another agent's edition"
        );
    }
}

/// A link points at the working tree, so uncommitted edits are what an agent
/// would load. That is stated before the user confirms, and does not stop them.
#[test]
fn an_uncommitted_source_checkout_warns_without_blocking() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    fs::write(
        repository.join("skills/portable/SKILL.md"),
        "---\nname: portable\ndescription: edited in the working tree\n---\n# portable\n",
    )
    .expect("dirty the checkout");
    let app = fixture.registered(&repository);
    fixture.create_root_parents();

    let plan = fixture.plan(&app, "portable");

    assert_eq!(plan.warnings().len(), 1, "{:?}", plan.warnings());
    assert!(plan.warnings()[0].contains("uncommitted changes"));
    assert!(plan.is_executable());
}

/// OpenCode reads the other two agents' roots, so a link written into its own
/// root can leave it ambiguous rather than installed. Skilled will not write a
/// link whose own postcondition it can already see failing.
#[test]
fn an_install_that_would_leave_opencode_ambiguous_blocks_its_target() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    let app = fixture.registered(&repository);
    fixture.create_root_parents();
    // A second directory OpenCode would load under the same name, reached
    // through Codex's root.
    let elsewhere = fixture.path().join("elsewhere/portable");
    write_skill(&elsewhere, "portable");
    fixture.install_symlink(AgentKind::Codex, "portable", &elsewhere);

    let variant = fixture.variant(&app, "portable");
    let plan = fixture.plan_variant(&app, &variant, [false, false, true]);

    assert_eq!(
        blocking_code(&plan, AgentKind::OpenCode),
        Some("install.opencode_conflict")
    );
    assert!(!plan.is_executable());
}

/// Installing another agent's edition into that agent's own root is ordinary
/// work, even though OpenCode can see it there and cannot use it. Skilled says
/// so and carries on: exposure is an arrangement, not a fault in the install.
#[test]
fn exposure_to_opencode_is_stated_as_a_warning_rather_than_blocking() {
    let fixture = Fixture::new();
    let repository = fixture.directory.path().join("library");
    write_skill(&repository.join(".claude/skills/portable"), "portable");
    initialize_repository(&repository);
    let app = fixture.registered(&repository);
    fixture.create_root_parents();

    let plan = fixture.plan(&app, "portable");

    assert!(plan.is_executable());
    assert!(!plan.is_blocked());
    assert!(
        plan.warnings()
            .iter()
            .any(|warning| warning.contains("OpenCode")),
        "{:?}",
        plan.warnings()
    );
}

/// The common case: one directory reached through all three roots is a benign
/// alias, and nothing is said about it.
#[test]
fn installing_to_every_root_at_once_states_no_opencode_concern() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    let app = fixture.registered(&repository);
    fixture.create_root_parents();

    let plan = fixture.plan(&app, "portable");

    assert!(plan.is_executable());
    assert!(plan.warnings().is_empty(), "{:?}", plan.warnings());
}

/// A skill root that is not a directory is not something to create through or
/// write into, and the plan says which of the two it is.
#[test]
fn a_root_that_is_not_a_directory_blocks_its_target() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    let app = fixture.registered(&repository);
    fixture.create_root_parents();
    fs::write(fixture.root(AgentKind::Codex), "not a directory")
        .expect("write a file where the root belongs");

    let plan = fixture.plan(&app, "portable");

    assert_eq!(
        blocking_code(&plan, AgentKind::Codex),
        Some("install.unreadable_root")
    );
    let TargetDisposition::Blocked { finding } =
        plan.target(AgentKind::Codex).unwrap().disposition()
    else {
        unreachable!("just asserted");
    };
    assert!(
        finding.evidence().contains("not a directory"),
        "{}",
        finding.evidence()
    );
}

/// A link Skilled created that now points somewhere else is told apart from one
/// it never made — and neither is repaired, because repair does not exist.
#[test]
fn a_managed_link_pointing_somewhere_else_is_named_as_skilled_s_own() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    let mut app = fixture.registered(&repository);
    fixture.create_root_parents();
    install_through_the_application(&mut app);
    // Something repoints the link Skilled put down.
    let link = fixture.root(AgentKind::Codex).join("portable");
    let elsewhere = fixture.path().join("elsewhere/portable");
    write_skill(&elsewhere, "portable");
    fs::remove_file(&link).expect("remove the link");
    symlink(&elsewhere, &link).expect("point it elsewhere");

    let variant = fixture.variant(&app, "portable");
    let probe = probe_install(app.agents(), app.sources(), &variant);
    let plan = plan_install(
        app.agents(),
        app.sources(),
        &variant,
        [true; 3],
        &probe,
        &app.receipts().expect("receipts"),
    )
    .expect("a plan for a resolvable variant");

    assert_eq!(
        blocking_code(&plan, AgentKind::Codex),
        Some("install.wrong_managed_target")
    );
    let TargetDisposition::Blocked { finding } =
        plan.target(AgentKind::Codex).unwrap().disposition()
    else {
        unreachable!("just asserted");
    };
    assert!(
        finding.evidence().contains("Skilled created"),
        "{}",
        finding.evidence()
    );
}

/// An entry Skilled could not read might be anything, so it blocks rather than
/// being treated as free space.
#[test]
fn an_unreadable_entry_blocks_rather_than_being_treated_as_absent() {
    if running_as_root() {
        // Permission bits decide nothing for the superuser, so the entry this
        // case needs to be unreadable would be readable.
        return;
    }
    let fixture = Fixture::new();
    let repository = fixture.source("library", &["portable"]);
    let app = fixture.registered(&repository);
    fixture.create_root_parents();
    let root = fixture.create_root(AgentKind::Codex);
    fs::create_dir(root.join("portable")).expect("occupy the slot");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o600)).expect("seal the root");

    let plan = fixture.plan(&app, "portable");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).expect("unseal the root");

    assert_eq!(
        blocking_code(&plan, AgentKind::Codex),
        Some("install.unreadable_entry")
    );
}

/// Whether permission bits will be enforced against this process.
fn running_as_root() -> bool {
    let probe = std::env::temp_dir().join(format!("skilled-plan-probe-{}", std::process::id()));
    let _ = fs::remove_dir_all(&probe);
    fs::create_dir(&probe).expect("create permission probe");
    fs::set_permissions(&probe, fs::Permissions::from_mode(0o000)).expect("seal probe");
    let readable = fs::read_dir(&probe).is_ok();
    fs::set_permissions(&probe, fs::Permissions::from_mode(0o755)).expect("unseal probe");
    fs::remove_dir_all(&probe).expect("remove probe");
    readable
}

/// Install the focused variant the way a user would, so the receipts that
/// follow are ones Skilled really wrote.
fn install_through_the_application(app: &mut SkilledApp) {
    for action in [
        skilled::Action::OpenSources,
        skilled::Action::AdvanceSourcesPane,
        skilled::Action::BeginInstall,
        skilled::Action::ConfirmInstall,
        skilled::Action::DismissInstall,
    ] {
        let update = app.update(action);
        app.perform_effects(update.effects()).expect("install");
    }
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

    fn environment(&self) -> AppEnvironment {
        AppEnvironment::new(self.home(), self.directory.path().join("data"), "")
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

    /// Create the directory each documented skill root hangs off, without
    /// creating the root itself.
    ///
    /// This is what an installed agent leaves behind before anyone has put a
    /// global skill anywhere: the plan may create the root itself, but never
    /// the agent's own directory.
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

    fn variant(&self, app: &SkilledApp, skill_name: &str) -> VariantRef {
        locate_variant(app.sources(), app.sources()[0].id(), None, skill_name)
            .expect("one registered variant of that name")
    }

    fn plan(&self, app: &SkilledApp, skill_name: &str) -> InstallPlan {
        let variant = self.variant(app, skill_name);
        self.plan_variant(app, &variant, [true; 3])
    }

    fn plan_variant(
        &self,
        app: &SkilledApp,
        variant: &VariantRef,
        requested: [bool; 3],
    ) -> InstallPlan {
        let probe = probe_install(app.agents(), app.sources(), variant);
        plan_install(app.agents(), app.sources(), variant, requested, &probe, &[])
            .expect("a plan for a resolvable variant")
    }
}

/// Rerun setup and leave OpenCode deselected, through the same steps a user
/// would take.
fn deselect_last_agent(app: &mut SkilledApp) {
    app.update(skilled::Action::OpenSettings);
    let update = app.update(skilled::Action::RerunSetup);
    app.perform_effects(update.effects()).expect("reset setup");
    app.update(skilled::Action::Continue);
    app.update(skilled::Action::MoveSelection(-1));
    app.update(skilled::Action::ToggleSelection);
    for _ in 0..6 {
        let update = app.update(skilled::Action::Continue);
        app.perform_effects(update.effects())
            .expect("setup effects");
    }
    assert!(!app.agent(AgentKind::OpenCode).selected());
}

/// The finding code blocking one agent's target, or `None` where it is not
/// blocked.
fn blocking_code(plan: &InstallPlan, agent: AgentKind) -> Option<&'static str> {
    match plan.target(agent)?.disposition() {
        TargetDisposition::Blocked { finding } => Some(finding.code()),
        _ => None,
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
