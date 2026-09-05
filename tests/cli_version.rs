use std::{fs, process::Command};

#[test]
fn version_reports_the_packaged_identity_without_initializing_application_state() {
    let isolated = tempfile::tempdir().expect("temporary process environment");
    let output = Command::new(env!("CARGO_BIN_EXE_skilled"))
        .arg("--version")
        .env("HOME", isolated.path().join("home"))
        .env("XDG_DATA_HOME", isolated.path().join("data"))
        .env("PATH", isolated.path().join("bin"))
        .env_remove("USER")
        .env_remove("LOGNAME")
        .env_remove("USERNAME")
        .output()
        .expect("run the packaged binary");

    assert!(
        output.status.success(),
        "status: {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"skilled 0.2.0\n");
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    assert_eq!(
        fs::read_dir(isolated.path())
            .expect("read isolated process environment")
            .count(),
        0,
        "the version command created application or agent state"
    );
}
