//! Real terminal modes and escape sequences, in isolated subprocesses. The
//! test harness owns the PTY so a child failure cannot damage the user's TTY.
#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::{
    fs::{self, File},
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{fs::PermissionsExt, process::CommandExt},
    },
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use skilled::{
    Action, AppEnvironment, SkilledApp,
    terminal::{CrosstermControl, TerminalSession, install_panic_restore_hook},
};

#[test]
fn quitting_during_a_real_git_check_retires_its_transport() {
    for scenario in ["fetch-ctrl-c", "fetch-sigint"] {
        let mut pty = Pty::spawn(scenario);
        pty.wait_for("Inventory");
        pty.master.write_all(b"3u").unwrap();
        let pid_file = pty._directory.path().join("transport.pid");
        let deadline = Instant::now() + Duration::from_secs(10);
        let pid: i32 = loop {
            pty.read();
            if let Some(pid) = fs::read_to_string(&pid_file)
                .ok()
                .and_then(|text| text.trim().parse().ok())
            {
                break pid;
            }
            assert!(
                Instant::now() < deadline,
                "fetch did not start: {}",
                pty.output
            );
            thread::sleep(Duration::from_millis(10));
        };
        if scenario == "fetch-ctrl-c" {
            pty.master.write_all(b"\x03").unwrap();
        } else {
            assert_eq!(
                unsafe { libc::kill(pty.child.id() as i32, libc::SIGINT) },
                0
            );
        }
        pty.finish(true);
        // A killed orphan can briefly remain as a zombie awaiting init on
        // Linux; it has no descriptors and cannot emit terminal output.
        let deadline = Instant::now() + Duration::from_secs(2);
        while process_running(pid) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!process_running(pid), "transport outlived terminal release");
    }
}

fn process_running(pid: i32) -> bool {
    #[cfg(target_os = "linux")]
    if fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        stat.rsplit_once(") ")
            .is_some_and(|(_, state)| state.starts_with('Z'))
    }) {
        return false;
    }
    // SAFETY: signal zero observes this fixture's child without signalling it.
    unsafe { libc::kill(pid, 0) == 0 }
}

const ENTER: &str = "\x1b[?1049h";
const LEAVE: &str = "\x1b[?1049l";
const SHOW: &str = "\x1b[?25h";

#[test]
fn quit_and_both_interrupt_forms_restore_the_real_terminal() {
    for scenario in ["quit", "ctrl-c", "sigint"] {
        let mut pty = Pty::spawn(scenario);
        pty.wait_for("First-run setup");
        assert_eq!(attributes(&pty.slave).c_lflag & libc::ICANON, 0);
        match scenario {
            "quit" => pty.master.write_all(b"q").unwrap(),
            "ctrl-c" => pty.master.write_all(b"\x03").unwrap(),
            _ => {
                // SAFETY: only this fixture's live child receives SIGINT.
                assert_eq!(
                    unsafe { libc::kill(pty.child.id() as i32, libc::SIGINT) },
                    0
                );
            }
        }
        pty.finish(true);
        assert!(pty.output.contains("SIGINT_RESTORED"), "{}", pty.output);
    }
}

#[test]
fn workers_stop_before_restoration_on_success_error_and_panic() {
    for scenario in ["success", "error", "panic"] {
        let mut pty = Pty::spawn(scenario);
        pty.finish(scenario != "panic");
        let worker = pty
            .output
            .find("WORKER_STOPPED_RAW=true")
            .expect(&pty.output);
        let restored = pty.output.find(LEAVE).expect(&pty.output);
        assert!(worker < restored, "{}", pty.output);
        if scenario == "panic" {
            let diagnostic = pty
                .output
                .find("injected terminal panic")
                .expect(&pty.output);
            assert!(restored < diagnostic, "{}", pty.output);
            assert!(restored < pty.output.find("stack backtrace:").expect(&pty.output));
            assert!(
                pty.output.contains("terminal_pty::run_scenario"),
                "{}",
                pty.output
            );
            assert!(
                !pty.output.contains("\x1b[31m"),
                "panic text must be escaped"
            );
        } else if scenario == "error" {
            assert!(restored < pty.output.find("ERROR_RETURNED").expect(&pty.output));
        }
    }
}

