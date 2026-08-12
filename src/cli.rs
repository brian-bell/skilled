//! The `skilled install` command.
//!
//! One narrow surface, parsed by hand. Spec 16 asks for a handful of flags and
//! distinguishable exit statuses, and adding a production dependency for that
//! is a decision this slice does not need to make: an argument parser can
//! replace this under review when the surface grows past what one `match`
//! reads well.
//!
//! Everything the command does after parsing is the same code the Sources
//! screen runs — the same plan, the same guards, the same rescan, the same
//! verification. `--yes` skips the confirmation and nothing else.

use std::{
    io::{BufRead, Write},
    path::{Path, PathBuf},
};

use crate::{
    AgentKind, AppEnvironment, SkilledApp, View,
    agents::adapter,
    app::PlanRequestFailure,
    components::terminal_safe,
    operations::{
        AppliedStep, ExcludedReason, InstallOutcome, InstallPlan, InstallStatus, InstallTarget,
        LocateFailure, StepOutcome, TargetDisposition, UninstallDisposition, UninstallOutcome,
        UninstallPlan, UninstallStatus, locate_variant,
    },
};

/// How the command ended.
///
/// The numbers become a contract the first time this ships: a script that
/// distinguishes a blocked plan from a partial apply is doing exactly what
/// spec 16 asks distinguishable statuses to make possible, so they are not
/// renumbered afterwards.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExitCodeKind {
    /// The request was honoured, including a plan the user declined.
    Success,
    /// Something Skilled depends on failed — the metadata store, most likely.
    InternalError,
    /// The request could not be understood, or named no single variant.
    InvalidRequest,
    /// The plan was blocked, so nothing was written.
    Blocked,
    /// The apply stopped before finishing what it planned.
    PartialApply,
    /// Everything was written, and the scan afterwards did not bear it out.
    VerificationFailed,
}

impl ExitCodeKind {
    pub fn code(self) -> u8 {
        match self {
            Self::Success => 0,
            Self::InternalError => 1,
            Self::InvalidRequest => 2,
            Self::Blocked => 3,
            Self::PartialApply => 4,
            Self::VerificationFailed => 5,
        }
    }
}

const USAGE: &str = "\
usage: skilled install --source <id-or-path> --skill <name> \
[--agents claude-code,codex,opencode] [--yes]
       skilled uninstall --skill <name> --agent <agent> [--yes]

  --source   a registered source, by the identifier Skilled gave it or by its
             checkout path
  --skill    the skill directory name to install
  --agents   which agents to install for; defaults to every configured agent
  --yes      skip the confirmation. Requires --source, --skill, and --agents to
             be given explicitly; every safety check still runs.

Uninstall removes only an exact matching Skilled-managed link. Its --agent is
singular and every receipt, object-type, target, containment, and verification
check still runs with --yes.

Run skilled with no arguments for the interactive application.";

/// Run one command, over an injected environment and an injected pair of
/// streams.
///
/// Both streams are parameters so the command can be exercised without a
/// process: what is worth testing is the decision it makes, not the pipe it
/// makes it through.
pub fn run(
    arguments: &[String],
    environment: AppEnvironment,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
) -> ExitCodeKind {
    let parsed = match parse(arguments) {
        Ok(parsed) => parsed,
        Err(message) => return refuse(output, &message),
    };
    let result = match parsed {
        Parsed::Install(request) => execute(&request, environment, input, output),
        Parsed::Uninstall(request) => execute_uninstall(&request, environment, input, output),
        Parsed::Usage => {
            let _ = writeln!(output, "{USAGE}");
            return ExitCodeKind::Success;
        }
    };
    match result {
        Ok(code) => code,
        Err(message) => {
            let _ = writeln!(output, "skilled: {}", safe(&message));
            ExitCodeKind::InternalError
        }
    }
}

