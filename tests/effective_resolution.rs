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
    inventory::{Finding, FindingSeverity, InventoryRow, RowVerdict},
    resolution::{CandidateSelection, OpenCodeResolution, UnknownCause},
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

/// A compatibility declaration and an edition owner answer different
/// questions. A common catalog with OpenCode unchecked is not another agent's
/// edition, but Skilled still must not present it as a healthy OpenCode choice.
#[test]
fn an_incompatible_common_variant_is_not_foreign_or_healthy() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &[("skills", &["review"])]);
    fixture.install_symlink(
        AgentKind::ClaudeCode,
        "review",
        &repository.join("skills/review"),
    );
    let app = fixture.registered_without_opencode_compatibility(&repository);
    let row = app.inventory().row("review").expect("review row");

    assert!(matches!(
        row.opencode_resolution(),
        Some(OpenCodeResolution::IncompatibleExposure { .. })
    ));
    assert_eq!(row.verdict(), RowVerdict::IncompatibleVariant);
    let finding = resolution_finding(row, "variant.incompatible_for_opencode");
    assert_eq!(finding.severity(), FindingSeverity::Warning);
    assert!(
        finding.evidence().contains("not registered for OpenCode"),
        "the evidence states the compatibility declaration: {finding:?}"
    );
    assert!(
        !finding.evidence().contains("another agent's edition"),
        "incompatibility is not misreported as foreignness: {finding:?}"
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
    assert_eq!(
        roots.iter().map(|root| root.root()).collect::<Vec<_>>(),
        [AgentKind::ClaudeCode]
    );
    // The root was never read, which is not the same as an entry in it that
    // could not be followed.
    assert_eq!(roots[0].cause(), UnknownCause::RootNotRead);
    assert!(
        row.resolution_findings().is_empty(),
        "no classification is stated over a root that was not read"
    );
}