// Invoked by the parent tests with a private fixture directory. No production
// environment switches, panic triggers, or fault-injection paths are needed.
#[test]
fn terminal_child() {
    let Ok(scenario) = std::env::var("SKILLED_PTY_SCENARIO") else {
        return;
    };
    let outcome = std::panic::catch_unwind(|| run_scenario(&scenario));
    println!("RESTORATION_READY");
    io::stdout().flush().unwrap();
    // Keep the controlling session alive while the parent inspects termios.
    // macOS revokes the slave when its controlling process exits.
    let mut acknowledgement = String::new();
    io::stdin().read_line(&mut acknowledgement).unwrap();
    if let Err(payload) = outcome {
        std::panic::resume_unwind(payload);
    }
}

fn run_scenario(scenario: &str) {
    if matches!(
        scenario,
        "quit" | "ctrl-c" | "sigint" | "fetch-ctrl-c" | "fetch-sigint"
    ) {
        let root = std::path::PathBuf::from(std::env::var_os("SKILLED_PTY_ROOT").unwrap());
        if scenario.starts_with("fetch-") {
            prepare_fetch(&root);
        }
        // Prove the runner restores an earlier disposition, not just SIG_DFL.
        // SAFETY: this isolated subprocess owns its signal disposition.
        unsafe {
            libc::signal(libc::SIGINT, libc::SIG_IGN);
        }
        skilled::run(AppEnvironment::new(
            root.join("home"),
            root.join("data"),
            "",
        ))
        .expect("run isolated TUI");
        let mut disposition = unsafe { std::mem::zeroed::<libc::sigaction>() };
        assert_eq!(
            unsafe { libc::sigaction(libc::SIGINT, std::ptr::null(), &mut disposition) },
            0
        );
        assert_eq!(disposition.sa_sigaction, libc::SIG_IGN);
        println!("SIGINT_RESTORED");
        return;
    }

    install_panic_restore_hook();
    let session = TerminalSession::start(CrosstermControl).unwrap();
    let outcome: io::Result<()> = session.run(|| {
        // Like SkilledApp::drop, this owner joins its worker on every return
        // and unwind. The worker checks the actual PTY when it is retired.
        struct Worker(Option<mpsc::Sender<()>>, Option<thread::JoinHandle<()>>);
        impl Drop for Worker {
            fn drop(&mut self) {
                self.0.take().unwrap().send(()).unwrap();
                self.1.take().unwrap().join().unwrap();
            }
        }
        let (stop, stopped) = mpsc::channel();
        let handle = thread::spawn(move || {
            stopped.recv().unwrap();
            thread::sleep(Duration::from_millis(30));
            println!(
                "WORKER_STOPPED_RAW={}",
                crossterm::terminal::is_raw_mode_enabled().unwrap()
            );
        });
        let _worker = Worker(Some(stop), Some(handle));
        match scenario {
            "panic" => panic!("injected terminal panic \x1b[31m"),
            "error" => Err(io::Error::other("injected event error")),
            _ => Ok(()),
        }
    });
    if scenario == "error" {
        assert!(outcome.is_err());
        println!("ERROR_RETURNED");
    } else {
        outcome.unwrap();
    }
}

fn prepare_fetch(root: &std::path::Path) {
    let repository = root.join("library");
    fs::create_dir_all(repository.join("skills/demo")).unwrap();
    fs::write(
        repository.join("skills/demo/SKILL.md"),
        "---\nname: demo\ndescription: Fixture\n---\nFixture\n",
    )
    .unwrap();
    let global = root.join("gitconfig");
    fs::write(&global, "").unwrap();
    // This child has not started any application threads. All Git reads and
    // writes use private configuration and repositories, never the real home.
    unsafe {
        std::env::set_var("GIT_CONFIG_GLOBAL", &global);
        std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
        std::env::set_var("GIT_SSH_VARIANT", "ssh");
    }
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .arg("-C")
            .arg(&repository)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "-b", "main"]);
    git(&["config", "user.name", "Fixture"]);
    git(&["config", "user.email", "fixture@example.invalid"]);
    git(&["add", "."]);
    git(&["-c", "commit.gpgsign=false", "commit", "-m", "fixture"]);
    git(&["remote", "add", "origin", "ssh://example.invalid/skills"]);
    git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
    git(&["branch", "--set-upstream-to=origin/main"]);
    let transport = root.join("transport");
    let quote =
        |path: &std::path::Path| format!("'{}'", path.display().to_string().replace('\'', "'\\''"));
    fs::write(
        &transport,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$$\" > {}\nexec /bin/sleep 60\n",
            quote(&root.join("transport.pid"))
        ),
    )
    .unwrap();
    fs::set_permissions(&transport, fs::Permissions::from_mode(0o755)).unwrap();
    git(&["config", "--global", "core.sshCommand", &quote(&transport)]);
    let mut app = SkilledApp::open(AppEnvironment::new(
        root.join("home"),
        root.join("data"),
        "",
    ))
    .unwrap();
    let preview = app.preview_source(&repository).unwrap();
    app.confirm_source(preview).unwrap();
    for _ in 0..7 {
        let update = app.update(Action::Continue);
        app.perform_effects(update.effects()).unwrap();
    }
}