/// The status a finished run reports, as an exit code.
///
/// Public and total so the contract can be read and pinned in one place rather
/// than inferred from whichever runs a test could stage. Exit four is "the
/// machine is not in the state the plan described" — a run that stopped part
/// way, one that wrote nothing after being asked to, and one whose links
/// Skilled could not record owning all mean that, and the printed report is
/// what distinguishes them.
///
/// An ancillary OpenCode postcondition Skilled could not establish is *not*
/// one of them. Every link asked for was still observed; what is missing is an
/// effective-resolution check over a root the user asked Skilled to leave
/// alone, which is the ordinary configuration for anyone running fewer than
/// three agents. Reporting that as non-zero would make the common path fail and
/// teach a reader to ignore the status. It is said in words instead —
/// "Verified as far as it could be", then each unestablished check by name —
/// where it can be read rather than only branched on. A written target that was
/// not re-observed is different and exits as a verification failure.
pub fn exit_code_for(status: InstallStatus) -> ExitCodeKind {
    match status {
        InstallStatus::Installed | InstallStatus::NothingToDo => ExitCodeKind::Success,
        InstallStatus::PartiallyApplied
        | InstallStatus::NotApplied
        | InstallStatus::InstalledUnrecorded => ExitCodeKind::PartialApply,
        InstallStatus::VerificationFailed => ExitCodeKind::VerificationFailed,
    }
}

pub fn exit_code_for_uninstall(status: UninstallStatus) -> ExitCodeKind {
    match status {
        UninstallStatus::Uninstalled
        | UninstallStatus::UninstalledUnrecorded
        | UninstallStatus::NothingToDo => ExitCodeKind::Success,
        UninstallStatus::PartiallyApplied | UninstallStatus::NotApplied => {
            ExitCodeKind::PartialApply
        }
        UninstallStatus::VerificationFailed => ExitCodeKind::VerificationFailed,
    }
}

fn refuse(output: &mut dyn Write, message: &str) -> ExitCodeKind {
    let _ = writeln!(output, "skilled: {}\n\n{USAGE}", safe(message));
    ExitCodeKind::InvalidRequest
}

/// What one `install` invocation asked for.
struct InstallRequest {
    source: String,
    skill: String,
    agents: Option<[bool; 3]>,
    assume_yes: bool,
}

struct UninstallRequest {
    skill: String,
    agent: AgentKind,
    assume_yes: bool,
}

enum Parsed {
    Install(InstallRequest),
    Uninstall(UninstallRequest),
    Usage,
}

fn parse(arguments: &[String]) -> Result<Parsed, String> {
    let mut arguments = arguments.iter();
    let command = match arguments.next().map(String::as_str) {
        Some("install") => "install",
        Some("uninstall") => "uninstall",
        Some("--help" | "-h" | "help") => return Ok(Parsed::Usage),
        Some(other) => return Err(format!("unknown command {other}")),
        None => return Err("no command was given".to_owned()),
    };

    let mut source = None;
    let mut skill = None;
    let mut agents = None;
    let mut agent = None;
    let mut assume_yes = false;
    while let Some(flag) = arguments.next() {
        // A value that looks like a flag is a missing value, not a value:
        // taking `--skill` as the source path would run a request nobody wrote.
        let mut value = |flag: &str| match arguments.next() {
            Some(value) if !value.starts_with('-') => Ok(value.clone()),
            _ => Err(format!("{flag} needs a value")),
        };
        match flag.as_str() {
            "--source" => source = Some(value("--source")?),
            "--skill" => skill = Some(value("--skill")?),
            "--agents" => agents = Some(parse_agents(&value("--agents")?)?),
            "--agent" => agent = Some(parse_agent(&value("--agent")?)?),
            "--yes" => assume_yes = true,
            "--help" | "-h" => return Ok(Parsed::Usage),
            other => return Err(format!("unknown option {other}")),
        }
    }

    // `--yes` answers a question the user will not see, so it is fail-closed:
    // every part of what would have been shown has to have been stated. Spec 15
    // asks for the confirmation to be the only thing it removes, and a target
    // set Skilled chose is not a target set the user agreed to.
    if assume_yes && command == "install" {
        for (flag, given) in [
            ("--source", source.is_some()),
            ("--skill", skill.is_some()),
            ("--agents", agents.is_some()),
        ] {
            if !given {
                return Err(format!("--yes requires {flag} to be given explicitly"));
            }
        }
    }

    if command == "uninstall" {
        if source.is_some() || agents.is_some() {
            return Err("uninstall takes --agent, not --source or --agents".to_owned());
        }
        return Ok(Parsed::Uninstall(UninstallRequest {
            skill: skill.ok_or("--skill is required")?,
            agent: agent.ok_or("--agent is required")?,
            assume_yes,
        }));
    }
    if agent.is_some() {
        return Err("install takes --agents, not --agent".to_owned());
    }
    Ok(Parsed::Install(InstallRequest {
        source: source.ok_or("--source is required")?,
        skill: skill.ok_or("--skill is required")?,
        agents,
        assume_yes,
    }))
}

