use std::{ffi::OsString, path::PathBuf, process::Command};

use directories::{ProjectDirs, UserDirs};

use crate::{Error, Result};

/// Who and where this session is, as far as the process can honestly tell.
///
/// Every segment is optional: a value the environment does not provide is
/// omitted from the title bar rather than invented. Gathered once at startup —
/// by [`SessionIdentity::for_process`] on the interactive path in production,
/// by injection in tests, which never read the real environment — so the
/// reducer and renderer stay free of process and filesystem work.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SessionIdentity {
    pub user: Option<String>,
    pub host: Option<String>,
    pub os: Option<String>,
}

/// Where a `hostname` executable is expected to be found, addressed
/// absolutely: resolving the name through `PATH` would execute whatever
/// binary the caller's environment put first, with this process's
/// privileges, on every launch. macOS keeps it in `/bin`; Linux
/// distributions in `/bin` or `/usr/bin`. Where neither exists — native
/// Windows among them — the host segment is omitted rather than searched
/// for.
const HOSTNAME_COMMANDS: [&str; 2] = ["/bin/hostname", "/usr/bin/hostname"];

impl SessionIdentity {
    /// The identity the interactive session can observe, each segment
    /// omitted when the environment does not provide it.
    ///
    /// The hostname comes from a one-shot command at startup — no new
    /// dependency, and running it here rather than at render time keeps
    /// `update` free of process work. Only the interactive application calls
    /// this: `skilled install` never shows identity, so it never runs the
    /// command at all.
    pub fn for_process() -> Self {
        let non_empty = |value: String| {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_owned())
            }
        };
        Self {
            user: std::env::var("USER")
                .or_else(|_| std::env::var("LOGNAME"))
                // Native Windows spells the same fact USERNAME.
                .or_else(|_| std::env::var("USERNAME"))
                .ok()
                .and_then(non_empty),
            host: HOSTNAME_COMMANDS.iter().find_map(|command| {
                Command::new(command)
                    .output()
                    .ok()
                    .filter(|output| output.status.success())
                    .and_then(|output| String::from_utf8(output.stdout).ok())
                    .and_then(non_empty)
            }),
            os: os_label(std::env::consts::OS).map(str::to_owned),
        }
    }
}

#[derive(Clone, Debug)]
pub struct AppEnvironment {
    pub home_dir: PathBuf,
    pub data_dir: PathBuf,
    pub executable_path: OsString,
    pub identity: SessionIdentity,
}

impl AppEnvironment {
    pub fn new(
        home_dir: impl Into<PathBuf>,
        data_dir: impl Into<PathBuf>,
        executable_path: impl Into<OsString>,
    ) -> Self {
        Self {
            home_dir: home_dir.into(),
            data_dir: data_dir.into(),
            executable_path: executable_path.into(),
            identity: SessionIdentity::default(),
        }
    }

    /// The same environment carrying this session identity, builder-style so
    /// the many existing `new()` call sites do not gain a parameter they would
    /// all fill with the default.
    #[must_use]
    pub fn with_identity(mut self, identity: SessionIdentity) -> Self {
        self.identity = identity;
        self
    }

    pub fn for_process() -> Result<Self> {
        let home_dir = UserDirs::new()
            .map(|directories| directories.home_dir().to_owned())
            .ok_or(Error::HomeDirectoryUnavailable)?;
        let data_dir = ProjectDirs::from("", "", "skilled")
            .map(|directories| directories.data_dir().to_owned())
            .ok_or(Error::DataDirectoryUnavailable)?;
        let executable_path = std::env::var_os("PATH").unwrap_or_default();
        Ok(Self::new(home_dir, data_dir, executable_path))
    }
}

/// The user-facing name of an operating system Skilled knows how to name.
///
/// Only the platforms the prototype speaks of are mapped; any other value is
/// omitted rather than shown as a raw identifier the title bar never promised.
fn os_label(os: &str) -> Option<&'static str> {
    match os {
        "macos" => Some("macOS"),
        "linux" => Some("Linux"),
        "windows" => Some("Windows"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_environment_starts_with_no_identity_and_takes_an_injected_one() {
        let environment = AppEnvironment::new("/home", "/data", "");
        assert_eq!(environment.identity, SessionIdentity::default());

        let identity = SessionIdentity {
            user: Some("brian".to_owned()),
            host: Some("macbook".to_owned()),
            os: Some("macOS".to_owned()),
        };
        let environment = environment.with_identity(identity.clone());
        assert_eq!(environment.identity, identity);
    }

    #[test]
    fn os_labels_map_only_the_operating_systems_skilled_names() {
        assert_eq!(os_label("macos"), Some("macOS"));
        assert_eq!(os_label("linux"), Some("Linux"));
        assert_eq!(os_label("windows"), Some("Windows"));
        // An unrecognised value is omitted rather than shown raw or invented.
        assert_eq!(os_label("freebsd"), None);
        assert_eq!(os_label(""), None);
    }
}
