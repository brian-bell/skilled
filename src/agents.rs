use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

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
        adapter(self).display_name()
    }

    fn index(self) -> usize {
        match self {
            Self::ClaudeCode => 0,
            Self::Codex => 1,
            Self::OpenCode => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DocumentationSnapshot {
    url: &'static str,
    snapshot_date: &'static str,
}

impl DocumentationSnapshot {
    pub fn url(self) -> &'static str {
        self.url
    }

    pub fn snapshot_date(self) -> &'static str {
        self.snapshot_date
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AgentAdapter {
    display_name: &'static str,
    executable_name: &'static str,
    native_skill_root: &'static str,
    compatibility_skill_roots: &'static [&'static str],
    source_catalog_parent_aliases: &'static [&'static str],
    configuration_path: Option<&'static str>,
    documentation: DocumentationSnapshot,
}

impl AgentAdapter {
    pub fn display_name(self) -> &'static str {
        self.display_name
    }

    pub fn executable_name(self) -> &'static str {
        self.executable_name
    }

    pub fn native_skill_root(self) -> &'static str {
        self.native_skill_root
    }

    pub fn compatibility_skill_roots(self) -> &'static [&'static str] {
        self.compatibility_skill_roots
    }

    pub fn supports_source_catalog(self, relative_path: &Path) -> bool {
        if relative_path.file_name() != Some(OsStr::new("skills")) {
            return false;
        }
        std::iter::once(self.native_skill_root)
            .chain(self.compatibility_skill_roots.iter().copied())
            .any(|root| relative_path.ends_with(root))
            || relative_path
                .parent()
                .and_then(Path::file_name)
                .and_then(OsStr::to_str)
                .is_some_and(|parent| self.source_catalog_parent_aliases.contains(&parent))
    }

    pub fn configuration_path(self) -> Option<&'static str> {
        self.configuration_path
    }

    pub fn documentation(self) -> DocumentationSnapshot {
        self.documentation
    }
}

const SNAPSHOT_DATE: &str = "2026-08-02";

const CLAUDE_CODE_ADAPTER: AgentAdapter = AgentAdapter {
    display_name: "Claude Code",
    executable_name: "claude",
    native_skill_root: ".claude/skills",
    compatibility_skill_roots: &[],
    source_catalog_parent_aliases: &["claude", "claude-code"],
    configuration_path: None,
    documentation: DocumentationSnapshot {
        url: "https://code.claude.com/docs/en/slash-commands",
        snapshot_date: SNAPSHOT_DATE,
    },
};

const CODEX_ADAPTER: AgentAdapter = AgentAdapter {
    display_name: "Codex",
    executable_name: "codex",
    native_skill_root: ".agents/skills",
    compatibility_skill_roots: &[],
    source_catalog_parent_aliases: &["codex"],
    configuration_path: Some(".codex/config.toml"),
    documentation: DocumentationSnapshot {
        url: "https://developers.openai.com/codex/skills",
        snapshot_date: SNAPSHOT_DATE,
    },
};

const OPENCODE_ADAPTER: AgentAdapter = AgentAdapter {
    display_name: "OpenCode",
    executable_name: "opencode",
    native_skill_root: ".config/opencode/skills",
    compatibility_skill_roots: &[".agents/skills", ".claude/skills"],
    source_catalog_parent_aliases: &["opencode"],
    configuration_path: None,
    documentation: DocumentationSnapshot {
        url: "https://opencode.ai/docs/skills",
        snapshot_date: SNAPSHOT_DATE,
    },
};

pub fn adapter(kind: AgentKind) -> &'static AgentAdapter {
    match kind {
        AgentKind::ClaudeCode => &CLAUDE_CODE_ADAPTER,
        AgentKind::Codex => &CODEX_ADAPTER,
        AgentKind::OpenCode => &OPENCODE_ADAPTER,
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
        let adapter = adapter(kind);
        let root = environment.home_dir.join(adapter.native_skill_root());
        AgentDetection {
            kind,
            root_exists: fs::metadata(&root).is_ok_and(|metadata| metadata.is_dir()),
            root,
            executable_path: find_executable(
                &environment.executable_path,
                adapter.executable_name(),
            ),
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