fn parse_agent(value: &str) -> Result<AgentKind, String> {
    if value.contains(',') {
        return Err("--agent takes exactly one agent, not a list".to_owned());
    }
    AgentKind::ALL
        .into_iter()
        .find(|agent| agent_identifier(*agent) == value)
        .ok_or_else(|| {
            format!(
                "unknown agent {value}; --agent takes one of {}",
                AgentKind::ALL.map(agent_identifier).join(", ")
            )
        })
}

fn parse_agents(value: &str) -> Result<[bool; 3], String> {
    let mut selected = [false; 3];
    for name in value
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        let agent = AgentKind::ALL
            .into_iter()
            .find(|agent| agent_identifier(*agent) == name)
            .ok_or_else(|| {
                format!(
                    "unknown agent {name}; --agents takes any of {}",
                    AgentKind::ALL.map(agent_identifier).join(", ")
                )
            })?;
        selected[agent.index()] = true;
    }
    if selected == [false; 3] {
        return Err("--agents named no agent".to_owned());
    }
    Ok(selected)
}

/// The spelling `--agents` takes, derived from the adapter's own executable
/// name so a documented rename moves both together.
fn agent_identifier(agent: AgentKind) -> &'static str {
    match agent {
        AgentKind::ClaudeCode => "claude-code",
        AgentKind::Codex | AgentKind::OpenCode => adapter(agent).executable_name(),
    }
}

fn execute(
    request: &InstallRequest,
    environment: AppEnvironment,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
) -> Result<ExitCodeKind, String> {
    let mut app = SkilledApp::open(environment).map_err(|error| error.to_string())?;
    if request.agents.is_none() && matches!(app.view(), View::Setup(_)) {
        return Ok(refuse(
            output,
            "--agents is required until setup is complete",
        ));
    }
    let Some(source_id) = resolve_source(&app, &request.source) else {
        return Ok(refuse(
            output,
            &format!("no registered source matches {}", request.source),
        ));
    };
    let variant = match locate_variant(app.sources(), source_id, None, &request.skill) {
        Ok(variant) => variant,
        // A name several catalogs in one source answer to is one this command
        // cannot narrow: it takes a source and a name, and nothing finer. The
        // interactive application stands on an exact row, so it is what the
        // message points at rather than a flag that does not exist.
        Err(failure @ LocateFailure::Ambiguous { .. }) => {
            return Ok(refuse(
                output,
                &format!(
                    "{failure}. Run skilled with no arguments and install the variant you want \
                     from the Sources screen"
                ),
            ));
        }
        Err(failure) => return Ok(refuse(output, &failure.to_string())),
    };
    // An agent set that was not given is every agent the user configured, which
    // is what the Sources screen installs for.
    let requested = request
        .agents
        .unwrap_or_else(|| app.agents().each_ref().map(|agent| agent.selected()));

    let plan = match app.plan_install_for(&variant, requested) {
        Ok(plan) => plan,
        Err(PlanRequestFailure::Unplannable(message)) => {
            return Ok(refuse(output, &message));
        }
        // Not a request error: a different request would not fix it, and
        // printing usage would tell the reader to look in the wrong place.
        Err(PlanRequestFailure::Metadata(message)) => return Err(message),
    };
    write_plan(output, &plan, app.home()).map_err(|error| error.to_string())?;

    // An agent the request named and the plan cannot act on is stated as a
    // refusal, not passed over. `--agents` is the target set the user agreed
    // to; installing to fewer of them and reporting success would be the same
    // gap `--yes` is fail-closed against, on the channel a script reads.
    if request.agents.is_some() {
        let unmet: Vec<&InstallTarget> = plan
            .targets()
            .iter()
            .filter(|target| {
                requested[target.agent().index()]
                    && matches!(target.disposition(), TargetDisposition::Excluded { .. })
            })
            .collect();
        if !unmet.is_empty() {
            let _ = writeln!(
                output,
                "\nBlocked: nothing was written. {} could not be installed to, and this request \
                 named {}.",
                unmet
                    .iter()
                    .map(|target| target.agent().display_name())
                    .collect::<Vec<_>>()
                    .join(", "),
                if unmet.len() == 1 { "it" } else { "them" }
            );
            return Ok(ExitCodeKind::Blocked);
        }
    }

    if plan.is_blocked() {
        let _ = writeln!(
            output,
            "\nBlocked: nothing was written. Skilled does not overwrite or repair an existing \
             entry."
        );
        return Ok(ExitCodeKind::Blocked);
    }
    if !plan.is_executable() {
        let _ = writeln!(output, "\nNothing to do.");
        return Ok(ExitCodeKind::Success);
    }
    if !request.assume_yes && !confirmed(input, output)? {
        let _ = writeln!(output, "Cancelled. Nothing was written.");
        return Ok(ExitCodeKind::Success);
    }

    let outcome = app.apply_plan(&plan);
    write_report(output, &outcome).map_err(|error| error.to_string())?;
    Ok(exit_code_for(outcome.status()))
}

