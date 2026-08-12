use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[test]
#[ignore = "release gate: packages and rebuilds every dependency in a clean Cargo home"]
fn the_exact_cargo_package_installs_and_honors_the_release_contract() {
    let temporary = tempfile::tempdir().expect("temporary release verification root");
    let package_target = temporary.path().join("package-target");
    let package = run(
        Command::new(env!("CARGO"))
            .args(["package", "--locked", "--target-dir"])
            .arg(&package_target)
            .current_dir(env!("CARGO_MANIFEST_DIR")),
        "package the checkout",
    );
    assert!(package.status.success(), "{}", diagnostics(&package));

    let packaged_source = package_target.join("package/skilled-0.2.0");
    assert!(packaged_source.is_dir(), "missing {packaged_source:?}");
    let cargo_home = temporary.path().join("cargo-home");
    let install_root = temporary.path().join("install");
    let install = run(
        Command::new(env!("CARGO"))
            .args(["install", "--locked", "--path"])
            .arg(&packaged_source)
            .arg("--root")
            .arg(&install_root)
            .env("CARGO_HOME", &cargo_home)
            .env("CARGO_TARGET_DIR", temporary.path().join("install-target")),
        "install the exact package payload",
    );
    assert!(install.status.success(), "{}", diagnostics(&install));

    let executable = installed_executable(&install_root);
    let runtime = temporary.path().join("runtime");
    let version = run(
        Command::new(&executable)
            .arg("--version")
            .env("HOME", runtime.join("home"))
            .env("XDG_DATA_HOME", runtime.join("data"))
            .env("PATH", runtime.join("bin")),
        "read the installed version",
    );
    assert!(version.status.success(), "{}", diagnostics(&version));
    assert_eq!(version.stdout, b"skilled 0.2.0\n");
    assert!(version.stderr.is_empty(), "{}", diagnostics(&version));
    assert!(
        !runtime.exists(),
        "version discovery initialized runtime state at {runtime:?}"
    );

    let startup_runtime = temporary.path().join("startup-runtime");
    let startup = run_tui_smoke(&executable, &startup_runtime);
    assert!(startup.status.success(), "{}", diagnostics(&startup));
    assert!(
        application_data_dir(&startup_runtime)
            .join("skilled.sqlite3")
            .is_file(),
        "the installed TUI did not finish application startup"
    );

    let future_runtime = temporary.path().join("future-runtime");
    let future_data = application_data_dir(&future_runtime);
    fs::create_dir_all(&future_data).expect("create future application data directory");
    let database = future_data.join("skilled.sqlite3");
    let connection = rusqlite::Connection::open(&database).expect("create future database");
    connection
        .execute_batch(
            "CREATE TABLE future_data (value TEXT NOT NULL);
             INSERT INTO future_data VALUES ('leave this untouched');
             PRAGMA user_version = 99;",
        )
        .expect("create future schema fixture");
    drop(connection);
    let before = snapshot_tree(&future_runtime);

    let refused = run(
        Command::new(&executable)
            .env("HOME", future_runtime.join("home"))
            .env("XDG_DATA_HOME", future_runtime.join("data"))
            .env("PATH", future_runtime.join("bin")),
        "start the installed binary against future metadata",
    );
    assert!(!refused.status.success(), "{}", diagnostics(&refused));
    assert!(refused.stdout.is_empty(), "{}", diagnostics(&refused));
    let error = String::from_utf8_lossy(&refused.stderr);
    assert!(
        error.contains("application metadata schema 99 is newer than supported schema 5"),
        "{}",
        diagnostics(&refused)
    );
    assert_eq!(
        snapshot_tree(&future_runtime),
        before,
        "the older packaged executable modified future application data"
    );
}

fn run(command: &mut Command, context: &str) -> std::process::Output {
    command
        .output()
        .unwrap_or_else(|error| panic!("{context}: {error}"))
}

fn diagnostics(output: &std::process::Output) -> String {
    format!(
        "status: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn installed_executable(root: &Path) -> std::path::PathBuf {
    let name = if cfg!(windows) {
        "skilled.exe"
    } else {
        "skilled"
    };
    root.join("bin").join(name)
}

fn run_tui_smoke(executable: &Path, runtime: &Path) -> std::process::Output {
    let mut command = Command::new("/usr/bin/script");
    if cfg!(target_os = "macos") {
        command.args(["-q", "/dev/null"]).arg(executable);
    } else if cfg!(target_os = "linux") {
        command
            .args(["-q", "-e", "-c"])
            .arg(format!("exec {}", shell_quote(executable)))
            .arg("/dev/null");
    } else {
        panic!("the release package gate supports the advertised macOS and Linux platforms");
    }
    command
        .env("HOME", runtime.join("home"))
        .env("XDG_DATA_HOME", runtime.join("data"))
        .env("PATH", runtime.join("bin"))
        .env("TERM", "xterm-256color")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command
        .spawn()
        .expect("start installed TUI in a pseudo-terminal");
    child
        .stdin
        .take()
        .expect("pseudo-terminal input")
        .write_all(b"q")
        .expect("quit installed TUI");

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if child.try_wait().expect("poll installed TUI").is_some() {
            return child
                .wait_with_output()
                .expect("collect installed TUI output");
        }
        if Instant::now() >= deadline {
            child.kill().expect("stop hung installed TUI");
            let output = child
                .wait_with_output()
                .expect("collect timed-out installed TUI output");
            panic!("installed TUI did not quit:\n{}", diagnostics(&output));
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\"'\"'"))
}

fn application_data_dir(runtime: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        runtime
            .join("home/Library/Application Support")
            .join("skilled")
    } else if cfg!(target_os = "linux") {
        runtime.join("data/skilled")
    } else {
        panic!("the release package gate supports the advertised macOS and Linux platforms")
    }
}

fn snapshot_tree(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn visit(root: &Path, path: &Path, snapshot: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        let mut entries: Vec<_> = fs::read_dir(path)
            .unwrap_or_else(|error| panic!("read {path:?}: {error}"))
            .map(|entry| entry.expect("read directory entry"))
            .collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .expect("snapshot entry is beneath its root")
                .to_owned();
            let file_type = entry.file_type().expect("read snapshot entry type");
            if file_type.is_dir() {
                snapshot.insert(relative, None);
                visit(root, &path, snapshot);
            } else {
                snapshot.insert(relative, Some(fs::read(&path).expect("read snapshot file")));
            }
        }
    }

    let mut snapshot = BTreeMap::new();
    visit(root, root, &mut snapshot);
    snapshot
}
