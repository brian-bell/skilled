use std::fs;

use skilled::source::{CatalogClassification, propose_catalogs};

#[test]
fn supported_catalog_shapes_enumerate_only_immediate_skill_children() {
    let temporary = tempfile::tempdir().expect("temporary source repository");
    let repository = temporary.path();
    write_skill(repository.join("skills/portable/SKILL.md"), "portable");
    fs::create_dir_all(repository.join("skills/broken")).expect("create invalid candidate");
    fs::write(repository.join("skills/broken/skill.md"), "not portable")
        .expect("write invalid candidate");
    write_skill(
        repository.join("skills/portable/references/example/SKILL.md"),
        "example",
    );
    write_skill(
        repository.join("catalogs/experimental/claude-code/skills/review/SKILL.md"),
        "review",
    );

    let catalogs = propose_catalogs(repository).expect("detect catalog roots");

    assert_eq!(catalogs.len(), 2);
    assert_eq!(
        catalogs[0].relative_path().to_string_lossy(),
        "catalogs/experimental/claude-code/skills"
    );
    assert_eq!(
        catalogs[0].classification(),
        CatalogClassification::AgentSpecific
    );
    assert!(catalogs[0].compatibility().claude_code());
    assert!(!catalogs[0].compatibility().codex());
    assert_eq!(catalogs[0].candidates().len(), 1);
    assert_eq!(catalogs[0].candidates()[0].directory_name(), "review");

    assert_eq!(catalogs[1].relative_path().to_string_lossy(), "skills");
    assert_eq!(catalogs[1].classification(), CatalogClassification::Common);
    assert!(catalogs[1].compatibility().all_supported());
    assert_eq!(catalogs[1].candidates().len(), 2);
    assert_eq!(catalogs[1].candidates()[0].directory_name(), "broken");
    assert!(!catalogs[1].candidates()[0].validation().is_valid());
    assert_eq!(catalogs[1].candidates()[1].directory_name(), "portable");
    assert!(catalogs[1].candidates()[1].validation().is_valid());
}

#[test]
fn an_exact_skill_filename_is_proposed_even_when_portable_metadata_is_invalid() {
    let temporary = tempfile::tempdir().expect("temporary source repository");
    let repository = temporary.path();
    fs::create_dir_all(repository.join("skills/broken")).expect("create invalid skill");
    fs::write(
        repository.join("skills/broken/SKILL.md"),
        "---\nname: [invalid\n---\nbody\n",
    )
    .expect("write invalid skill");

    let catalogs = propose_catalogs(repository).expect("detect common catalog");

    assert_eq!(catalogs.len(), 1);
    assert_eq!(catalogs[0].candidates().len(), 1);
    assert!(!catalogs[0].candidates()[0].validation().is_valid());
}

#[test]
fn recognized_catalogs_remain_visible_when_empty_or_entirely_invalid() {
    let temporary = tempfile::tempdir().expect("temporary source repository");
    let repository = temporary.path();
    fs::create_dir_all(repository.join("skills/wrong-case")).expect("create invalid candidate");
    fs::write(
        repository.join("skills/wrong-case/skill.md"),
        "wrong filename",
    )
    .expect("write invalid candidate");
    fs::create_dir_all(repository.join(".agents/skills")).expect("create empty catalog");

    let catalogs = propose_catalogs(repository).expect("scan recognized catalogs");

    assert_eq!(catalogs.len(), 2);
    assert!(catalogs[0].candidates().is_empty());
    assert_eq!(catalogs[1].candidates().len(), 1);
    assert!(!catalogs[1].candidates()[0].validation().is_valid());
}

#[test]
fn catalog_candidate_enumeration_has_a_hard_limit() {
    let temporary = tempfile::tempdir().expect("temporary source repository");
    let repository = temporary.path();
    for index in 0..4_097 {
        fs::create_dir_all(repository.join("skills").join(format!("candidate-{index}")))
            .expect("create candidate directory");
    }

    let result = propose_catalogs(repository);

    assert!(matches!(
        result,
        Err(skilled::Error::SourceScanLimitExceeded)
    ));
}