fn execute_uninstall(
    request: &UninstallRequest,
    environment: AppEnvironment,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
) -> Result<ExitCodeKind, String> {
    let mut app = SkilledApp::open(environment).map_err(|error| error.to_string())?;
    let mut requested = [false; 3];
    requested[request.agent.index()] = true;
    let plan = app
        .plan_uninstall_for(&request.skill, requested)
        .map_err(|failure| failure.message().to_owned())?;
    write_uninstall_plan(output, &plan).map_err(|error| error.to_string())?;
    let named = plan
        .target(request.agent)
        .expect("every plan has one target per agent");
    if matches!(named.disposition(), UninstallDisposition::Excluded { .. }) {
        let _ = writeln!(
            output,
            "\nBlocked: nothing was removed. The named agent has no matching Skilled-managed link."
        );
        return Ok(ExitCodeKind::Blocked);
    }
    if plan.is_blocked() {
        let _ = writeln!(output, "\nBlocked: nothing was removed.");
        return Ok(ExitCodeKind::Blocked);
    }
    if !plan.is_executable() {
        let _ = writeln!(output, "\nNothing to do.");
        return Ok(ExitCodeKind::Success);
    }
    if !request.assume_yes && !confirmed(input, output)? {
        let _ = writeln!(output, "Cancelled. Nothing was removed.");
        return Ok(ExitCodeKind::Success);
    }
    let outcome = app.apply_uninstall_plan(&plan);
    write_uninstall_report(output, &outcome).map_err(|error| error.to_string())?;
    Ok(exit_code_for_uninstall(outcome.status()))
}

fn write_uninstall_plan(output: &mut dyn Write, plan: &UninstallPlan) -> std::io::Result<()> {
    writeln!(output, "Uninstall {}:", safe(plan.skill_name()))?;
    for target in plan.targets() {
        let verdict = match target.disposition() {
            UninstallDisposition::RemoveLink {
                link_target,
                resolves,
            } => format!(
                "remove managed link to {}{}",
                safe(&link_target.display()),
                if *resolves {
                    ""
                } else {
                    " (target no longer resolves)"
                },
            ),
            UninstallDisposition::Excluded { reason } => format!("excluded: {reason:?}"),
            UninstallDisposition::Blocked { finding } => {
                format!("blocked: {} — {}", finding.code(), safe(finding.evidence()))
            }
        };
        writeln!(output, "  {:<12} {verdict}", target.agent().display_name())?;
        writeln!(
            output,
            "               {}",
            safe(&target.link_path().display())
        )?;
    }
    for warning in plan.warnings() {
        writeln!(output, "\n  warning: {}", safe(warning))?;
    }
    writeln!(
        output,
        "\nSource content and agent skill roots will not be removed."
    )
}

