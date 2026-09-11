//! Real Git regressions for auxiliary bundle fetching during explicit checks.

use super::*;
use crate::{AppEnvironment, SkilledApp, updates};
use std::fs;
use std::os::unix::fs::PermissionsExt;

struct Fixture {
    temporary: tempfile::TempDir,
    seed: PathBuf,
    checkout: PathBuf,
    bundle: PathBuf,
    global: PathBuf,
    previous_global: Option<OsString>,
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let seed = temporary.path().join("seed");
        let checkout = temporary.path().join("checkout");
        let bundle = temporary.path().join("data.bundle");
        let global = temporary.path().join("global.config");
        fs::write(&global, "").unwrap();
        let previous_global =
            TEST_GIT_CONFIG_GLOBAL.with(|value| value.replace(Some(global.as_os_str().to_owned())));
        let fixture = Self {
            temporary,
            seed,
            checkout,
            bundle,
            global,
            previous_global,
        };
        fixture.git(fixture.temporary.path(), &["init", "-b", "main", "seed"]);
        fs::create_dir_all(fixture.seed.join("skills/demo")).unwrap();
        fs::write(
            fixture.seed.join("skills/demo/SKILL.md"),
            "---\nname: demo\ndescription: fixture\n---\n",
        )
        .unwrap();
        fixture.commit("base");
        fixture.git(
            fixture.temporary.path(),
            &[
                "clone",
                "--branch",
                "main",
                fixture.seed.to_str().unwrap(),
                "checkout",
            ],
        );
        fs::write(fixture.seed.join("skills/demo/new.txt"), "incoming\n").unwrap();
        fixture.commit("incoming");
        fixture.git(
            &fixture.seed,
            &[
                "bundle",
                "create",
                fixture.bundle.to_str().unwrap(),
                "--all",
            ],
        );
        fixture
    }

    fn isolate(&self, command: &mut Command) {
        for (key, _) in env::vars_os().filter(|(key, _)| key.to_string_lossy().starts_with("GIT_"))
        {
            command.env_remove(key);
        }
        command
            .env("HOME", self.temporary.path())
            .env("XDG_CONFIG_HOME", self.temporary.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", &self.global)
            .env("GIT_TERMINAL_PROMPT", "0");
    }

    fn git(&self, directory: &Path, args: &[&str]) -> String {
        let mut command = Command::new("git");
        self.isolate(&mut command);
        let output = command
            .arg("-C")
            .arg(directory)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn commit(&self, message: &str) {
        self.git(&self.seed, &["add", "."]);
        self.git(
            &self.seed,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.test",
                "commit",
                "-m",
                message,
            ],
        );
    }

    fn configure_bundle(&self, list: bool) {
        let uri = if list {
            let list_path = self.temporary.path().join("bundles.list");
            self.git(
                self.temporary.path(),
                &[
                    "config",
                    "--file",
                    list_path.to_str().unwrap(),
                    "bundle.version",
                    "1",
                ],
            );
            for (key, value) in [
                ("bundle.mode", "all"),
                ("bundle.heuristic", "creationToken"),
                ("bundle.latest.uri", self.bundle.to_str().unwrap()),
                ("bundle.latest.creationToken", "2"),
            ] {
                self.git(
                    self.temporary.path(),
                    &["config", "--file", list_path.to_str().unwrap(), key, value],
                );
            }
            list_path
        } else {
            self.bundle.clone()
        };
        // Duplicate local and global values must all lose to the spawn override.
        for args in [
            vec![
                "config",
                "--global",
                "fetch.bundleURI",
                uri.to_str().unwrap(),
            ],
            vec!["config", "--add", "fetch.bundleURI", ""],
            vec!["config", "--add", "fetch.bundleURI", uri.to_str().unwrap()],
        ] {
            self.git(&self.checkout, &args);
        }
        self.git(
            &self.checkout,
            &["config", "fetch.bundleCreationToken", "1"],
        );
        self.git(&self.checkout, &["config", "transfer.bundleURI", "true"]);
    }

    fn refs(&self) -> String {
        self.git(
            &self.checkout,
            &[
                "for-each-ref",
                "--format=%(refname) %(objectname) %(symref)",
            ],
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        TEST_GIT_CONFIG_GLOBAL.with(|value| *value.borrow_mut() = self.previous_global.take());
    }
}

#[test]
fn explicit_checks_suppress_bundle_refs_and_creation_token_writes() {
    for cancellable in [false, true] {
        for list in [false, true] {
            let fixture = Fixture::new();
            let mut app = SkilledApp::open(AppEnvironment::new(
                fixture.temporary.path().join("home"),
                fixture.temporary.path().join("data"),
                "",
            ))
            .unwrap();
            let preview = app.preview_source(&fixture.checkout).unwrap();
            app.confirm_source(preview).unwrap();
            fixture.git(&fixture.checkout, &["branch", "protected"]);
            fixture.git(&fixture.checkout, &["tag", "keep"]);
            fixture.git(
                &fixture.checkout,
                &["update-ref", "refs/bundles/heads/keep", "HEAD"],
            );
            fixture.git(
                &fixture.checkout,
                &[
                    "symbolic-ref",
                    "refs/bundles/heads/main",
                    "refs/heads/protected",
                ],
            );
            fixture.configure_bundle(list);
            let hook_marker = fixture.temporary.path().join("traditional-hook-ran");
            let configured_marker = fixture.temporary.path().join("configured-hook-ran");
            let hook = fixture.checkout.join(".git/hooks/reference-transaction");
            fs::write(
                &hook,
                format!("#!/bin/sh\necho invoked > '{}'\n", hook_marker.display()),
            )
            .unwrap();
            fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
            fixture.git(
                &fixture.checkout,
                &[
                    "config",
                    "hook.bundle-test.command",
                    &format!("echo invoked > '{}'", configured_marker.display()),
                ],
            );
            fixture.git(
                &fixture.checkout,
                &["config", "hook.bundle-test.event", "reference-transaction"],
            );
            fixture.git(
                &fixture.checkout,
                &["config", "hook.reference-transaction.enabled", "true"],
            );
            // A real ref transaction makes the traditional fixture a positive
            // control and discovers configured-hook support on this Git.
            fixture.git(
                &fixture.checkout,
                &["update-ref", "refs/test/hook-control", "HEAD"],
            );
            assert!(hook_marker.exists());
            let configured_hooks_supported = configured_marker.exists();
            fs::remove_file(&hook_marker).unwrap();
            if configured_hooks_supported {
                fs::remove_file(&configured_marker).unwrap();
            }
            let before = fixture.refs();
            let old = fixture.git(
                &fixture.checkout,
                &["rev-parse", "refs/remotes/origin/main"],
            );
            let incoming = fixture.git(&fixture.seed, &["rev-parse", "HEAD"]);
            let head = fs::read(fixture.checkout.join(".git/HEAD")).unwrap();
            let config = fs::read(fixture.checkout.join(".git/config")).unwrap();
            let global = fs::read(&fixture.global).unwrap();
            let fetch_head = fixture.checkout.join(".git/FETCH_HEAD");
            fs::write(&fetch_head, "user fetch state\n").unwrap();
            let slot = Mutex::new(None);
            let probe = if cancellable {
                updates::probe_repository_update_cancellable(
                    &app.sources()[0],
                    &AtomicBool::new(false),
                    &slot,
                )
                .unwrap()
            } else {
                updates::probe_repository_update(&app.sources()[0], true)
            };
            assert_eq!(
                updates::classify_repository_update(&probe).0,
                updates::RepositoryUpdateVerdict::Available,
                "{probe:?}"
            );
            // The symbolic origin/HEAD is unchanged, but its displayed OID
            // follows the one tracking ref the check is allowed to advance.
            let expected = before
                .replace(
                    &format!("refs/remotes/origin/main {old}"),
                    &format!("refs/remotes/origin/main {incoming}"),
                )
                .replace(
                    &format!("refs/remotes/origin/HEAD {old}"),
                    &format!("refs/remotes/origin/HEAD {incoming}"),
                );
            assert_eq!(
                fixture.refs(),
                expected,
                "cancellable={cancellable}, list={list}"
            );
            assert_eq!(fs::read(fixture.checkout.join(".git/HEAD")).unwrap(), head);
            assert_eq!(
                fs::read(fixture.checkout.join(".git/config")).unwrap(),
                config
            );
            assert_eq!(fs::read(&fixture.global).unwrap(), global);
            assert_eq!(
                fs::read_to_string(fetch_head).unwrap(),
                "user fetch state\n"
            );
            assert!(slot.lock().unwrap().is_none());
            assert!(
                !hook_marker.exists(),
                "traditional hook ran during the check"
            );
            assert!(
                !configured_marker.exists(),
                "configured hook ran during the check (supported={configured_hooks_supported})"
            );
        }
    }
}

#[test]
fn unsuppressed_creation_token_bundle_list_is_a_live_positive_control() {
    let fixture = Fixture::new();
    fixture.configure_bundle(true);
    fixture.git(
        &fixture.checkout,
        &[
            "fetch",
            "--dry-run",
            "--no-auto-maintenance",
            "--no-write-fetch-head",
            "origin",
        ],
    );
    assert!(fixture.refs().contains("refs/bundles/heads/main"));
    assert_eq!(
        fixture.git(
            &fixture.checkout,
            &["config", "--local", "fetch.bundleCreationToken"]
        ),
        "2"
    );
}

#[test]
fn confirmed_merge_keeps_its_bundle_configuration() {
    let args = UpdateOp::Merge("HEAD".into()).arguments();
    assert!(!args.iter().any(|arg| {
        arg.to_string_lossy()
            .to_ascii_lowercase()
            .contains("bundleuri")
    }));
}

#[test]
fn a_bundle_configured_after_preflight_is_suppressed_at_fetch_spawn() {
    let fixture = Fixture::new();
    let repository = fixture.checkout.as_path().into();
    let head = head_state(repository).unwrap();
    let upstream = upstream_of(repository, &head).unwrap().unwrap();
    assert!(repository_transport_code(repository).unwrap().is_none());
    fixture.configure_bundle(false);
    let incoming = fetch_upstream(repository, &upstream).unwrap();
    assert_eq!(incoming, fixture.git(&fixture.seed, &["rev-parse", "HEAD"]));
    assert!(!fixture.refs().contains("refs/bundles/"));
}

/// A loopback-only bundle server with bounded reads and deterministic shutdown.
struct BundleServer {
    uri: String,
    requests: Arc<AtomicUsize>,
    stopped: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl BundleServer {
    fn new(bytes: Vec<u8>) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let uri = format!("http://{}/data.bundle", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let stopped = Arc::new(AtomicBool::new(false));
        let count = requests.clone();
        let stop = stopped.clone();
        let worker = thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        stream
                            .set_write_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let mut header = Vec::new();
                        while header.len() < 8192 && !header.ends_with(b"\r\n\r\n") {
                            let mut byte = [0];
                            if stream.read(&mut byte).unwrap_or(0) == 0 {
                                break;
                            }
                            header.push(byte[0]);
                        }
                        if header.starts_with(b"GET /data.bundle ") {
                            count.fetch_add(1, Ordering::AcqRel);
                            write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len()).unwrap();
                            stream.write_all(&bytes).unwrap();
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(error) => panic!("bundle server: {error}"),
                }
            }
        });
        Self {
            uri,
            requests,
            stopped,
            worker: Some(worker),
        }
    }
}