/// Stray content is not an installation, so a registry ambiguity over a name a
/// root merely holds a file under stays uncertain rather than becoming
/// critical: the agent is resolving nothing.
#[test]
fn stray_content_does_not_escalate_a_selection_conflict() {
    let fixture = Fixture::new();
    let first = fixture.source("alpha", &[("skills", &["review"])]);
    let second = fixture.source("beta", &[("skills", &["review"])]);
    drop(fixture.registered(&[&first, &second]));
    let root = fixture.root(AgentKind::ClaudeCode);
    fs::create_dir_all(&root).expect("create Claude Code root");
    fs::write(root.join("review"), "not a directory").expect("write stray content");

    let app = fixture.app();
    let claude = app
        .inventory()
        .selection_findings()
        .iter()
        .find(|finding| finding.agent() == AgentKind::ClaudeCode)
        .expect("Claude Code has no unambiguous variant")
        .finding()
        .severity();

    assert_eq!(claude, FindingSeverity::Warning);
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

/// An installation in a compatibility root is installed for OpenCode too:
/// OpenCode resolves that root even though its own native slot is empty.
#[test]
fn an_opencode_selection_conflict_installed_through_a_compatibility_root_is_critical() {
    let fixture = Fixture::new();
    let first = fixture.source("alpha", &[("skills", &["review"])]);
    let second = fixture.source("beta", &[("skills", &["review"])]);
    drop(fixture.registered(&[&first, &second]));
    fixture.install_symlink(AgentKind::Codex, "review", &first.join("skills/review"));

    let app = fixture.app();
    let row = app.inventory().row("review").expect("review row");
    assert!(matches!(
        row.opencode_resolution(),
        Some(OpenCodeResolution::Selected { .. })
    ));

    let opencode = app
        .inventory()
        .selection_findings()
        .iter()
        .find(|finding| finding.agent() == AgentKind::OpenCode)
        .expect("OpenCode has no unambiguous registered variant");
    assert_eq!(opencode.finding().severity(), FindingSeverity::Critical);
    assert!(
        opencode.finding().evidence().contains("ambiguous now"),
        "the evidence states that the installed name is affected: {:?}",
        opencode.finding()
    );
}

/// Installed-root precedence and source selection answer different questions.
/// When both are ambiguous for OpenCode, Doctor retains each fact: the runtime
/// conflict names the roots, and the registry conflict names every candidate.
#[test]
fn an_opencode_root_conflict_retains_the_registry_ambiguity() {
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
    let conflicts = app
        .inventory()
        .doctor_findings()
        .filter(|entry| {
            entry.agent() == AgentKind::OpenCode
                && entry.finding().code() == "variant.duplicate_for_agent"
        })
        .collect::<Vec<_>>();

    assert_eq!(
        conflicts.len(),
        2,
        "runtime and registry conflicts remain distinct"
    );
    let runtime = conflicts
        .iter()
        .find(|entry| !entry.concerns_the_registry())
        .expect("effective-resolution conflict");
    assert!(
        runtime.finding().evidence().contains(OPENCODE_ROOT)
            && runtime.finding().evidence().contains(CLAUDE_CODE_ROOT),
        "the runtime conflict names every installed root: {runtime:?}"
    );
    let registry = conflicts
        .iter()
        .find(|entry| entry.concerns_the_registry())
        .expect("registry selection conflict");
    assert_eq!(
        registry
            .variants()
            .iter()
            .map(|variant| variant.source_label().to_owned())
            .collect::<Vec<_>>(),
        ["alpha", "beta"]
    );
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

/// A root OpenCode consults that could not be read leaves the resolution
/// incomplete for exactly the same reason a deselected one does, and names the
/// state it was in rather than flattening every gap into one word.
#[test]
fn an_unreadable_compatibility_root_leaves_the_resolution_incomplete() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &[("skills", &["review"])]);
    drop(fixture.registered(&[&repository]));
    fixture.install_symlink(
        AgentKind::OpenCode,
        "review",
        &repository.join("skills/review"),
    );
    // A file where the Codex root belongs is a root that cannot be read.
    fs::create_dir_all(fixture.home().join(".agents")).expect("create the root's parent");
    fs::write(fixture.root(AgentKind::Codex), "not a directory")
        .expect("block the root with a file");

    let app = fixture.app();
    let row = app.inventory().row("review").expect("review row");

    let Some(OpenCodeResolution::Incomplete { roots }) = row.opencode_resolution() else {
        panic!("a root that could not be read cannot be classified over");
    };
    assert_eq!(
        roots.iter().map(|root| root.root()).collect::<Vec<_>>(),
        [AgentKind::Codex]
    );
    assert_eq!(roots[0].cause(), UnknownCause::RootNotRead);
    assert!(row.resolution_findings().is_empty());
    // A count covers the roots it was asked to look at, and one of them
    // contributed nothing, so no number may be stated for either list.
    assert_eq!(app.inventory().stated_finding_count(), None);
    assert_eq!(app.inventory().stated_skill_count(), None);
}

/// OpenCode deselected is OpenCode left alone: no effective resolution is
/// computed for it at all, rather than one computed and then hidden.
#[test]
fn a_deselected_opencode_is_asked_nothing() {
    let fixture = Fixture::new();
    let first = fixture.source("alpha", &[("skills", &["review"])]);
    let second = fixture.source("beta", &[("skills", &["review"])]);
    drop(fixture.registered_with(&[&first, &second], [true, true, false]));
    fixture.install_symlink(
        AgentKind::ClaudeCode,
        "review",
        &first.join("skills/review"),
    );
    fixture.install_symlink(AgentKind::Codex, "review", &second.join("skills/review"));

    let app = fixture.app();
    let row = app.inventory().row("review").expect("review row");

    assert!(row.opencode_resolution().is_none());
    assert!(row.resolution_findings().is_empty());
    assert_eq!(row.verdict(), RowVerdict::Healthy);
    // Nor is a selection conflict raised for an agent Skilled was asked to
    // leave alone.
    assert!(
        app.inventory()
            .selection_findings()
            .iter()
            .all(|finding| finding.agent() != AgentKind::OpenCode)
    );
}

