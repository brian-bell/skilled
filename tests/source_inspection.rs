use std::{fs, path::Path, process::Command};

use skilled::source::inspect_local_source;

#[test]
fn a_path_inside_a_checkout_resolves_to_its_canonical_git_top_level() {
    let temporary = tempfile::tempdir().expect("temporary source repository");
    let repository = temporary.path().join("catalog");
    fs::create_dir_all(repository.join("skills/example")).expect("create repository contents");
    git(&repository, &["init", "-b", "main"]);
    git(&repository, &["config", "user.name", "Skilled Test"]);
    git(
        &repository,
        &["config", "user.email", "skilled@example.test"],
    );
    fs::write(repository.join("README.md"), "fixture\n").expect("write fixture");
    git(&repository, &["add", "README.md"]);
    git(&repository, &["commit", "-m", "fixture"]);

    let source = inspect_local_source(&repository.join("skills/example"))
        .expect("inspect nested checkout path");

    assert_eq!(source.git_top_level(), repository.canonicalize().unwrap());
    assert_eq!(source.branch(), Some("main"));
    assert_eq!(source.head().len(), 40);
    assert!(!source.dirty());
}

fn git(repository: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .expect("run Git fixture command");
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
