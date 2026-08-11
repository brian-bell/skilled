use std::{collections::BTreeSet, fs, process::Command};

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

    let metadata = String::from_utf8(output.stdout).expect("Cargo metadata is UTF-8 JSON");
    for field in [
        r#""name":"skilled""#,
        r#""version":"0.2.0""#,
        r#""license":"MIT""#,
        r#""readme":"README.md""#,
        r#""repository":"https://github.com/brian-bell/skilled""#,
        r#""homepage":"https://github.com/brian-bell/skilled""#,
        r#""rust_version":"1.97""#,
        r#""kind":["bin"]"#,
        r#""name":"skilled","src_path":"#,
    ] {
        assert!(metadata.contains(field), "missing {field} in {metadata}");
    }
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
    for entry in
        fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/src")).expect("read Rust sources")
    {
        let entry = entry.expect("read source entry");
        if entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "rs")
        {
            expected.insert(format!("src/{}", entry.file_name().to_string_lossy()));
        }
    }

    assert_eq!(packaged, expected);
}
