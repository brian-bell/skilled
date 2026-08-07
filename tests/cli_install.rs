//! `skilled install`, driven in process.
//!
//! The command is exercised through `cli::run` rather than by spawning a
//! binary: the point of the test is the decision the command makes, and a
//! subprocess would add a build artefact and a temporary `PATH` to every case
//! without proving anything more. Every fixture builds its own temporary home.
#![cfg(unix)]

use std::{
    fs,
    io::Cursor,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

use skilled::{
    Action, AgentKind, AppEnvironment, SkilledApp,
    cli::{self, ExitCodeKind},
    operations::InstallStatus,
};

const ROOTS: [(AgentKind, &str); 3] = [
    (AgentKind::ClaudeCode, ".claude/skills"),
    (AgentKind::Codex, ".agents/skills"),
    (AgentKind::OpenCode, ".config/opencode/skills"),
];

/// The bead's fourth criterion: `--yes` skips the confirmation and nothing
/// else. The plan, the safety checks, the receipts, and the verification all
/// still run.
#[test]
fn yes_installs_after_planning_and_verifying_without_asking() {
    let fixture = Fixture::new();
    let repository = fixture.register("library", &["portable"]);

    let (code, output) = fixture.run(&[
        "install",
        "--yes",
        "--source",
        &repository.display().to_string(),
        "--skill",
        "portable",
        "--agents",
        "claude-code,codex,opencode",
    ]);

    assert_eq!(code, ExitCodeKind::Success, "{output}");
    assert!(!output.contains("Proceed?"), "{output}");
    let variant = repository
        .join("skills/portable")
        .canonicalize()
        .expect("canonical variant directory");
    for (_, root) in ROOTS {
        let link = fixture.home().join(root).join("portable");
        assert_eq!(fs::read_link(&link).expect("read the link"), variant);
    }
    assert_eq!(fixture.app().receipts().expect("receipts").len(), 3);
    assert!(output.contains("Verified: every link written"), "{output}");
    // Every path a user is told about is absolute.
    assert!(
        output.contains(
            &fixture
                .home()
                .join(".claude/skills/portable")
                .display()
                .to_string()
        ),
        "{output}"
    );
}

/// Spec 15: `--yes` is fail-closed. It answers a question the user cannot see,
/// so every part of the target set has to be spelled out before it is accepted.
#[test]
fn yes_requires_every_part_of_the_request_to_be_explicit() {
    let fixture = Fixture::new();
    let repository = fixture.register("library", &["portable"]);
    let source = repository.display().to_string();

    for (missing, arguments) in [
        (
            "--agents",
            vec![
                "install", "--yes", "--source", &source, "--skill", "portable",
            ],
        ),
        (
            "--skill",
            vec![
                "install",
                "--yes",
                "--source",
                &source,
                "--agents",
                "claude-code",
            ],
        ),
        (
            "--source",
            vec![
                "install",
                "--yes",
                "--skill",
                "portable",
                "--agents",
                "claude-code",
            ],
        ),
    ] {
        let (code, output) = fixture.run(&arguments);

        assert_eq!(code, ExitCodeKind::InvalidRequest, "{missing}: {output}");
        assert!(output.contains(missing), "{missing}: {output}");
        assert!(fixture.nothing_installed(), "{missing} wrote something");
    }
}

/// A request that names no single variant is refused before any plan is made,
/// and every candidate is named so the user can choose.
#[test]
fn an_ambiguous_skill_name_is_refused_and_names_its_candidates() {
    let fixture = Fixture::new();
    let repository = fixture.directory.path().join("library");
    write_skill(&repository.join("skills/portable"), "portable");
    write_skill(&repository.join(".claude/skills/portable"), "portable");
    initialize_repository(&repository);
    fixture.register_path(&repository);

    let (code, output) = fixture.run(&[
        "install",
        "--yes",
        "--source",
        &repository.display().to_string(),
        "--skill",
        "portable",
        "--agents",
        "claude-code",
    ]);

    assert_eq!(code, ExitCodeKind::InvalidRequest, "{output}");
    assert!(output.contains("skills/portable"), "{output}");
    assert!(output.contains(".claude/skills/portable"), "{output}");
    assert!(fixture.nothing_installed(), "{output}");
}

/// A blocked plan has its own exit status, prints the finding behind it, and
/// writes nothing — including to the targets that were free.
#[test]
fn a_blocked_plan_exits_distinctly_and_leaves_the_occupant_alone() {
    let fixture = Fixture::new();
    let repository = fixture.register("library", &["portable"]);
    let root = fixture.home().join(".agents/skills");
    fs::create_dir_all(&root).expect("create Codex root");
    fs::write(root.join("portable"), "someone else's file").expect("occupy the slot");

    let (code, output) = fixture.run(&[
        "install",
        "--yes",
        "--source",
        &repository.display().to_string(),
        "--skill",
        "portable",
        "--agents",
        "claude-code,codex,opencode",
    ]);

    assert_eq!(code, ExitCodeKind::Blocked, "{output}");
    assert!(
        output.contains("install.physical_path_collision"),
        "{output}"
    );
    assert_eq!(
        fs::read_to_string(root.join("portable")).expect("the occupant survives"),
        "someone else's file"
    );
    assert!(
        !fixture.home().join(".claude/skills/portable").exists(),
        "{output}"
    );
    assert!(fixture.app().receipts().expect("receipts").is_empty());
}

/// Without `--yes` the plan is printed and the question is asked. Anything but
/// a yes cancels, and cancelling is a success: the user was asked and answered.
#[test]
fn declining_the_prompt_writes_nothing_and_is_not_an_error() {
    let fixture = Fixture::new();
    let repository = fixture.register("library", &["portable"]);
    let arguments = [
        "install",
        "--source",
        &repository.display().to_string(),
        "--skill",
        "portable",
    ];

    let (code, output) = fixture.answer(&arguments, "n\n");

    assert_eq!(code, ExitCodeKind::Success, "{output}");
    assert!(output.contains("Proceed?"), "{output}");
    assert!(output.contains("Cancelled"), "{output}");
    assert!(fixture.nothing_installed(), "{output}");

    let (code, output) = fixture.answer(&arguments, "y\n");

    assert_eq!(code, ExitCodeKind::Success, "{output}");
    assert!(
        fixture.home().join(".claude/skills/portable").is_dir(),
        "{output}"
    );
}

/// A source may be named by the identifier the registry gave it or by the path
/// it sits at, and an unknown one is a request error rather than a crash.
#[test]
fn a_source_is_named_by_identifier_or_path() {
    let fixture = Fixture::new();
    let repository = fixture.register("library", &["portable"]);
    let id = fixture.app().sources()[0].id().to_string();

    let (code, output) = fixture.run(&[
        "install", "--yes", "--source", &id, "--skill", "portable", "--agents", "codex",
    ]);
    assert_eq!(code, ExitCodeKind::Success, "{output}");
    assert!(fixture.home().join(".agents/skills/portable").is_dir());

    let (code, output) = fixture.run(&[
        "install",
        "--yes",
        "--source",
        "/nowhere/at/all",
        "--skill",
        "portable",
        "--agents",
        "codex",
    ]);
    assert_eq!(code, ExitCodeKind::InvalidRequest, "{output}");
    let _ = repository;
}

/// An unknown flag, an unknown agent, and an unknown command are all requests
/// Skilled cannot honour, and none of them touches the filesystem.
#[test]
fn malformed_requests_are_refused_with_usage() {
    let fixture = Fixture::new();
    let repository = fixture.register("library", &["portable"]);
    let source = repository.display().to_string();

    for arguments in [
        vec!["uninstall", "--skill", "portable"],
        vec![
            "install",
            "--yes",
            "--source",
            &source,
            "--skill",
            "portable",
            "--agents",
            "claude-code,emacs",
        ],
        vec![
            "install", "--yes", "--source", &source, "--skill", "portable", "--colour", "always",
        ],
        vec!["install", "--source"],
    ] {
        let (code, output) = fixture.run(&arguments);

        assert_eq!(
            code,
            ExitCodeKind::InvalidRequest,
            "{arguments:?}: {output}"
        );
        assert!(
            output.contains("skilled install"),
            "{arguments:?}: {output}"
        );
        assert!(fixture.nothing_installed(), "{arguments:?} wrote something");
    }
}

/// A postcondition the command could not check is printed as unestablished,
/// and the verified line does not claim more than was checked.
#[test]
fn a_withheld_postcondition_is_printed_rather_than_claimed_as_verified() {
    let fixture = Fixture::new();
    let repository = fixture.register("library", &["portable"]);
    fixture.deselect_codex();

    let (code, output) = fixture.run(&[
        "install",
        "--yes",
        "--source",
        &repository.display().to_string(),
        "--skill",
        "portable",
        "--agents",
        "claude-code,opencode",
    ]);

    assert_eq!(code, ExitCodeKind::Success, "{output}");
    assert!(
        output.contains("Verified as far as it could be"),
        "{output}"
    );
    assert!(!output.contains("Verified: every link written"), "{output}");
    assert!(output.contains("Not established: OpenCode"), "{output}");
    // The two kinds of unknown stay apart: this root was never read.
    assert!(output.contains("did not read Codex"), "{output}");
}

/// An agent the request named but the plan cannot act on is a blocked request,
/// not a silent skip: `--agents` is the target set the user agreed to, and
/// installing to fewer of them while reporting success is the same gap `--yes`
/// is fail-closed against.
#[test]
fn a_named_agent_that_cannot_be_installed_to_blocks_the_request() {
    let fixture = Fixture::new();
    let repository = fixture.directory.path().join("library");
    write_skill(&repository.join(".claude/skills/portable"), "portable");
    initialize_repository(&repository);
    fixture.register_path(&repository);

    let (code, output) = fixture.run(&[
        "install",
        "--yes",
        "--source",
        &repository.display().to_string(),
        "--skill",
        "portable",
        "--agents",
        "claude-code,codex",
    ]);

    assert_eq!(code, ExitCodeKind::Blocked, "{output}");
    assert!(output.contains("Codex"), "{output}");
    assert!(output.contains("cannot use this variant"), "{output}");
    // Blocked means blocked whole: the agent that could have been installed to
    // is left alone as well.
    assert!(!fixture.home().join(".agents/skills/portable").exists());
    assert!(!fixture.home().join(".claude/skills/portable").exists());
}

/// Spec 19: a run that stopped part way through has its own exit status, says
/// which step failed, and states plainly that nothing was undone.
#[test]
fn a_run_that_stops_part_way_through_exits_as_a_partial_apply() {
    if running_as_root() {
        // Permission bits decide nothing for the superuser, so the step this
        // case needs to fail would succeed.
        return;
    }
    let fixture = Fixture::new();
    let repository = fixture.register("library", &["portable"]);
    // OpenCode is the last target, so the two before it are written before the
    // step that cannot be.
    let sealed = fixture.home().join(".config/opencode");
    fs::set_permissions(&sealed, fs::Permissions::from_mode(0o555)).expect("seal the parent");

    let (code, output) = fixture.run(&[
        "install",
        "--yes",
        "--source",
        &repository.display().to_string(),
        "--skill",
        "portable",
        "--agents",
        "claude-code,codex,opencode",
    ]);
    fs::set_permissions(&sealed, fs::Permissions::from_mode(0o755)).expect("unseal the parent");

    assert_eq!(code, ExitCodeKind::PartialApply, "{output}");
    assert!(output.contains("not written"), "{output}");
    assert!(output.contains("does not undo"), "{output}");
    // The links written before the failure are real, and are left alone.
    assert!(
        fixture.home().join(".claude/skills/portable").is_dir(),
        "{output}"
    );
    assert!(
        fixture.home().join(".agents/skills/portable").is_dir(),
        "{output}"
    );
    assert!(
        !fixture.home().join(".config/opencode/skills").exists(),
        "{output}"
    );
    assert_eq!(fixture.app().receipts().expect("receipts").len(), 2);
}

/// Spec 16 asks for distinguishable statuses, so every status has one and the
/// whole mapping is stated in one place rather than inferred from whichever
/// runs a test can stage.
#[test]
fn every_outcome_has_its_own_exit_status() {
    for (status, expected, code) in [
        (InstallStatus::Installed, ExitCodeKind::Success, 0),
        (InstallStatus::NothingToDo, ExitCodeKind::Success, 0),
        (
            InstallStatus::PartiallyApplied,
            ExitCodeKind::PartialApply,
            4,
        ),
        (InstallStatus::NotApplied, ExitCodeKind::PartialApply, 4),
        (
            InstallStatus::InstalledUnrecorded,
            ExitCodeKind::PartialApply,
            4,
        ),
        (
            InstallStatus::VerificationFailed,
            ExitCodeKind::VerificationFailed,
            5,
        ),
    ] {
        assert_eq!(cli::exit_code_for(status), expected, "{status:?}");
        assert_eq!(expected.code(), code, "{status:?}");
    }
    // The statuses no outcome carries, pinned beside them: a blocked plan never
    // reaches an apply, and an internal failure is not an outcome at all.
    assert_eq!(ExitCodeKind::InvalidRequest.code(), 2);
    assert_eq!(ExitCodeKind::Blocked.code(), 3);
    assert_eq!(ExitCodeKind::InternalError.code(), 1);
}

/// A flag that swallowed the next flag as its value would run a request nobody
/// wrote.
#[test]
fn a_missing_flag_value_is_not_taken_from_the_next_flag() {
    let fixture = Fixture::new();
    fixture.register("library", &["portable"]);

    let (code, output) = fixture.run(&["install", "--source", "--skill", "portable"]);

    assert_eq!(code, ExitCodeKind::InvalidRequest, "{output}");
    assert!(output.contains("--source needs a value"), "{output}");
    assert!(fixture.nothing_installed());
}

/// Whether permission bits will be enforced against this process.
fn running_as_root() -> bool {
    let probe = std::env::temp_dir().join(format!("skilled-cli-probe-{}", std::process::id()));
    let _ = fs::remove_dir_all(&probe);
    fs::create_dir(&probe).expect("create permission probe");
    fs::set_permissions(&probe, fs::Permissions::from_mode(0o000)).expect("seal probe");
    let readable = fs::read_dir(&probe).is_ok();
    fs::set_permissions(&probe, fs::Permissions::from_mode(0o755)).expect("unseal probe");
    fs::remove_dir_all(&probe).expect("remove probe");
    readable
}

struct Fixture {
    directory: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let fixture = Self {
            directory: tempfile::tempdir().expect("temporary application directory"),
        };
        for root in [".claude", ".agents", ".config/opencode"] {
            fs::create_dir_all(fixture.home().join(root)).expect("create the root's parent");
        }
        fixture
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

    /// A registered source holding the named skills, with setup complete.
    fn register(&self, name: &str, skills: &[&str]) -> PathBuf {
        let repository = self.directory.path().join(name);
        for skill in skills {
            write_skill(&repository.join("skills").join(skill), skill);
        }
        initialize_repository(&repository);
        self.register_path(&repository);
        repository
    }

    fn register_path(&self, repository: &Path) {
        let mut app = self.app();
        let preview = app.preview_source(repository).expect("preview source");
        app.confirm_source(preview).expect("register source");
        for _ in 0..7 {
            let update = app.update(Action::Continue);
            app.perform_effects(update.effects())
                .expect("perform setup effects");
        }
    }

    /// Rerun setup and leave Codex deselected, through the steps a user takes.
    fn deselect_codex(&self) {
        let mut app = self.app();
        for action in [
            Action::OpenSettings,
            Action::RerunSetup,
            Action::Continue,
            Action::MoveSelection(1),
            Action::ToggleSelection,
        ] {
            let update = app.update(action);
            app.perform_effects(update.effects())
                .expect("setup effects");
        }
        for _ in 0..6 {
            let update = app.update(Action::Continue);
            app.perform_effects(update.effects())
                .expect("setup effects");
        }
        assert!(!app.agent(AgentKind::Codex).selected());
    }

    fn run(&self, arguments: &[&str]) -> (ExitCodeKind, String) {
        self.answer(arguments, "")
    }

    fn answer(&self, arguments: &[&str], reply: &str) -> (ExitCodeKind, String) {
        let arguments: Vec<String> = arguments.iter().map(|value| (*value).to_owned()).collect();
        let mut input = Cursor::new(reply.as_bytes().to_vec());
        let mut output: Vec<u8> = Vec::new();
        let code = cli::run(&arguments, self.environment(), &mut input, &mut output);
        (code, String::from_utf8(output).expect("valid UTF-8 output"))
    }

    fn nothing_installed(&self) -> bool {
        ROOTS
            .iter()
            .all(|(_, root)| !self.home().join(root).join("portable").exists())
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