/// Sameness is canonical-path equality rather than byte comparison, so two
/// physical copies of identical content are two variants and conflict. The
/// rule is deliberate and surprising, so it is pinned rather than left to be
/// discovered.
#[test]
fn two_physical_copies_of_the_same_content_still_conflict() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &[("skills", &["other"])]);
    drop(fixture.registered(&[&repository]));
    for agent in [AgentKind::OpenCode, AgentKind::ClaudeCode] {
        write_skill(&fixture.root(agent).join("review"), "review");
    }

    let app = fixture.app();
    let row = app.inventory().row("review").expect("review row");

    let Some(OpenCodeResolution::Conflict { entries }) = row.opencode_resolution() else {
        panic!("two directories are two variants, whatever their bytes say");
    };
    assert_eq!(entries.len(), 2);
    assert_eq!(row.verdict(), RowVerdict::Conflict);
}

/// A variant that does not validate is not a candidate, so it cannot compete:
/// an agent could not load it, and reporting a conflict against it would block
/// a choice that was never ambiguous.
#[test]
fn a_variant_that_does_not_validate_is_not_a_candidate() {
    let fixture = Fixture::new();
    let first = fixture.source("alpha", &[("skills", &["review"])]);
    let second = fixture.source("beta", &[("skills", &["review"])]);
    // Break the second repository's copy before it is registered, so the
    // catalog records the validation failure the scanner found.
    fs::write(
        second.join("skills/review/SKILL.md"),
        "no frontmatter here\n",
    )
    .expect("break the variant");
    drop(fixture.registered(&[&first, &second]));

    let app = fixture.app();

    let CandidateSelection::Selected(variant) =
        skilled::resolution::select_candidates(app.sources(), AgentKind::ClaudeCode, "review")
    else {
        panic!("only the variant that validates is a candidate");
    };
    assert_eq!(variant.source_label(), "alpha");
    assert!(app.inventory().selection_findings().is_empty());
}

/// Exposure is about what OpenCode can use, not about which root the content
/// was reached through: another agent's variant sitting in OpenCode's own root
/// is the arrangement Skilled must least of all call healthy.
#[test]
fn a_foreign_variant_in_opencodes_own_root_is_exposure_too() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &[("claude/skills", &["review"])]);
    drop(fixture.registered(&[&repository]));
    fixture.install_symlink(
        AgentKind::OpenCode,
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
    assert_eq!(
        resolution_finding(row, "variant.foreign_opencode_exposure").severity(),
        FindingSeverity::Warning
    );
}

/// Content an agent cannot load is not what any agent resolves a name to, so
/// it offers nothing to another agent's effective resolution — the same
/// exclusion the registry side applies to a variant that does not validate.
#[test]
fn an_installation_that_cannot_be_loaded_offers_nothing_to_resolution() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &[("skills", &["review"])]);
    drop(fixture.registered(&[&repository]));
    fixture.install_symlink(
        AgentKind::ClaudeCode,
        "review",
        &repository.join("skills/review"),
    );
    // A directory under the same name in OpenCode's own root whose SKILL.md
    // fails the portable core: OpenCode would not load it.
    let broken = fixture.root(AgentKind::OpenCode).join("review");
    fs::create_dir_all(&broken).expect("create the broken installation");
    fs::write(broken.join("SKILL.md"), "no frontmatter here\n").expect("break it");

    let app = fixture.app();
    let row = app.inventory().row("review").expect("review row");

    let Some(OpenCodeResolution::Selected { winner, aliases }) = row.opencode_resolution() else {
        panic!("only one of the two is loadable, so there is nothing to conflict");
    };
    assert_eq!(winner.root(), AgentKind::ClaudeCode);
    assert!(aliases.is_empty());
    assert!(
        row.resolution_findings().is_empty(),
        "no conflict is manufactured out of content no agent can load"
    );
    // The broken installation is still reported as broken on its own account.
    assert_eq!(row.verdict(), RowVerdict::Broken);
}

/// A registry read in part may hold the very variant that would have made a
/// name ambiguous, so no finding total is stated over it — and the empty list
/// says which kind of gap it is rather than reporting a clean bill of health.
#[test]
fn a_source_that_could_not_be_read_withholds_the_finding_count() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &[("skills", &["review"])]);
    drop(fixture.registered(&[&repository]));
    fs::create_dir_all(fixture.root(AgentKind::ClaudeCode)).expect("create a root that was read");
    fs::rename(&repository, fixture.home().join("moved-away")).expect("move the checkout away");

    let app = fixture.app();
    let inventory = app.inventory();

    assert!(!inventory.registry_is_complete());
    assert_eq!(inventory.stated_finding_count(), None);
    // The roots themselves were read in full, so their own count survives.
    assert_eq!(inventory.stated_skill_count(), Some(0));
}

