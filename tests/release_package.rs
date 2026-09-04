use std::{
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
    let before = fs::read(&database).expect("read future database");

    // A newer schema no longer exits before the TUI starts: the session opens
    // degraded and read-only, the same way `SkilledApp::open` does in-process.
    // Drive it through a pseudo-terminal so the packaged binary can show that
    // refusal, then prove it did not write through the future database.
    let refused = run_tui_smoke(&executable, &future_runtime);
    assert!(refused.status.success(), "{}", diagnostics(&refused));
    let recorded =
        fs::read(future_runtime.join("typescript")).expect("read the recorded future-schema TUI");
    let recorded = String::from_utf8_lossy(&recorded);
    let visible = visible_text(&recorded);
    assert!(
        future_schema_refusal_is_visible(&visible),
        "recorded TUI:\n{recorded}\nvisible:\n{visible}\n{}",
        diagnostics(&refused)
    );
    assert_eq!(
        fs::read(&database).expect("reread future database"),
        before,
        "the older packaged executable modified future application data"
    );
    assert!(
        !future_data.join("skilled.sqlite3-wal").exists(),
        "the older packaged executable created a write-ahead log beside the future database"
    );
    assert!(
        !future_data.join("skilled.sqlite3-shm").exists(),
        "the older packaged executable created a shared-memory sidecar beside the future database"
    );
}

#[test]
fn visible_text_joins_a_wrapped_schema_refusal() {
    let recorded = "application metadata schema\r\n\u{1b}[0m99 is newer than supported schema 10.";
    let visible = visible_text(recorded);
    assert!(
        visible.contains("application metadata schema 99 is newer than supported schema 10"),
        "{visible}"
    );
    assert!(future_schema_refusal_is_visible(&visible), "{visible}");
}

#[test]
fn future_schema_gate_accepts_csi_glued_schema_version() {
    // macOS Application Support paths wrap so `schema` ends a row and `99`
    // starts the next, separated by CSI cursor addressing rather than
    // whitespace. After CSI strip the tokens glue to `schema99`.
    let recorded = "application metadata schema\u{1b}[8;1H99 is newer than supported schema 10.";
    let visible = visible_text(recorded);
    assert!(
        visible.contains("schema99"),
        "CSI-stripped recording should glue the wrapped tokens: {visible}"
    );
    assert!(
        !visible.contains("application metadata schema 99 is newer than supported schema 10"),
        "the glued recording must not satisfy the old spanning phrase: {visible}"
    );
    assert!(
        future_schema_refusal_is_visible(&visible),
        "the release gate must still pass after that glue: {visible}"
    );
}

/// The future-schema gate looks for substrings that cannot span an 80-column
/// wrap. Ratatui/`script` on macOS can place `schema` at the end of one row
/// and `99` at the start of the next, with CSI cursor addressing and no
/// whitespace between them; after CSI strip those tokens glue to `schema99`.
fn future_schema_refusal_is_visible(visible: &str) -> bool {
    visible.contains("99 is newer than supported schema 10")
        && visible.contains("application metadata schema")
}

/// Strip CSI/OSC and collapse whitespace so a wrap that left a newline still
/// reads as one sentence. CSI-only addressing leaves no separator to collapse.
fn visible_text(recorded: &str) -> String {
    let mut out = String::new();
    let mut chars = recorded.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\u{1b}' {
            match chars.next() {
                Some('[') => {
                    for next in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&next) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(next) = chars.next() {
                        if next == '\u{7}' {
                            break;
                        }
                        if next == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                Some(_) | None => {}
            }
            continue;
        }
        if character.is_whitespace() {
            if !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
        } else if !character.is_control() {
            out.push(character);
        }
    }
    out
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
    fs::create_dir_all(runtime).expect("create runtime directory for the recorded TUI");
    let typescript = runtime.join("typescript");
    // A piped `script(1)` PTY starts at 0×0, which draws nothing. 80×24 is the
    // documented minimum layout, and the schema-refusal banner has to be on
    // screen for the release gate to observe it. `stty` and macOS `/bin/sh`
    // are addressed absolutely because the smoke PATH is an empty isolated
    // directory.
    let stty = if cfg!(target_os = "macos") {
        "/bin/stty"
    } else {
        "/usr/bin/stty"
    };
    let launch = format!("{stty} cols 80 rows 24; exec {}", shell_quote(executable));
    let mut command = Command::new("/usr/bin/script");
    if cfg!(target_os = "macos") {
        command
            .args(["-q"])
            .arg(&typescript)
            .args(["/bin/sh", "-c"])
            .arg(&launch);
    } else if cfg!(target_os = "linux") {
        command
            .args(["-q", "-e", "-c"])
            .arg(&launch)
            .arg(&typescript);
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