fn write_uninstall_report(
    output: &mut dyn Write,
    outcome: &UninstallOutcome,
) -> std::io::Result<()> {
    writeln!(output)?;
    for step in outcome.applied().steps() {
        let verdict = match step.outcome() {
            StepOutcome::Removed => "link removed".to_owned(),
            StepOutcome::Failed(reason) => format!("not removed — {}", safe(reason)),
            StepOutcome::Unattempted => "not attempted after an earlier failure".to_owned(),
            other => install_step_verdict(other),
        };
        writeln!(output, "  {:<12} {verdict}", step.agent().display_name())?;
        writeln!(
            output,
            "               {}",
            safe(&step.link_path().display())
        )?;
    }
    for withheld in outcome.verification().withheld() {
        writeln!(
            output,
            "Not established: {} — {}",
            withheld.agent().display_name(),
            safe(withheld.reason())
        )?;
    }
    for failure in outcome.verification().failures() {
        writeln!(
            output,
            "Not verified: {} — {}",
            failure.agent().display_name(),
            safe(failure.observed())
        )?;
    }
    for failure in outcome.finalized().failures() {
        writeln!(
            output,
            "Ownership record remains: {} — {}",
            failure.agent().display_name(),
            safe(failure.reason())
        )?;
    }
    writeln!(
        output,
        "Source content and agent skill roots were not removed."
    )
}

/// A source named by the identifier the registry gave it, or by the path its
/// checkout sits at.
///
/// The path is canonicalized before it is compared, so `.`, a relative path,
/// and a path through a symbolic link all name the checkout they resolve to.
fn resolve_source(app: &SkilledApp, named: &str) -> Option<i64> {
    if let Ok(id) = named.parse::<i64>()
        && app.sources().iter().any(|source| source.id() == id)
    {
        return Some(id);
    }
    let path = PathBuf::from(named).canonicalize().ok()?;
    app.sources()
        .iter()
        .find(|source| source.git_top_level() == path)
        .map(|source| source.id())
}

/// Ask, and take anything but a yes as a no.
///
/// A stream that ends without an answer is a no: an unattended run that did not
/// pass `--yes` did not agree to anything.
fn confirmed(input: &mut dyn BufRead, output: &mut dyn Write) -> Result<bool, String> {
    write!(output, "\nProceed? [y/N] ").map_err(|error| error.to_string())?;
    output.flush().map_err(|error| error.to_string())?;
    let mut answer = String::new();
    input
        .read_line(&mut answer)
        .map_err(|error| error.to_string())?;
    let answer = answer.trim().to_lowercase();
    let _ = writeln!(output);
    Ok(answer == "y" || answer == "yes")
}

/// Everything a command prints that came from the filesystem goes through here.
///
/// A checkout directory, a catalog path, a link target, and the text of an
/// operating-system error are all outside Skilled's control, and a terminal
/// would execute a control sequence in any of them rather than show it. The
/// screens escape for the same reason; this is the same escaper, reached from
/// the other surface.
fn safe(value: &(impl std::fmt::Display + ?Sized)) -> String {
    terminal_safe(&value.to_string())
}

fn write_plan(output: &mut dyn Write, plan: &InstallPlan, home: &Path) -> std::io::Result<()> {
    writeln!(output, "Install {}", safe(plan.skill_name()))?;
    writeln!(
        output,
        "  from {} · {}",
        safe(plan.variant().source_label()),
        safe(&plan.variant().catalog_relative_path().display())
    )?;
    writeln!(output, "  links to {}", safe(&plan.source_dir().display()))?;
    writeln!(output, "  home {}", safe(&home.display()))?;
    writeln!(output)?;
    for target in plan.targets() {
        writeln!(
            output,
            "  {:<12} {}",
            target.agent().display_name(),
            target_verdict(target, plan.is_blocked())
        )?;
        writeln!(
            output,
            "               {}",
            safe(&target.link_path().display())
        )?;
    }
    for warning in plan.warnings() {
        writeln!(output, "\n  warning: {}", safe(warning))?;
    }
    Ok(())
}