/// The count and the list are two derivations of one thing, and the count
/// exists only to avoid the list's sort. They may never disagree.
#[test]
fn the_finding_count_matches_the_findings_listed() {
    let fixture = Fixture::new();
    let first = fixture.source(
        "alpha",
        &[
            ("skills", &["review", "shared"]),
            ("claude/skills", &["exposed"]),
        ],
    );
    let second = fixture.source("beta", &[("skills", &["review"])]);
    drop(fixture.registered(&[&first, &second]));
    fixture.install_symlink(AgentKind::OpenCode, "review", &first.join("skills/review"));
    fixture.install_symlink(
        AgentKind::ClaudeCode,
        "review",
        &second.join("skills/review"),
    );
    for agent in [AgentKind::ClaudeCode, AgentKind::Codex] {
        fixture.install_symlink(agent, "shared", &first.join("skills/shared"));
    }
    fixture.install_symlink(
        AgentKind::ClaudeCode,
        "exposed",
        &first.join("claude/skills/exposed"),
    );
    fixture.install_symlink(
        AgentKind::ClaudeCode,
        "dangling",
        &fixture.home().join("nowhere"),
    );

    let inventory = fixture.app();
    let inventory = inventory.inventory();

    assert!(inventory.finding_count() > 4, "the fixture must be rich");
    assert_eq!(
        inventory.finding_count(),
        inventory.doctor_findings().count()
    );
}

/// The layout spec 5.1 is written about: a repository vendoring at
/// `.claude/skills`, installed into Claude Code's root, seen by OpenCode
/// through a compatibility root. OpenCode discovers that root, which is why
/// registration proposes the catalog as OpenCode-compatible — but the edition
/// in it is Claude Code's, and Skilled must not claim OpenCode can use it.
#[test]
fn a_dot_prefixed_claude_catalog_is_still_a_foreign_variant_for_opencode() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &[(".claude/skills", &["review"])]);
    drop(fixture.registered(&[&repository]));
    fixture.install_symlink(
        AgentKind::ClaudeCode,
        "review",
        &repository.join(".claude/skills/review"),
    );

    let app = fixture.app();
    let row = app.inventory().row("review").expect("review row");

    assert!(
        matches!(
            row.opencode_resolution(),
            Some(OpenCodeResolution::ForeignExposure { .. })
        ),
        "{:?}",
        row.opencode_resolution()
    );
    assert_eq!(row.verdict(), RowVerdict::ForeignVariant);
    // The evidence says what was observed — that this is another agent's
    // edition — rather than resting on a stored compatibility set that, for
    // this layout, records that OpenCode reads the root.
    let evidence = resolution_finding(row, "variant.foreign_opencode_exposure").evidence();
    assert!(evidence.contains("another agent's edition"), "{evidence}");
    assert!(
        !evidence.contains("not registered for OpenCode"),
        "the Sources screen registers this catalog for OpenCode: {evidence}"
    );
    // Claude Code's own selection is unaffected: this is its edition.
    assert!(matches!(
        skilled::resolution::select_candidates(app.sources(), AgentKind::ClaudeCode, "review"),
        CandidateSelection::Selected(_)
    ));
}

/// The same conflation, on the selection side. A portable edition beside a
/// Claude Code one must be what OpenCode resolves to, and two editions written
/// for two other agents must not read as a choice OpenCode has to make.
#[test]
fn opencode_selects_the_portable_edition_over_another_agents() {
    let fixture = Fixture::new();
    let both = fixture.source(
        "both",
        &[("skills", &["review"]), (".claude/skills", &["review"])],
    );
    drop(fixture.registered(&[&both]));

    let app = fixture.app();

    let CandidateSelection::Selected(variant) =
        skilled::resolution::select_candidates(app.sources(), AgentKind::OpenCode, "review")
    else {
        panic!("the portable edition is the only one OpenCode can use");
    };
    assert_eq!(variant.catalog_relative_path(), Path::new("skills"));
    // And Claude Code still prefers the edition written for it.
    let CandidateSelection::Selected(variant) =
        skilled::resolution::select_candidates(app.sources(), AgentKind::ClaudeCode, "review")
    else {
        panic!("Claude Code has an exact edition");
    };
    assert_eq!(variant.catalog_relative_path(), Path::new(".claude/skills"));
}