impl Drop for BundleServer {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        self.worker.take().unwrap().join().unwrap();
    }
}

#[test]
fn bundle_suppression_prevents_http_helpers_even_when_http_is_permitted() {
    for indirect in [false, true] {
        // The controls establish that the bundle is usable and distinguish
        // helper launch from actual network access under an inherited policy.
        for (suppressed, allowed, expected_requests, expected_helper) in [
            (false, "file:http:https", 1, true),
            (false, "file", 0, true),
            (true, "file:http:https", 0, false),
            (true, "file", 0, false),
        ] {
            let fixture = Fixture::new();
            let server = BundleServer::new(fs::read(&fixture.bundle).unwrap());
            let list = fixture.temporary.path().join("http.list");
            fs::write(
                &list,
                format!(
                    "[bundle]\nversion = 1\nmode = all\n[bundle \"one\"]\nuri = {}\n",
                    server.uri
                ),
            )
            .unwrap();
            let uri = if indirect {
                list.to_str().unwrap()
            } else {
                &server.uri
            };
            fixture.git(&fixture.checkout, &["config", "fetch.bundleURI", uri]);
            let repository = fixture.checkout.as_path().into();
            let upstream = upstream_of(repository, &head_state(repository).unwrap())
                .unwrap()
                .unwrap();
            let destination = "refs/skilled/fetch/bundle-test";
            let op = fetch_op(
                &upstream,
                FetchTransport {
                    ssh_command: "ssh -o BatchMode=yes".into(),
                    allowed_protocols: allowed.into(),
                },
                destination,
            );
            let mut child = if suppressed {
                command(repository, &op)
            } else {
                // Deliberately omit Skilled's configuration overrides in the
                // positive control; these are the original audited fetch flags.
                let mut child = Command::new("git");
                child.arg("-C").arg(&fixture.checkout).args([
                    "-c",
                    "core.hooksPath=/dev/null",
                    "-c",
                    "core.fsmonitor=false",
                    "fetch",
                    "--porcelain",
                    "--dry-run",
                    "--no-auto-maintenance",
                    "--no-write-fetch-head",
                    "--no-tags",
                    "--no-prune",
                    "--no-prune-tags",
                    "--recurse-submodules=no",
                    "--refmap=",
                    "--",
                    "origin",
                    &format!("+refs/heads/main:{destination}"),
                ]);
                child
            };
            fixture.isolate(&mut child);
            child
                .envs(op.environment())
                .env("GIT_TRACE", "1")
                .env("GIT_CONFIG_COUNT", "2")
                .env("GIT_CONFIG_KEY_0", "fetch.bundleURI")
                .env("GIT_CONFIG_VALUE_0", uri)
                .env("GIT_CONFIG_KEY_1", "transfer.bundleURI")
                .env("GIT_CONFIG_VALUE_1", "true");
            let output = child.output().unwrap();
            let trace = String::from_utf8_lossy(&output.stderr);
            assert!(output.status.success(), "{trace}");
            assert!(
                reported_revision(&output.stdout, destination).is_some(),
                "normal fetch report missing"
            );
            assert_eq!(
                trace.contains("git-remote-https"),
                expected_helper,
                "{trace}"
            );
            assert_eq!(server.requests.load(Ordering::Acquire), expected_requests);
            assert_eq!(
                fixture.refs().contains("refs/bundles/"),
                expected_requests == 1
            );
        }
    }
}

