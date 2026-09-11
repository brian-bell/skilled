//! Ownership of the processes launched by cancellable Git checks only.
//!
//! Unix children start a new session/process group, with no controlling
//! terminal and only null/piped standard streams. We kill the owned group on
//! cancellation, errors, and leader completion. `waitid(WNOWAIT)` observes exit
//! without reaping: the leader's PID reserves the group ID until the last signal
//! has been sent. A UI cancellation may signal under the slot lock, but only the
//! collector reaps. Confirmed writes never use this boundary.
//!
//! This contains ordinary transport descendants, not programs that deliberately
//! escape the group or reopen a terminal by pathname. Pipe reads are nonblocking
//! on Unix and stay on the worker, so even an escaped pipe holder cannot strand
//! a reader thread. As with ordinary Child::wait, reaping a SIGKILLed process
//! assumes the kernel can schedule its exit (not indefinite uninterruptible I/O).

use std::{
    io,
    process::{Child, Command, ExitStatus, Stdio},
};

pub(crate) struct CancellableChild {
    pub(super) child: Child,
    retired: bool,
}

impl CancellableChild {
    pub(super) fn spawn(command: &mut Command) -> io::Result<Self> {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // SAFETY: setsid is async-signal-safe; no allocation or locks in
            // this post-fork callback. It also creates the child's own group.
            // Merely setpgid(0, 0) would leave /dev/tty available to helpers.
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() == -1 {
                        Err(io::Error::last_os_error())
                    } else {
                        Ok(())
                    }
                });
            }
        }
        Ok(Self {
            child: command.spawn()?,
            retired: false,
        })
    }

    /// Signal only; the worker retains the unreaped leader and pipe ownership.
    pub(crate) fn cancel(&mut self) {
        if self.retired {
            return;
        }
        #[cfg(unix)]
        // SAFETY: spawn established PGID == PID, and this owner has not reaped
        // the leader. The group number cannot have been recycled. SIGKILL has
        // no grace period: these are checks, never confirmed worktree writes.
        unsafe {
            libc::kill(-(self.child.id() as libc::pid_t), libc::SIGKILL);
        }
        #[cfg(not(unix))]
        let _ = self.child.kill();
    }

    #[cfg(unix)]
    fn has_exited(&self) -> io::Result<bool> {
        // SAFETY: info is writable and zero initialization represents no event.
        // WNOWAIT is essential: no other owner may reap or signal this child.
        unsafe {
            let mut info: libc::siginfo_t = std::mem::zeroed();
            if libc::waitid(
                libc::P_PID,
                self.child.id() as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            ) == -1
            {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    return Ok(false);
                }
                return Err(error);
            }
            Ok(info.si_pid() != 0)
        }
    }

    fn finish(&mut self) -> io::Result<ExitStatus> {
        self.cancel();
        let status = self.child.wait()?;
        self.retired = true;
        Ok(status)
    }
}

impl Drop for CancellableChild {
    fn drop(&mut self) {
        if !self.retired {
            let _ = self.finish();
        }
    }
}

#[cfg(unix)]
use std::{
    io::Read,
    os::fd::AsRawFd,
    process::Output,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[cfg(unix)]
struct SlotOwner<'a>(&'a Mutex<Option<CancellableChild>>);

#[cfg(unix)]
impl Drop for SlotOwner<'_> {
    fn drop(&mut self) {
        // Also runs on setup/read/wait errors and worker panic. No detached
        // readers exist, and the child is always signalled before it is reaped.
        self.0.lock().unwrap_or_else(|p| p.into_inner()).take();
    }
}