/// An ordinary dual-edition repository is not a conflict for the agent that
/// merely reads both roots.
#[test]
fn two_other_agents_editions_are_not_a_choice_opencode_has_to_make() {
    let fixture = Fixture::new();
    let repository = fixture.source(
        "library",
        &[
            (".claude/skills", &["review"]),
            (".agents/skills", &["review"]),
        ],
    );
    drop(fixture.registered(&[&repository]));

    let app = fixture.app();

    assert_eq!(
        skilled::resolution::select_candidates(app.sources(), AgentKind::OpenCode, "review"),
        CandidateSelection::NoCandidate
    );
    assert!(
        app.inventory()
            .selection_findings()
            .iter()
            .all(|finding| finding.agent() != AgentKind::OpenCode),
        "no duplicate is raised for an agent with no candidate at all"
    );
}

/// An entry a fully-read root holds but the scan could not follow is not the
/// root being unread, and the two are never reported as one another.
#[test]
fn an_entry_that_could_not_be_followed_is_not_an_unread_root() {
    let fixture = Fixture::new();
    let repository = fixture.source("library", &[("skills", &["review"])]);
    drop(fixture.registered(&[&repository]));
    fixture.install_symlink(
        AgentKind::OpenCode,
        "review",
        &repository.join("skills/review"),
    );
    // A pair of links pointing at each other: the root reads in full, but this
    // entry cannot be resolved.
    let claude = fixture.root(AgentKind::ClaudeCode);
    fs::create_dir_all(&claude).expect("create Claude Code root");
    symlink(claude.join("looping"), claude.join("review")).expect("install the first link");
    symlink(claude.join("review"), claude.join("looping")).expect("install the second link");

    let app = fixture.app();
    let row = app.inventory().row("review").expect("review row");

    let Some(OpenCodeResolution::Incomplete { roots }) = row.opencode_resolution() else {
        panic!("an entry that could not be followed leaves the answer unknown");
    };
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].root(), AgentKind::ClaudeCode);
    assert_eq!(roots[0].cause(), UnknownCause::EntryUnresolved);
    // The root itself was read in full, and says so.
    assert!(matches!(
        app.inventory().root(AgentKind::ClaudeCode).status(),
        skilled::inventory::RootStatus::Scanned { .. }
    ));
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

    /// Complete setup through the public source-confirmation flow after
    /// explicitly removing OpenCode from a common catalog's compatibility set.
    fn registered_without_opencode_compatibility(&self, repository: &Path) -> SkilledApp {
        let mut app = self.app();
        for _ in 0..3 {
            self.advance(&mut app);
        }
        app.update(Action::BeginAddSource);
        for character in repository.to_string_lossy().chars() {
            app.update(Action::AppendSourcePath(character));
        }
        let update = app.update(Action::SubmitSourcePath);
        app.perform_effects(update.effects())
            .expect("inspect source");
        app.update(Action::ToggleCatalogCompatibility(AgentKind::OpenCode));
        self.advance(&mut app);
        self.advance(&mut app);
        self.advance(&mut app);
        app
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

    /// Every entry of every agent root, with what it points at: a scan must
    /// disturb neither the entries nor their targets, and comparing paths
    /// alone would not notice a link that was repointed in place.
    fn root_contents(&self) -> BTreeSet<(PathBuf, PathBuf)> {
        AgentKind::ALL
            .into_iter()
            .flat_map(|agent| fs::read_dir(self.root(agent)).into_iter().flatten())
            .map(|entry| {
                let path = entry.expect("read a root entry").path();
                let target = fs::read_link(&path).unwrap_or_else(|_| path.clone());
                (path, target)
            })
            .collect()
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
