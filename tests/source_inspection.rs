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
    assert_eq!(source.dirty(), Some(false));
}

#[test]
fn inspection_removes_credentials_from_remote_metadata() {
    let temporary = tempfile::tempdir().expect("temporary source repository");
    let repository = temporary.path().join("catalog");
    fs::create_dir_all(&repository).expect("create repository");
    git(&repository, &["init", "-b", "main"]);
    git(&repository, &["config", "user.name", "Skilled Test"]);
    git(
        &repository,
        &["config", "user.email", "skilled@example.test"],
    );
    fs::write(repository.join("README.md"), "fixture\n").expect("write fixture");
    git(&repository, &["add", "README.md"]);
    git(&repository, &["commit", "-m", "fixture"]);
    git(
        &repository,
        &[
            "remote",
            "add",
            "origin",
            "https://user:secret@example.test/owner/catalog.git",
        ],
    );

    let source = inspect_local_source(&repository).expect("inspect checkout");

    assert_eq!(
        source.remote_url(),
        Some("https://example.test/owner/catalog.git")
    );

    git(
        &repository,
        &[
            "remote",
            "set-url",
            "origin",
            "user/name:secret@example.test:catalog.git",
        ],
    );
    let scp_style = inspect_local_source(&repository).expect("inspect scp-style remote");
    assert_eq!(scp_style.remote_url(), Some("example.test:catalog.git"));

    git(
        &repository,
        &["remote", "set-url", "origin", "/tmp/catalog@backup.git"],
    );
    let local_path = inspect_local_source(&repository).expect("inspect local-path remote");
    assert_eq!(local_path.remote_url(), Some("/tmp/catalog@backup.git"));
}

#[test]
fn inspection_accepts_an_explicitly_empty_filter_attribute() {
    let temporary = tempfile::tempdir().expect("temporary source repository");
    let repository = temporary.path().join("catalog");
    fs::create_dir_all(&repository).expect("create repository");
    git(&repository, &["init", "-b", "main"]);
    git(&repository, &["config", "user.name", "Skilled Test"]);
    git(
        &repository,
        &["config", "user.email", "skilled@example.test"],
    );
    fs::write(repository.join(".gitattributes"), "README.md filter=\n").expect("write attributes");
    fs::write(repository.join("README.md"), "original\n").expect("write fixture");
    git(&repository, &["add", ".gitattributes", "README.md"]);
    git(&repository, &["commit", "-m", "fixture"]);
    fs::write(repository.join("README.md"), "modified\n").expect("modify tracked file");

    let source = inspect_local_source(&repository).expect("inspect checkout");

    assert_eq!(source.dirty(), Some(true));
}

#[cfg(unix)]
#[test]
fn inspection_preserves_path_whitespace_and_does_not_invoke_fsmonitor() {
    use std::os::unix::fs::PermissionsExt;

    let temporary = tempfile::tempdir().expect("temporary source repository");
    let repository = temporary.path().join("catalog ");
    fs::create_dir_all(&repository).expect("create repository");
    git(&repository, &["init", "-b", "main"]);
    git(&repository, &["config", "user.name", "Skilled Test"]);
    git(
        &repository,
        &["config", "user.email", "skilled@example.test"],
    );
    fs::write(repository.join("README.md"), "fixture\n").expect("write fixture");
    git(&repository, &["add", "README.md"]);
    git(&repository, &["commit", "-m", "fixture"]);
    let sentinel = temporary.path().join("fsmonitor-was-invoked");
    let monitor = temporary.path().join("fsmonitor");
    fs::write(
        &monitor,
        format!("#!/bin/sh\ntouch '{}'\nprintf '0\\n'\n", sentinel.display()),
    )
    .expect("write fsmonitor fixture");
    fs::set_permissions(&monitor, fs::Permissions::from_mode(0o755))
        .expect("make fsmonitor executable");
    git(
        &repository,
        &["config", "core.fsmonitor", monitor.to_str().unwrap()],
    );
    let index_modified = fs::metadata(repository.join(".git/index"))
        .expect("read index metadata")
        .modified()
        .expect("read index modification time");

    let source = inspect_local_source(&repository).expect("inspect checkout");

    assert_eq!(source.git_top_level(), repository.canonicalize().unwrap());
    assert!(
        !sentinel.exists(),
        "inspection invoked configured fsmonitor"
    );
    assert_eq!(
        fs::metadata(repository.join(".git/index"))
            .expect("read index metadata after inspection")
            .modified()
            .expect("read index modification time after inspection"),
        index_modified,
        "inspection refreshed the Git index"
    );
}

#[cfg(unix)]
#[test]
fn inspection_reports_filtered_dirty_state_as_unavailable_without_invoking_the_filter() {
    use std::os::unix::fs::PermissionsExt;

    let temporary = tempfile::tempdir().expect("temporary source repository");
    let repository = temporary.path().join("catalog");
    fs::create_dir_all(&repository).expect("create repository");
    git(&repository, &["init", "-b", "main"]);
    git(&repository, &["config", "user.name", "Skilled Test"]);
    git(
        &repository,
        &["config", "user.email", "skilled@example.test"],
    );
    fs::write(
        repository.join(".gitattributes"),
        "README.md filter=sentinel\n",
    )
    .expect("write attributes");
    let sentinel = temporary.path().join("filter-was-invoked");
    let filter = temporary.path().join("clean-filter");
    fs::write(
        &filter,
        format!(
            "#!/bin/sh\ntouch '{}'\nprintf 'normalized\\n'\n",
            sentinel.display()
        ),
    )
    .expect("write clean filter fixture");
    fs::set_permissions(&filter, fs::Permissions::from_mode(0o755))
        .expect("make clean filter executable");
    git(
        &repository,
        &["config", "filter.sentinel.clean", filter.to_str().unwrap()],
    );
    git(&repository, &["config", "filter.sentinel.required", "true"]);
    fs::write(repository.join("README.md"), "original\n").expect("write fixture");
    git(&repository, &["add", ".gitattributes", "README.md"]);
    git(&repository, &["commit", "-m", "fixture"]);
    fs::remove_file(&sentinel).expect("clear filter sentinel after fixture setup");
    fs::write(repository.join("README.md"), "different\n").expect("modify filtered file");

    let source = inspect_local_source(&repository).expect("inspect checkout");

    assert!(source.dirty().is_none());
    assert!(
        !sentinel.exists(),
        "inspection invoked a repository-defined clean filter"
    );

    fs::write(repository.join("untracked.txt"), "definitely dirty\n")
        .expect("write untracked fixture");
    let definitely_dirty = inspect_local_source(&repository).expect("reinspect checkout");

    assert_eq!(definitely_dirty.dirty(), Some(true));
    assert!(
        !sentinel.exists(),
        "reinspection invoked a repository-defined clean filter"
    );
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
