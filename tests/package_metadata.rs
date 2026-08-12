use std::{collections::BTreeSet, fs, path::Path, process::Command};

#[test]
fn cargo_metadata_identifies_the_release_and_its_executable() {
    let output = Command::new(env!("CARGO"))
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("read Cargo metadata");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("Cargo metadata is JSON");
    let package = &metadata["packages"][0];
    assert_eq!(package["name"], "skilled");
    assert_eq!(package["version"], "0.2.0");
    assert_eq!(package["license"], "MIT");
    assert_eq!(package["readme"], "README.md");
    assert_eq!(
        package["repository"],
        "https://github.com/brian-bell/skilled"
    );
    assert_eq!(package["homepage"], "https://github.com/brian-bell/skilled");
    assert_eq!(package["rust_version"], "1.97");

    let binary = package["targets"]
        .as_array()
        .expect("Cargo targets are an array")
        .iter()
        .find(|target| {
            target["kind"]
                .as_array()
                .is_some_and(|kinds| kinds.iter().any(|kind| kind == "bin"))
        })
        .expect("Cargo package has a binary target");
    assert_eq!(binary["name"], "skilled");
    assert_eq!(
        binary["src_path"],
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/main.rs")
            .to_string_lossy()
            .as_ref()
    );
}

#[test]
fn cargo_package_contains_only_the_buildable_release_payload() {
    let output = Command::new(env!("CARGO"))
        .args(["package", "--list", "--allow-dirty"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("list the Cargo package payload");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let packaged: BTreeSet<_> = String::from_utf8(output.stdout)
        .expect("package list is UTF-8")
        .lines()
        .map(str::to_owned)
        .collect();
    let mut expected = BTreeSet::from([
        ".cargo_vcs_info.json".to_owned(),
        "Cargo.lock".to_owned(),
        "Cargo.toml".to_owned(),
        "Cargo.toml.orig".to_owned(),
        "LICENSE".to_owned(),
        "README.md".to_owned(),
    ]);
    collect_rust_sources(
        Path::new(env!("CARGO_MANIFEST_DIR")),
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src")),
        &mut expected,
    );

    assert_eq!(packaged, expected);
}

fn collect_rust_sources(root: &Path, directory: &Path, expected: &mut BTreeSet<String>) {
    for entry in fs::read_dir(directory).expect("read Rust source directory") {
        let entry = entry.expect("read source entry");
        let path = entry.path();
        if path.is_dir() {
            collect_rust_sources(root, &path, expected);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            expected.insert(
                path.strip_prefix(root)
                    .expect("source is beneath package root")
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
}