#[test]
fn origin_caches_ignore_global_bundles_and_still_read_the_fetched_tree() {
    for http in [false, true] {
        let fixture = Fixture::new();
        let server = BundleServer::new(fs::read(&fixture.bundle).unwrap());
        let uri = if http {
            &server.uri
        } else {
            fixture.bundle.to_str().unwrap()
        };
        fixture.git(
            &fixture.checkout,
            &["config", "--global", "fetch.bundleURI", uri],
        );
        fixture.git(
            &fixture.checkout,
            &["config", "--global", "transfer.bundleURI", "true"],
        );
        fixture.git(
            &fixture.checkout,
            &[
                "config",
                "--global",
                &format!("url.file://{}.insteadOf", fixture.seed.display()),
                "https://github.com/example/demo",
            ],
        );
        let global = fs::read(&fixture.global).unwrap();
        let cache_root = fixture.temporary.path().join("cache");
        let snapshot = origin::fetch_snapshot(
            &cache_root,
            &crate::provenance::Origin::new(
                "https://github.com/example/demo".into(),
                "skills/demo".into(),
            )
            .unwrap(),
            "refs/heads/main",
            &AtomicBool::new(false),
            &Mutex::new(None),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            snapshot.revision,
            fixture.git(&fixture.seed, &["rev-parse", "HEAD"])
        );
        assert!(
            snapshot
                .entries
                .iter()
                .any(|entry| entry.path == Path::new("new.txt") && entry.bytes == b"incoming\n")
        );
        assert_eq!(server.requests.load(Ordering::Acquire), 0);
        assert_eq!(fs::read(&fixture.global).unwrap(), global);
        let caches: Vec<_> = fs::read_dir(cache_root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(caches.len(), 1);
        assert_eq!(
            fixture.git(&caches[0], &["for-each-ref", "--format=%(refname)"]),
            ""
        );
        assert!(!caches[0].join("FETCH_HEAD").exists());
        let config = fs::read_to_string(caches[0].join("config")).unwrap();
        assert!(!config.to_ascii_lowercase().contains("bundlecreationtoken"));
    }
}
