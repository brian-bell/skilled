use std::{ffi::OsStr, fs, path::PathBuf};

use crate::AppEnvironment;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentKind {
    ClaudeCode,
    Codex,
    OpenCode,
}

impl AgentKind {
    pub const ALL: [Self; 3] = [Self::ClaudeCode, Self::Codex, Self::OpenCode];

    pub fn display_name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
            Self::OpenCode => "OpenCode",
        }
    }

    fn executable_name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude",
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
        }
    }

    fn root_relative_to_home(self) -> &'static str {
        match self {
            Self::ClaudeCode => ".claude/skills",
            Self::Codex => ".agents/skills",
            Self::OpenCode => ".config/opencode/skills",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::ClaudeCode => 0,
            Self::Codex => 1,
            Self::OpenCode => 2,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentDetection {
    kind: AgentKind,
    root: PathBuf,
    root_exists: bool,
    executable_path: Option<PathBuf>,
    selected: bool,
}

impl AgentDetection {
    pub fn kind(&self) -> AgentKind {
        self.kind
    }

    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    pub fn root_exists(&self) -> bool {
        self.root_exists
    }

    pub fn executable_path(&self) -> Option<&std::path::Path> {
        self.executable_path.as_deref()
    }

    pub fn selected(&self) -> bool {
        self.selected
    }

    pub(crate) fn toggle_selected(&mut self) {
        self.selected = !self.selected;
    }

    pub(crate) fn set_selected(&mut self, selected: bool) {
        self.selected = selected;
    }
}

pub(crate) fn detect_agents(environment: &AppEnvironment) -> [AgentDetection; 3] {
    AgentKind::ALL.map(|kind| {
        let root = environment.home_dir.join(kind.root_relative_to_home());
        AgentDetection {
            kind,
            root_exists: fs::metadata(&root).is_ok_and(|metadata| metadata.is_dir()),
            root,
            executable_path: find_executable(&environment.executable_path, kind.executable_name()),
            selected: true,
        }
    })
}

fn find_executable(search_path: &OsStr, name: &str) -> Option<PathBuf> {
    std::env::split_paths(search_path)
        .map(|directory| directory.join(name))
        .find(|candidate| is_executable(candidate))
}

#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &std::path::Path) -> bool {
    fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
}

pub(crate) fn detection_at(detections: &[AgentDetection; 3], kind: AgentKind) -> &AgentDetection {
    &detections[kind.index()]
}