#[test]
fn crowded_skill_directories_are_bounded_invalid_candidates() {
    let temporary = tempfile::tempdir().expect("temporary source repository");
    let repository = temporary.path();
    write_skill(repository.join("skills/valid/SKILL.md"), "valid");
    let crowded = repository.join("skills/a-crowded");
    fs::create_dir_all(&crowded).expect("create crowded candidate");
    for index in 0..4_097 {
        fs::write(crowded.join(format!("file-{index}")), "fixture")
            .expect("write crowded candidate entry");
    }

    let catalogs = propose_catalogs(repository).expect("scan bounded crowded candidate");

    assert_eq!(catalogs.len(), 1);
    let crowded = catalogs[0]
        .candidates()
        .iter()
        .find(|candidate| candidate.directory_name() == "a-crowded")
        .expect("crowded candidate");
    assert!(!crowded.validation().is_valid());
    assert!(
        crowded
            .validation()
            .message()
            .is_some_and(|message| message.contains("entry inspection limit"))
    );
}

#[test]
fn a_bounded_indeterminate_candidate_keeps_its_catalog_visible() {
    let temporary = tempfile::tempdir().expect("temporary source repository");
    let repository = temporary.path();
    let crowded = repository.join("skills/crowded");
    fs::create_dir_all(&crowded).expect("create crowded candidate");
    for index in 0..4_097 {
        fs::write(crowded.join(format!("file-{index}")), "fixture")
            .expect("write crowded candidate entry");
    }

    let catalogs = propose_catalogs(repository).expect("scan bounded crowded catalog");

    assert_eq!(catalogs.len(), 1);
    assert_eq!(catalogs[0].candidates().len(), 1);
    assert!(!catalogs[0].candidates()[0].validation().is_valid());
}

#[test]
fn a_large_valid_catalog_is_not_counted_twice() {
    let temporary = tempfile::tempdir().expect("temporary source repository");
    let repository = temporary.path();
    for index in 0..2_050 {
        let name = format!("candidate-{index}");
        write_skill(
            repository.join("skills").join(&name).join("SKILL.md"),
            &name,
        );
    }

    let catalogs = propose_catalogs(repository).expect("scan large valid catalog");

    assert_eq!(catalogs.len(), 1);
    assert_eq!(catalogs[0].candidates().len(), 2_050);
}

#[test]
fn shared_agent_roots_include_opencode_in_their_defaults() {
    let temporary = tempfile::tempdir().expect("temporary source repository");
    let repository = temporary.path();
    write_skill(
        repository.join(".agents/skills/codex-shared/SKILL.md"),
        "codex-shared",
    );
    write_skill(
        repository.join(".claude/skills/claude-shared/SKILL.md"),
        "claude-shared",
    );

    let catalogs = propose_catalogs(repository).expect("scan shared agent roots");
    let agents = catalogs
        .iter()
        .find(|catalog| catalog.relative_path() == std::path::Path::new(".agents/skills"))
        .expect(".agents catalog");
    assert!(agents.compatibility().codex());
    assert!(agents.compatibility().opencode());
    let claude = catalogs
        .iter()
        .find(|catalog| catalog.relative_path() == std::path::Path::new(".claude/skills"))
        .expect(".claude catalog");
    assert!(claude.compatibility().claude_code());
    assert!(claude.compatibility().opencode());
}

#[test]
fn skill_document_reads_share_an_aggregate_byte_budget() {
    let temporary = tempfile::tempdir().expect("temporary source repository");
    let repository = temporary.path();
    for index in 0..17 {
        let name = format!("large-{index}");
        let prefix = format!("---\nname: {name}\ndescription: fixture\n---\n");
        let mut content = prefix.into_bytes();
        content.resize(1_000_000, b'x');
        let path = repository.join("skills").join(&name).join("SKILL.md");
        fs::create_dir_all(path.parent().expect("skill parent"))
            .expect("create large skill directory");
        fs::write(path, content).expect("write large skill document");
    }

    let result = propose_catalogs(repository);

    assert!(matches!(
        result,
        Err(skilled::Error::SourceScanLimitExceeded)
    ));
}

fn write_skill(path: impl AsRef<std::path::Path>, name: &str) {
    let path = path.as_ref();
    fs::create_dir_all(path.parent().expect("skill parent")).expect("create skill directory");
    fs::write(
        path,
        format!("---\nname: {name}\ndescription: fixture\n---\n# {name}\n"),
    )
    .expect("write skill fixture");
}
