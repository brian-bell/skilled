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

fn write_skill(path: impl AsRef<std::path::Path>, name: &str) {
    let path = path.as_ref();
    fs::create_dir_all(path.parent().expect("skill parent")).expect("create skill directory");
    fs::write(
        path,
        format!("---\nname: {name}\ndescription: fixture\n---\n# {name}\n"),
    )
    .expect("write skill fixture");
}