#[cfg(unix)]
fn nonblocking(pipe: &impl AsRawFd) -> io::Result<()> {
    // SAFETY: pipe owns this descriptor for the duration of both calls.
    unsafe {
        let flags = libc::fcntl(pipe.as_raw_fd(), libc::F_GETFL);
        if flags == -1
            || libc::fcntl(pipe.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) == -1
        {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(unix)]
fn drain(
    pipe: &mut impl Read,
    bytes: &mut Vec<u8>,
    limit: Option<usize>,
    strict: bool,
) -> io::Result<bool> {
    // Limit work per turn too: a writer continuously filling either pipe must
    // not starve cancellation or the other pipe. Non-strict fetch output keeps
    // its historical bounded-prefix behavior while still draining both pipes.
    let mut chunk = [0; 8192];
    for _ in 0..8 {
        let count = match pipe.read(&mut chunk) {
            Ok(0) => return Ok(true),
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        let remaining = limit.unwrap_or(usize::MAX).saturating_sub(bytes.len());
        if strict && count > remaining {
            return Err(io::Error::other("Git output exceeds its read budget"));
        }
        bytes.extend_from_slice(&chunk[..count.min(remaining)]);
    }
    Ok(false)
}

#[cfg(unix)]
pub(super) fn collect_child_output(
    mut child: CancellableChild,
    cancelled: &AtomicBool,
    child_slot: &Mutex<Option<CancellableChild>>,
    output_limit: Option<usize>,
    strict: bool,
) -> crate::Result<Option<Output>> {
    let mut stdout = child
        .child
        .stdout
        .take()
        .ok_or(crate::Error::InvalidGitOutput)?;
    let mut stderr = child
        .child
        .stderr
        .take()
        .ok_or(crate::Error::InvalidGitOutput)?;
    nonblocking(&stdout)?;
    nonblocking(&stderr)?;
    let _owner = SlotOwner(child_slot);
    *child_slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(child);
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut completed = None;
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Ok(None);
        }
        let out_done = drain(&mut stdout, &mut out, output_limit, strict)?;
        let err_done = drain(&mut stderr, &mut err, output_limit, strict)?;
        if completed.is_none() {
            let mut slot = child_slot.lock().unwrap_or_else(|p| p.into_inner());
            let child = slot.as_mut().ok_or(crate::Error::InvalidGitOutput)?;
            if child.has_exited().map_err(crate::Error::GitUnavailable)? {
                // Even a successful leader can leave a transport holding pipes
                // or writing elsewhere. End its group before releasing its PID.
                let status = child.finish().map_err(crate::Error::GitUnavailable)?;
                completed = Some((status, Instant::now()));
            }
        }
        if let Some((status, finished_at)) = completed {
            if out_done && err_done {
                // Cancellation wins through the output-collection boundary.
                return Ok((!cancelled.load(Ordering::Acquire)).then_some(Output {
                    status,
                    stdout: out,
                    stderr: err,
                }));
            }
            // A deliberately escaped descendant may retain a pipe. Close it
            // and fail rather than accept incomplete output or leave a reader.
            if finished_at.elapsed() >= Duration::from_millis(250) {
                return Err(io::Error::other(
                    "Git output pipes remained open after process cleanup",
                )
                .into());
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{fs, path::Path, sync::mpsc, thread};

    fn wait_until(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(
                Instant::now() < deadline,
                "fixture did not reach its barrier"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn assert_stopped(pid: u32) {
        // An orphan may remain a zombie until the system reaper runs (notably
        // in Linux containers). It cannot run or retain an output descriptor.
        wait_until(|| {
            let output = Command::new("ps")
                .args(["-o", "stat=", "-p", &pid.to_string()])
                .output()
                .unwrap();
            let state = String::from_utf8_lossy(&output.stdout);
            state.trim().is_empty() || state.trim().starts_with('Z')
        });
    }

    fn grandchild_script(root: &Path) -> std::path::PathBuf {
        let script = root.join("transport.sh");
        fs::write(
            &script,
            r#"#!/bin/sh
# Git's SSH capability query must not launch a fixture transport.
for arg in "$@"; do [ "$arg" = -G ] && exit 0; done
cd "$(dirname "$0")" || exit 1
sh -c '
    trap "" TERM
    echo $$ > grandchild.pid
    if (echo bad > /dev/tty) 2>/dev/null; then touch terminal-access; fi
    touch ready
    while :; do sleep 0.05; done
' &
wait
"#,
        )
        .unwrap();
        script
    }

    fn grandchild_pid(root: &Path) -> u32 {
        wait_until(|| root.join("ready").exists());
        fs::read_to_string(root.join("grandchild.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }

    #[test]
    fn process_group_cancellation_retires_grandchildren_and_worker() {
        let temp = tempfile::tempdir().unwrap();
        let script = grandchild_script(temp.path());
        // An extra shell represents Git -> transport -> grandchild.
        let child = CancellableChild::spawn(
            Command::new("sh")
                .arg("-c")
                .arg("sh \"$1\" & wait")
                .arg("git-fixture")
                .arg(script),
        )
        .unwrap();
        let leader = child.child.id();
        // Prove isolation even when the test runner itself has no terminal.
        assert_eq!(unsafe { libc::getpgid(leader as i32) }, leader as i32);
        assert_eq!(unsafe { libc::getsid(leader as i32) }, leader as i32);
        let grandchild = grandchild_pid(temp.path());
        assert!(!temp.path().join("terminal-access").exists());
        let cancelled = AtomicBool::new(false);
        let slot = Mutex::new(None);
        let (sender, receiver) = mpsc::channel();
        thread::scope(|scope| {
            let worker = scope.spawn(|| {
                sender
                    .send(collect_child_output(child, &cancelled, &slot, None, false))
                    .unwrap();
            });
            wait_until(|| slot.lock().unwrap().is_some());
            let started = Instant::now();
            cancelled.store(true, Ordering::Release);
            // Model repeated UI cancellation without taking or reaping the
            // leader. The collector may finish between any two lock attempts.
            for _ in 0..20 {
                if let Some(child) = slot.lock().unwrap().as_mut() {
                    child.cancel();
                }
            }
            assert!(
                receiver
                    .recv_timeout(Duration::from_secs(3))
                    .unwrap()
                    .unwrap()
                    .is_none()
            );
            worker.join().unwrap();
            assert!(started.elapsed() < Duration::from_secs(3));
        });
        assert!(slot.lock().unwrap().is_none());
        assert_stopped(grandchild);
        // The directly owned leader is reaped, not merely killed.
        let mut status = 0;
        assert_eq!(
            unsafe { libc::waitpid(leader as i32, &mut status, libc::WNOHANG) },
            -1
        );
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }

    #[test]
    fn process_group_cancellation_before_slot_publication_cleans_up() {
        let temp = tempfile::tempdir().unwrap();
        let child = CancellableChild::spawn(Command::new("sh").arg(grandchild_script(temp.path())))
            .unwrap();
        let grandchild = grandchild_pid(temp.path());
        let slot = Mutex::new(None);
        assert!(
            collect_child_output(child, &AtomicBool::new(true), &slot, None, false)
                .unwrap()
                .is_none()
        );
        assert!(slot.lock().unwrap().is_none());
        assert_stopped(grandchild);
    }

    #[test]
    fn process_group_completion_kills_pipe_holders_before_reaping_leader() {
        let temp = tempfile::tempdir().unwrap();
        let script = grandchild_script(temp.path());
        let child = CancellableChild::spawn(
            Command::new("sh")
                .arg("-c")
                .arg("sh \"$1\" & while [ ! -f \"$2/ready\" ]; do sleep 0.01; done; printf leader")
                .arg("git-fixture")
                .arg(script)
                .arg(temp.path()),
        )
        .unwrap();
        let grandchild = grandchild_pid(temp.path());
        wait_until(|| child.has_exited().unwrap());
        // Observing completion twice still leaves the PID reserved and the
        // original status waitable while the descendant holds both pipes.
        assert!(child.has_exited().unwrap());
        let started = Instant::now();
        let slot = Mutex::new(None);
        let output = collect_child_output(child, &AtomicBool::new(false), &slot, None, false)
            .unwrap()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"leader");
        assert!(output.stderr.is_empty());
        assert!(slot.lock().unwrap().is_none());
        assert!(started.elapsed() < Duration::from_secs(3));
        assert_stopped(grandchild);
    }

    #[test]
    fn process_group_setup_error_still_cleans_up_descendants() {
        let temp = tempfile::tempdir().unwrap();
        let mut child =
            CancellableChild::spawn(Command::new("sh").arg(grandchild_script(temp.path())))
                .unwrap();
        let grandchild = grandchild_pid(temp.path());
        drop(child.child.stdout.take());
        let slot = Mutex::new(None);
        assert!(collect_child_output(child, &AtomicBool::new(false), &slot, None, false).is_err());
        assert!(slot.lock().unwrap().is_none());
        assert_stopped(grandchild);
    }

    #[test]
    fn process_group_strict_overflow_cleans_up_transport_descendants() {
        let temp = tempfile::tempdir().unwrap();
        let script = grandchild_script(temp.path());
        let body = fs::read_to_string(&script).unwrap();
        fs::write(&script, body.replace("while :; do sleep 0.05; done", "yes")).unwrap();
        let child = CancellableChild::spawn(Command::new("sh").arg(script)).unwrap();
        let grandchild = grandchild_pid(temp.path());
        let slot = Mutex::new(None);
        let started = Instant::now();
        let error = collect_child_output(child, &AtomicBool::new(false), &slot, Some(1024), true)
            .unwrap_err();
        assert!(error.to_string().contains("read budget"));
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(slot.lock().unwrap().is_none());
        assert_stopped(grandchild);
    }

    #[test]
    fn process_group_output_limits_preserve_prefix_and_reject_strict_overflow() {
        for strict in [false, true] {
            let child = CancellableChild::spawn(Command::new("sh").args([
                "-c",
                "head -c 262144 /dev/zero; head -c 262144 /dev/zero >&2",
            ]))
            .unwrap();
            let slot = Mutex::new(None);
            let result =
                collect_child_output(child, &AtomicBool::new(false), &slot, Some(1024), strict);
            if strict {
                assert!(result.unwrap_err().to_string().contains("read budget"));
            } else {
                let output = result.unwrap().unwrap();
                assert!(output.status.success());
                assert_eq!(output.stdout, vec![0; 1024]);
                assert_eq!(output.stderr, vec![0; 1024]);
            }
            assert!(slot.lock().unwrap().is_none());
        }
    }
    #[test]
    fn process_group_repository_and_origin_fetches_cancel_fake_ssh_grandchildren() {
        use crate::git::{self, RepositoryHandle, Upstream};
        use std::os::unix::fs::PermissionsExt;
        for origin in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let script = grandchild_script(temp.path());
            fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
            let config = temp.path().join("user.gitconfig");
            fs::write(&config, format!(
                "[core]\n sshCommand = {}\n[url \"ssh://example.invalid/demo\"]\n insteadOf = https://github.com/example/demo\n",
                script.display()
            )).unwrap();
            let repository = temp.path().join("repository");
            let output = Command::new("git")
                .args(["init", "--quiet"])
                .arg(&repository)
                .env("GIT_CONFIG_GLOBAL", &config)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output()
                .unwrap();
            assert!(output.status.success());
            let cancelled = AtomicBool::new(false);
            let slot = Mutex::new(None);
            let (sender, receiver) = mpsc::channel();
            thread::scope(|scope| {
                let worker = scope.spawn(|| {
                    git::TEST_GIT_CONFIG_GLOBAL
                        .with(|value| *value.borrow_mut() = Some(config.into_os_string()));
                    let result = if origin {
                        git::origin::fetch_snapshot(
                            &temp.path().join("cache"),
                            &crate::provenance::Origin::new(
                                "https://github.com/example/demo".into(),
                                ".".into(),
                            )
                            .unwrap(),
                            "refs/heads/main",
                            &cancelled,
                            &slot,
                        )
                        .map(|output| output.is_none())
                    } else {
                        let handle = RepositoryHandle::open(&repository).unwrap();
                        git::fetch_upstream_cancellable(
                            (&handle).into(),
                            &Upstream {
                                branch: "main".into(),
                                remote: "ssh://example.invalid/demo".into(),
                                merge_ref: "refs/heads/main".into(),
                                tracking_ref: "refs/remotes/origin/main".into(),
                                revision: None,
                            },
                            &cancelled,
                            &slot,
                        )
                        .map(|output| output.is_none())
                        .map_err(|error| error.to_string())
                    };
                    git::TEST_GIT_CONFIG_GLOBAL.with(|value| *value.borrow_mut() = None);
                    sender.send(result).unwrap();
                });
                // If a regression prevents reaching transport, cancel before
                // failing so the scoped worker cannot strand the test suite.
                let deadline = Instant::now() + Duration::from_secs(5);
                while !temp.path().join("ready").exists()
                    && !worker.is_finished()
                    && Instant::now() < deadline
                {
                    thread::sleep(Duration::from_millis(10));
                }
                let ready = temp.path().join("ready").exists();
                cancelled.store(true, Ordering::Release);
                let result = receiver.recv_timeout(Duration::from_secs(3)).unwrap();
                worker.join().unwrap();
                assert!(ready, "transport not reached (origin={origin}): {result:?}");
                assert!(result.unwrap(), "check must return cancellation");
            });
            assert!(slot.lock().unwrap().is_none());
            assert_stopped(grandchild_pid(temp.path()));
            assert!(!temp.path().join("terminal-access").exists());
        }
    }
}