struct Pty {
    child: Child,
    master: File,
    slave: File,
    before: libc::termios,
    output: String,
    _directory: tempfile::TempDir,
}

impl Pty {
    fn spawn(scenario: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("home")).unwrap();
        let mut master = -1;
        let mut slave = -1;
        let mut size = libc::winsize {
            ws_row: 30,
            ws_col: 100,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: openpty initializes the two descriptors on success. File
        // takes ownership exactly once; the size pointer names a valid record.
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &raw mut size,
                )
            },
            0
        );
        let (master, slave) = unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) };
        // Keep fixture descriptors out of the child's exec except its stdio.
        for fd in [master.as_raw_fd(), slave.as_raw_fd()] {
            assert_ne!(
                unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) },
                -1
            );
        }
        let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
        assert_ne!(
            unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) },
            -1
        );
        let before = attributes(&slave);
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "terminal_child", "--nocapture"])
            .env("SKILLED_PTY_SCENARIO", scenario)
            .env("SKILLED_PTY_ROOT", directory.path())
            .env("TERM", "xterm-256color")
            .env(
                "RUST_BACKTRACE",
                if scenario == "panic" { "1" } else { "0" },
            )
            .env_remove("RUST_LIB_BACKTRACE")
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave.try_clone().unwrap()));
        // SAFETY: after fork, only async-signal-safe syscalls run before exec.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Self {
            child: command.spawn().unwrap(),
            master,
            slave,
            before,
            output: String::new(),
            _directory: directory,
        }
    }

    fn read(&mut self) {
        let mut bytes = [0; 8192];
        loop {
            match self.master.read(&mut bytes) {
                Ok(0) => break,
                Ok(n) => self.output.push_str(&String::from_utf8_lossy(&bytes[..n])),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock
                        || error.raw_os_error() == Some(libc::EIO) =>
                {
                    break;
                }
                Err(error) => panic!("PTY read: {error}"),
            }
        }
    }

    fn wait_for(&mut self, needle: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            self.read();
            if self.output.contains(needle) {
                return;
            }
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "child exited: {}",
                self.output
            );
            assert!(
                Instant::now() < deadline,
                "waiting for {needle}: {}",
                self.output
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn finish(&mut self, success: bool) {
        self.wait_for("RESTORATION_READY");
        let after = attributes(&self.slave);
        assert_eq!(after.c_iflag, self.before.c_iflag);
        assert_eq!(after.c_oflag, self.before.c_oflag);
        assert_eq!(after.c_cflag, self.before.c_cflag);
        assert_eq!(after.c_lflag, self.before.c_lflag);
        assert_eq!(after.c_cc, self.before.c_cc);
        self.master.write_all(b"\n").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            self.read();
            if let Some(status) = self.child.try_wait().unwrap() {
                self.read();
                assert_eq!(status.success(), success, "{}", self.output);
                break;
            }
            assert!(
                Instant::now() < deadline,
                "child did not stop: {}",
                self.output
            );
            thread::sleep(Duration::from_millis(10));
        }
        assert!(self.output.contains(ENTER), "{}", self.output);
        assert!(self.output.contains(LEAVE), "{}", self.output);
        assert!(self.output.contains(SHOW), "{}", self.output);
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn attributes(file: &File) -> libc::termios {
    // SAFETY: tcgetattr fills a live record using this fixture's open PTY.
    unsafe {
        let mut value = std::mem::zeroed();
        assert_eq!(libc::tcgetattr(file.as_raw_fd(), &mut value), 0);
        value
    }
}