/// What the plan will do about one target, printed.
///
/// A plan blocks whole, so a target that would have been work is not work: the
/// screen says "would create…" for the same reason, and a printed plan that
/// promised "create the link" three lines above "Blocked: nothing was written"
/// would be contradicting itself in the channel a script reads.
fn target_verdict(target: &InstallTarget, plan_is_blocked: bool) -> String {
    let would = if plan_is_blocked { "would " } else { "" };
    match target.disposition() {
        TargetDisposition::CreateLink => format!("{would}create the link"),
        TargetDisposition::CreateRootAndLink => {
            format!("{would}create the skill root, then the link")
        }
        TargetDisposition::AlreadyInstalled { receipted: true } => {
            "already installed, and Skilled holds a receipt for this path".to_owned()
        }
        TargetDisposition::AlreadyInstalled { receipted: false } => {
            "already in place, and Skilled holds no receipt for it".to_owned()
        }
        TargetDisposition::Excluded { reason } => match reason {
            ExcludedReason::NotConfigured => {
                "excluded: not configured, so Skilled leaves it alone".to_owned()
            }
            ExcludedReason::NotRequested => "excluded: not named by this request".to_owned(),
            ExcludedReason::Incompatible => {
                "excluded: cannot use this variant, so there is nothing to install".to_owned()
            }
            ExcludedReason::AgentSpecificOverride { selected } => format!(
                "excluded: prefers its own edition, {}",
                safe(&selected.evidence_label())
            ),
        },
        TargetDisposition::Blocked { finding } => {
            format!("blocked: {} — {}", finding.code(), safe(finding.evidence()))
        }
    }
}

/// What one applied step did, as one line of the command's report.
///
/// A step's reason carries paths and operating-system error text, which is
/// outside Skilled's control and escaped like everything else that comes from
/// there.
fn install_step_verdict(outcome: &StepOutcome) -> String {
    match outcome {
        StepOutcome::Created => "link created".to_owned(),
        StepOutcome::Removed => "link removed".to_owned(),
        StepOutcome::CreatedUnrecorded(error) => {
            format!(
                "link created, but Skilled could not record owning it: {}",
                safe(error)
            )
        }
        StepOutcome::RootCreatedLinkFailed(error) => {
            format!("skill root created, but the link was not: {}", safe(error))
        }
        StepOutcome::Failed(reason) => format!("not written — {}", safe(reason)),
        StepOutcome::Unattempted => {
            "not attempted, because an earlier step stopped the run".to_owned()
        }
    }
}

fn write_report(output: &mut dyn Write, outcome: &InstallOutcome) -> std::io::Result<()> {
    writeln!(output)?;
    for step in outcome.applied().steps() {
        let verdict = install_step_verdict(step.outcome());
        writeln!(output, "  {:<12} {verdict}", step.agent().display_name())?;
        writeln!(
            output,
            "               {}",
            safe(&step.link_path().display())
        )?;
    }
    writeln!(output)?;
    if outcome.verification().is_complete() {
        writeln!(
            output,
            "Verified: every link written was observed again and matches this plan."
        )?;
    } else if outcome.verification().is_verified() {
        writeln!(
            output,
            "Verified as far as it could be: every link written was observed again, and nothing \
             disagreed with this plan."
        )?;
    }
    for withheld in outcome.verification().withheld() {
        writeln!(
            output,
            "Not established: {} — {}",
            withheld.agent().display_name(),
            safe(withheld.reason())
        )?;
    }
    for failure in outcome.verification().failures() {
        writeln!(
            output,
            "Not verified: {} — {}",
            failure.agent().display_name(),
            safe(failure.observed())
        )?;
    }
    // Only where something was written: there is nothing to say about undoing
    // an operation that wrote nothing.
    if outcome.status() != InstallStatus::Installed
        && outcome
            .applied()
            .steps()
            .iter()
            .any(AppliedStep::changed_filesystem)
    {
        writeln!(
            output,
            "Skilled does not undo a partial install automatically; uninstall is a separate \
             confirmed operation, and no repair exists in this release."
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_residual_root_is_stated_in_the_command_report() {
        assert_eq!(
            install_step_verdict(&StepOutcome::RootCreatedLinkFailed(
                "permission denied".to_owned()
            )),
            "skill root created, but the link was not: permission denied"
        );
    }
}
