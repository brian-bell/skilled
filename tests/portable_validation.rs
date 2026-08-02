use std::fs;

use skilled::validation::{PortableValidationError, validate_portable_skill};

#[test]
fn a_portable_skill_accepts_parseable_frontmatter_and_unknown_fields() {
    let temporary = tempfile::tempdir().expect("temporary skill directory");
    let skill = temporary.path().join("portable");
    fs::create_dir(&skill).expect("create skill directory");
    fs::write(
        skill.join("SKILL.md"),
        "---\nname: portable\ndescription: A portable fixture\nunknown-agent-field: true\n---\n# Portable\n",
    )
    .expect("write skill");

    let validated = validate_portable_skill(&skill).expect("validate portable skill");

    assert_eq!(validated.name(), "portable");
    assert_eq!(validated.description(), "A portable fixture");
    assert_eq!(validated.body(), "# Portable\n");
}

#[test]
fn portable_frontmatter_may_close_at_end_of_file() {
    let temporary = tempfile::tempdir().expect("temporary skill directory");
    let skill = temporary.path().join("portable");
    fs::create_dir(&skill).expect("create skill directory");
    fs::write(
        skill.join("SKILL.md"),
        "---\nname: portable\ndescription: A portable fixture\n---",
    )
    .expect("write skill");

    let validated = validate_portable_skill(&skill).expect("validate portable skill");

    assert_eq!(validated.body(), "");
}

#[test]
fn portable_names_obey_length_separator_and_directory_match_rules() {
    let temporary = tempfile::tempdir().expect("temporary skill directory");
    let skill = temporary.path().join("portable");
    fs::create_dir(&skill).expect("create skill directory");

    for invalid_name in [
        "",
        "Portable",
        "portable_skill",
        "-portable",
        "portable-",
        "portable--skill",
    ] {
        write_skill(&skill, invalid_name, "fixture");
        assert!(
            matches!(
                validate_portable_skill(&skill),
                Err(PortableValidationError::InvalidName)
            ),
            "{invalid_name:?} should be rejected"
        );
    }

    write_skill(&skill, &"a".repeat(65), "fixture");
    assert!(matches!(
        validate_portable_skill(&skill),
        Err(PortableValidationError::InvalidName)
    ));

    write_skill(&skill, "different", "fixture");
    assert!(matches!(
        validate_portable_skill(&skill),
        Err(PortableValidationError::NameMismatch { .. })
    ));
}

#[test]
fn portable_descriptions_are_required_and_limited_to_1024_characters() {
    let temporary = tempfile::tempdir().expect("temporary skill directory");
    let skill = temporary.path().join("portable");
    fs::create_dir(&skill).expect("create skill directory");

    write_skill(&skill, "portable", "");
    assert!(matches!(
        validate_portable_skill(&skill),
        Err(PortableValidationError::InvalidDescription)
    ));

    write_skill(&skill, "portable", &"d".repeat(1025));
    assert!(matches!(
        validate_portable_skill(&skill),
        Err(PortableValidationError::InvalidDescription)
    ));

    write_skill(&skill, "portable", &"d".repeat(1024));
    assert!(validate_portable_skill(&skill).is_ok());
}

#[test]
fn portable_validation_rejects_wrong_filenames_frontmatter_and_unreadable_text() {
    let temporary = tempfile::tempdir().expect("temporary skill directory");
    let skill = temporary.path().join("portable");
    fs::create_dir(&skill).expect("create skill directory");

    fs::write(skill.join("skill.md"), "fixture").expect("write wrong-cased file");
    assert!(matches!(
        validate_portable_skill(&skill),
        Err(PortableValidationError::MissingSkillMd)
    ));
    fs::remove_file(skill.join("skill.md")).expect("remove wrong-cased file");

    fs::write(skill.join("SKILL.md"), "# Missing frontmatter\n").expect("write skill");
    assert!(matches!(
        validate_portable_skill(&skill),
        Err(PortableValidationError::MissingFrontmatter)
    ));

    fs::write(
        skill.join("SKILL.md"),
        "---\nname: [not, a, string]\ndescription: fixture\n---\nbody\n",
    )
    .expect("write malformed metadata");
    assert!(matches!(
        validate_portable_skill(&skill),
        Err(PortableValidationError::InvalidFrontmatter(_))
    ));

    fs::write(skill.join("SKILL.md"), [0xff, 0xfe]).expect("write unreadable text");
    assert!(matches!(
        validate_portable_skill(&skill),
        Err(PortableValidationError::UnreadableSkillMd(_))
    ));
}

#[cfg(unix)]
#[test]
fn portable_validation_rejects_a_symlinked_skill_document() {
    use std::os::unix::fs::symlink;

    let temporary = tempfile::tempdir().expect("temporary skill directory");
    let skill = temporary.path().join("portable");
    fs::create_dir(&skill).expect("create skill directory");
    let outside = temporary.path().join("outside.md");
    fs::write(
        &outside,
        "---\nname: portable\ndescription: Outside fixture\n---\n# Outside\n",
    )
    .expect("write outside document");
    symlink(&outside, skill.join("SKILL.md")).expect("symlink skill document");

    assert!(matches!(
        validate_portable_skill(&skill),
        Err(PortableValidationError::MissingSkillMd)
    ));
}

#[test]
fn portable_validation_bounds_the_skill_document_size() {
    let temporary = tempfile::tempdir().expect("temporary skill directory");
    let skill = temporary.path().join("portable");
    fs::create_dir(&skill).expect("create skill directory");
    fs::write(skill.join("SKILL.md"), vec![b'a'; 1_048_577])
        .expect("write oversized skill document");

    assert!(matches!(
        validate_portable_skill(&skill),
        Err(PortableValidationError::SkillMdTooLarge { .. })
    ));
}

fn write_skill(directory: &std::path::Path, name: &str, description: &str) {
    fs::write(
        directory.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\n# Body\n"),
    )
    .expect("write skill");
}
