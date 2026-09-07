//! Bounded local attribution evidence and explicit origin resolution.
//!
//! Parsing never contacts an origin. Raw input fingerprints remain part of
//! evidence equality even when distinct inputs suggest the same origin.

mod baseline;
pub(crate) use baseline::observe_directory_hash;
use baseline::open_file_without_following;
pub use baseline::{Baseline, directory_hash};

use std::{
    collections::BTreeSet,
    ffi::OsStr,
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
};

use serde::Deserialize;
use sha2::{Digest, Sha256};

/// A completed observation can disagree with a valid preview even when it
/// cannot produce a digest (for example an observed limit or type violation).
/// Preserve that distinction before formatting operating-system errors.
#[derive(Debug)]
pub(crate) enum ObservationFailure {
    Changed(String),
    Unavailable(String),
}
impl ObservationFailure {
    pub(crate) fn io(message: String, error: io::Error) -> Self {
        // No-follow opens report ELOOP when an expected physical entry has
        // become a link. Keep that observed path disagreement out of I/O unknowns.
        #[cfg(unix)]
        let redirected = error.raw_os_error() == Some(libc::ELOOP);
        #[cfg(not(unix))]
        let redirected = false;
        if redirected
            || matches!(
                error.kind(),
                io::ErrorKind::NotFound
                    | io::ErrorKind::NotADirectory
                    | io::ErrorKind::IsADirectory
            )
        {
            Self::Changed(message)
        } else {
            Self::Unavailable(message)
        }
    }
}
impl From<String> for ObservationFailure {
    fn from(message: String) -> Self {
        Self::Changed(message)
    }
}
impl From<&str> for ObservationFailure {
    fn from(message: &str) -> Self {
        Self::Changed(message.into())
    }
}
impl std::fmt::Display for ObservationFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Changed(message) | Self::Unavailable(message) => message.fmt(f),
        }
    }
}
impl std::error::Error for ObservationFailure {}

const MAX_EVIDENCE_BYTES: usize = 1024 * 1024;
const MAX_EVIDENCE_CANDIDATES: usize = 128;
const CANDIDATE_LIMIT_PROBLEM: &str = "provenance evidence exceeds 128 distinct origin candidates";

/// A local hint may identify a repository without identifying its skill path.
/// `None` is unknown; `Some(".")` explicitly names the repository root.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct OriginHint {
    pub repository: String,
    pub subdirectory: Option<String>,
}

/// A complete, validated origin declaration. It does not prove a revision.
/// Adoption confirms this declaration only after the full preview is visible.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct Origin {
    repository: String,
    subdirectory: String,
}

impl Origin {
    /// Validate exact values, including schema 12 values read from storage.
    /// Stored values are never silently trimmed or otherwise repaired.
    pub fn new(repository: String, subdirectory: String) -> Result<Self, String> {
        if !valid_repository(&repository) {
            return Err("origin repository must be an https://github.com/owner/name URL".into());
        }
        if !valid_subdirectory(&subdirectory) {
            return Err("origin subdirectory is not a safe relative path".into());
        }
        Ok(Self {
            repository,
            subdirectory,
        })
    }

    /// Form whitespace is ignored, but an unknown path must be entered.
    pub fn from_input(repository: &str, subdirectory: &str) -> Result<Self, String> {
        Self::new(repository.trim().into(), subdirectory.trim().into())
    }

    pub fn repository(&self) -> &str {
        &self.repository
    }
    pub fn subdirectory(&self) -> &str {
        &self.subdirectory
    }
}

/// Evidence found without accessing the network. Problems block resolution;
/// missing files and unknown paths are distinct from unreadable evidence.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Evidence {
    pub candidates: Vec<OriginHint>,
    pub problems: Vec<String>,
    fingerprints: Vec<(PathBuf, Option<String>)>,
    unavailable_read: bool,
    invalid_read: bool,
}

impl Evidence {
    /// Known changed bytes or an observed invalid input take precedence over
    /// another file whose read was unavailable. The preview had complete evidence.
    pub(crate) fn change_is_unavailable(&self, before: &Self) -> bool {
        self.unavailable_read
            && !self.invalid_read
            && !self.fingerprints.iter().any(|(path, digest)| {
                before
                    .fingerprints
                    .iter()
                    .any(|(old_path, old_digest)| path == old_path && digest != old_digest)
            })
    }

    pub fn is_ambiguous(&self) -> bool {
        self.candidates.len() > 1
    }

    /// Preserve the form's single-hint defaults, including an empty unknown path.
    pub fn suggested_fields(&self) -> [String; 3] {
        let mut fields = [String::new(), String::new(), String::new()];
        if let [hint] = self.candidates.as_slice() {
            fields[0] = hint.repository.clone();
            fields[1] = hint.subdirectory.clone().unwrap_or_default();
        }
        fields
    }

    /// Resolve a validated selection against all hints. Bare hints allow an
    /// explicitly entered path only if no hint for that repository knows one.
    /// Multiple repositories or paths require one exact supported selection.
    pub fn resolve(&self, origin: Origin) -> Result<Origin, String> {
        if !self.problems.is_empty() {
            return Err(
                "Origin evidence is incomplete; resolve its reported problems before adoption"
                    .into(),
            );
        }
        if !self.candidates.is_empty() {
            let mut repository_found = false;
            let mut known_path = false;
            let mut path_matches = false;
            for hint in &self.candidates {
                if hint.repository == origin.repository {
                    repository_found = true;
                    if let Some(path) = &hint.subdirectory {
                        known_path = true;
                        path_matches |= path == &origin.subdirectory;
                    }
                }
            }
            if !repository_found || (known_path && !path_matches) {
                return Err(
                    "Choose one hinted repository and match its subdirectory when specified".into(),
                );
            }
        }
        Ok(origin)
    }
}

/// Checks a narrow, fully-qualified branch ref for a later explicit update
/// check. An attribution commit is not accepted here because adoption does
/// not claim any historical revision.
pub fn validate_update_ref(reference: &str) -> Result<(), String> {
    let Some(branch) = reference.strip_prefix("refs/heads/") else {
        return Err("origin update reference must be a refs/heads/ branch".into());
    };
    if valid_git_path(branch) {
        Ok(())
    } else {
        Err("origin update reference is not a safe branch name".into())
    }
}

/// Reads supported local attribution and lock-hint formats for exactly one
/// skill. Absent files contribute no evidence; unreadable or invalid inputs
/// remain distinguishable in `problems`.
pub fn read_evidence(source_root: &Path, skill_path: &Path, skill_name: &str) -> Evidence {
    let mut evidence = Evidence::default();
    if !valid_skill_name(skill_name) {
        evidence
            .problems
            .push("skill name is not safe for provenance lookup".into());
        return evidence;
    }

    match root_table_binding(source_root, skill_path, skill_name) {
        Ok(true) => read_root_attribution(source_root, skill_name, &mut evidence),
        Ok(false) => {}
        Err(problem) => evidence.problems.push(problem),
    }
    read_skill_attribution(skill_path, &mut evidence);
    read_lock_hint(source_root, skill_path, skill_name, &mut evidence);

    evidence.candidates.sort();
    evidence
}

fn root_table_binding(
    source_root: &Path,
    skill_path: &Path,
    skill_name: &str,
) -> Result<bool, String> {
    let Ok(relative) = skill_path.strip_prefix(source_root) else {
        return Ok(false);
    };
    let components = relative.components().collect::<Vec<_>>();
    let is_named = |component: Option<&std::path::Component<'_>>| matches!(component, Some(std::path::Component::Normal(name)) if *name == OsStr::new(skill_name));
    let alternative = match components.as_slice() {
        [component] if is_named(Some(component)) => source_root.join("skills").join(skill_name),
        [std::path::Component::Normal(directory), component]
            if *directory == OsStr::new("skills") && is_named(Some(component)) =>
        {
            source_root.join(skill_name)
        }
        _ => return Ok(false),
    };
    if physical_skill_directory(&alternative) {
        Err(
            "root attribution cannot distinguish two common skill directories with this name"
                .into(),
        )
    } else {
        Ok(true)
    }
}

fn physical_skill_directory(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
        && fs::symlink_metadata(path.join("SKILL.md"))
            .is_ok_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
}

fn read_root_attribution(source_root: &Path, skill_name: &str, evidence: &mut Evidence) {
    let path = source_root.join("ATTRIBUTION.md");
    let Ok(text) = read_optional_text(&path, evidence, "root attribution") else {
        return;
    };
    let Some(text) = text else { return };
    let mut in_table = false;
    let mut columns = None;
    for line in text.lines() {
        if !is_table_row(line) {
            in_table = false;
            columns = None;
            continue;
        }
        let cells: Vec<_> = line
            .trim()
            .trim_matches('|')
            .split('|')
            .map(str::trim)
            .collect();
        if !in_table {
            in_table = true;
            let skill_columns: Vec<_> = cells
                .iter()
                .enumerate()
                .filter_map(|(i, cell)| cell.eq_ignore_ascii_case("skill").then_some(i))
                .collect();
            let source_columns: Vec<_> = cells
                .iter()
                .enumerate()
                .filter_map(|(i, cell)| cell.eq_ignore_ascii_case("source").then_some(i))
                .collect();
            columns = match (skill_columns.as_slice(), source_columns.as_slice()) {
                ([skill], [source]) => Some((*skill, *source)),
                _ => None,
            };
            continue;
        }
        let Some((skill_column, source_column)) = columns else {
            continue;
        };
        if cells
            .get(skill_column)
            .is_some_and(|cell| markdown_code(cell) == Some(skill_name))
        {
            match cells
                .get(source_column)
                .and_then(|cell| parse_origin_url(cell))
            {
                Some(origin) => push_candidate(evidence, origin),
                None => evidence
                    .problems
                    .push("matching root attribution source is malformed or unsupported".into()),
            }
        }
    }
}

fn read_skill_attribution(skill_path: &Path, evidence: &mut Evidence) {
    let path = skill_path.join("ATTRIBUTION.md");
    let Ok(text) = read_optional_text(&path, evidence, "skill attribution") else {
        return;
    };
    let Some(text) = text else { return };
    let mut origins = BTreeSet::new();
    for line in text.lines() {
        let line = line
            .trim_start()
            .strip_prefix("- ")
            .unwrap_or(line.trim_start());
        if line.starts_with("Upstream:") || line.starts_with("Source snapshot:") {
            let Some(origin) = parse_origin_url(line) else {
                evidence
                    .problems
                    .push("skill attribution origin is malformed or unsupported".into());
                continue;
            };
            if origins.len() == MAX_EVIDENCE_CANDIDATES && !origins.contains(&origin) {
                evidence.candidates.clear();
                evidence.problems.push(CANDIDATE_LIMIT_PROBLEM.into());
                return;
            }
            origins.insert(origin);
        }
    }
    let pinned_repositories: BTreeSet<String> = origins
        .iter()
        .filter(|origin| origin.subdirectory.is_some())
        .map(|origin| origin.repository.clone())
        .collect();
    for origin in origins {
        if origin.subdirectory.is_some() || !pinned_repositories.contains(&origin.repository) {
            push_candidate(evidence, origin);
        }
    }
}

fn read_lock_hint(
    source_root: &Path,
    skill_path: &Path,
    skill_name: &str,
    evidence: &mut Evidence,
) {
    let path = source_root.join("skills-lock.json");
    let Ok(text) = read_optional_text(&path, evidence, "skills lock") else {
        return;
    };
    let Some(text) = text else { return };
    let lock: LockFile = match serde_json::from_str(&text) {
        Ok(lock) => lock,
        Err(error) => {
            evidence
                .problems
                .push(format!("skills lock is invalid: {error}"));
            return;
        }
    };
    if lock.version != 1 {
        evidence.problems.push(format!(
            "skills lock version {} is unsupported",
            lock.version
        ));
        return;
    }
    let Some(entry) = lock.skills.get(skill_name) else {
        return;
    };
    if entry.source_type.as_deref() != Some("github") || !valid_repository_name(&entry.source) {
        evidence
            .problems
            .push("matching skills lock entry has unsupported origin".into());
        return;
    }
    let Some(expected) = lock_skill_directory(source_root, &entry.skill_path) else {
        evidence
            .problems
            .push("matching skills lock entry has an unsafe skill path".into());
        return;
    };
    if expected != skill_path {
        evidence
            .problems
            .push("matching skills lock entry does not name this exact skill path".into());
        return;
    }
    // A ref in a v1 lock can be `main`; it is a routing hint, never proof of
    // a revision.  Intentionally do not read it into `Origin`.
    push_candidate(
        evidence,
        OriginHint {
            repository: format!("https://github.com/{}", entry.source),
            subdirectory: Some(
                skill_path
                    .strip_prefix(source_root)
                    .ok()
                    .and_then(path_to_slash)
                    .filter(|path| !path.is_empty())
                    .unwrap_or_else(|| ".".into()),
            ),
        },
    );
}

fn push_candidate(evidence: &mut Evidence, origin: OriginHint) {
    if evidence
        .problems
        .iter()
        .any(|problem| problem == CANDIDATE_LIMIT_PROBLEM)
        || evidence.candidates.contains(&origin)
    {
        return;
    }
    if evidence.candidates.len() == MAX_EVIDENCE_CANDIDATES {
        evidence.candidates.clear();
        evidence.problems.push(CANDIDATE_LIMIT_PROBLEM.into());
        return;
    }
    evidence.candidates.push(origin);
}

#[derive(Deserialize)]
struct LockFile {
    version: u32,
    #[serde(deserialize_with = "unique_lock_skills")]
    skills: std::collections::BTreeMap<String, LockEntry>,
}

fn unique_lock_skills<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<std::collections::BTreeMap<String, LockEntry>, D::Error> {
    struct UniqueSkills;
    impl<'de> serde::de::Visitor<'de> for UniqueSkills {
        type Value = std::collections::BTreeMap<String, LockEntry>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a skills object with unique names")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(
            self,
            mut map: M,
        ) -> Result<Self::Value, M::Error> {
            let mut skills = Self::Value::new();
            while let Some((name, entry)) = map.next_entry::<String, LockEntry>()? {
                if skills.insert(name, entry).is_some() {
                    return Err(serde::de::Error::custom(
                        "duplicate skill name in lock evidence",
                    ));
                }
            }
            Ok(skills)
        }
    }
    deserializer.deserialize_map(UniqueSkills)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LockEntry {
    source: String,
    source_type: Option<String>,
    skill_path: String,
}

fn read_optional_text(
    path: &Path,
    evidence: &mut Evidence,
    label: &str,
) -> Result<Option<String>, ()> {
    match read_text_without_following(path) {
        Ok(text) => {
            evidence.fingerprints.push((
                path.to_path_buf(),
                Some(format!("{:x}", Sha256::digest(text.as_bytes()))),
            ));
            Ok(Some(text))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            evidence.fingerprints.push((path.to_path_buf(), None));
            Ok(None)
        }
        Err(error) => {
            evidence.unavailable_read |= error.kind() != io::ErrorKind::InvalidData;
            evidence.invalid_read |= error.kind() == io::ErrorKind::InvalidData;
            evidence
                .problems
                .push(format!("{label} is unreadable: {error}"));
            Err(())
        }
    }
}

fn read_text_without_following(path: &Path) -> io::Result<String> {
    let before = fs::symlink_metadata(path)?;
    if before.file_type().is_symlink() || !before.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "evidence is not a regular file",
        ));
    }
    if before.len() > MAX_EVIDENCE_BYTES as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "evidence exceeds the byte limit",
        ));
    }
    let mut file = open_file_without_following(path).map_err(|error| {
        let kind = match &error {
            ObservationFailure::Changed(_) => io::ErrorKind::InvalidData,
            ObservationFailure::Unavailable(_) => io::ErrorKind::Other,
        };
        io::Error::new(kind, error)
    })?;
    let mut bytes = Vec::with_capacity(before.len() as usize);
    file.by_ref()
        .take((MAX_EVIDENCE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_EVIDENCE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "evidence exceeds the byte limit",
        ));
    }
    let after = fs::symlink_metadata(path)?;
    if before.file_type() != after.file_type()
        || before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "evidence changed while reading",
        ));
    }
    String::from_utf8(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn is_table_row(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with('|') && trimmed.ends_with('|')
}

fn markdown_code(value: &str) -> Option<&str> {
    value.strip_prefix('`')?.strip_suffix('`')
}

fn parse_origin_url(text: &str) -> Option<OriginHint> {
    let start = text.find("https://github.com/")?;
    let url = &text[start..]
        .split_whitespace()
        .next()?
        .trim_end_matches([')', ']', '`', ',', ';']);
    let parts: Vec<_> = url
        .strip_prefix("https://github.com/")?
        .split('/')
        .collect();
    if parts.len() < 2 {
        return None;
    }
    let repository = format!("{}/{}", parts[0], parts[1]);
    if !valid_repository_name(&repository) {
        return None;
    }
    if parts.len() == 2 {
        return Some(OriginHint {
            repository: format!("https://github.com/{repository}"),
            subdirectory: None,
        });
    }
    if parts.len() < 4 || parts[2] != "tree" || !valid_pinned_commit(parts[3]) {
        return None;
    }
    let subdirectory = if parts.len() == 4 {
        ".".into()
    } else {
        parts[4..].join("/")
    };
    if !valid_subdirectory(&subdirectory) {
        return None;
    }
    Some(OriginHint {
        repository: format!("https://github.com/{repository}"),
        subdirectory: Some(subdirectory),
    })
}

fn lock_skill_directory(source_root: &Path, skill_path: &str) -> Option<PathBuf> {
    let path = Path::new(skill_path);
    if path.is_absolute() || path.file_name()? != OsStr::new("SKILL.md") {
        return None;
    }
    let parent = path.parent()?;
    if !valid_relative_path(parent) {
        return None;
    }
    Some(source_root.join(parent))
}

fn valid_repository(value: &str) -> bool {
    let Some(value) = value.strip_prefix("https://github.com/") else {
        return false;
    };
    valid_repository_name(value)
}

fn valid_repository_name(value: &str) -> bool {
    let Some((owner, name)) = value.split_once('/') else {
        return false;
    };
    !owner.is_empty()
        && !name.is_empty()
        && !name.contains('/')
        && owner
            .bytes()
            .chain(name.bytes())
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn valid_subdirectory(value: &str) -> bool {
    value == "."
        || (!value.is_empty()
            && !value.contains('\\')
            && value
                .bytes()
                .all(|byte| byte.is_ascii_graphic() || byte == b'/')
            && value
                .split('/')
                .all(|part| !part.is_empty() && part != "." && part != "..")
            && valid_relative_path(Path::new(value)))
}

fn valid_relative_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}

fn valid_skill_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn valid_pinned_commit(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_git_path(value: &str) -> bool {
    !value.is_empty()
        && !value.ends_with('/')
        && !value.contains("//")
        && !value.contains("..")
        && !value.contains("@{")
        && value.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.starts_with('.')
                && !part.ends_with('.')
                && !part.ends_with(".lock")
                && part.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/')
                })
        })
}

fn path_to_slash(path: &Path) -> Option<String> {
    let mut output = String::new();
    for component in path.components() {
        let std::path::Component::Normal(component) = component else {
            return None;
        };
        if !output.is_empty() {
            output.push('/');
        }
        output.push_str(component.to_str()?);
    }
    Some(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::Path};
    use tempfile::tempdir;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn root_tables_own_their_headers_and_do_not_leak_into_unrelated_tables() {
        let temp = tempdir().unwrap();
        let skill = temp.path().join("skills/example");
        write(
            &skill.join("SKILL.md"),
            "---\nname: example\ndescription: fixture\n---\n",
        );
        write(
            &temp.path().join("ATTRIBUTION.md"),
            "| Skill | Source | License |\n|---|---|---|\n| `other` | https://github.com/acme/other | MIT |\n\n| Item | Link |\n|---|---|\n| `example` | https://github.com/unrelated/link |\n\n| Source | License | Skill |\n|---|---|---|\n| https://github.com/acme/correct | MIT | `example` |\n",
        );
        let evidence = read_evidence(temp.path(), &skill, "example");
        assert_eq!(evidence.candidates.len(), 1);
        assert_eq!(
            evidence.candidates[0].repository,
            "https://github.com/acme/correct"
        );
        assert!(evidence.problems.is_empty());
    }

    #[test]
    fn a_repository_name_with_three_hyphens_is_not_a_table_separator() {
        let temp = tempdir().unwrap();
        let skill = temp.path().join("skills/example");
        write(
            &skill.join("SKILL.md"),
            "---\nname: example\ndescription: fixture\n---\n",
        );
        write(
            &temp.path().join("ATTRIBUTION.md"),
            "| Skill | Source | License |\n|---|---|---|\n| `example` | https://github.com/acme/example---skills | MIT |\n",
        );
        let evidence = read_evidence(temp.path(), &skill, "example");
        assert_eq!(evidence.candidates.len(), 1);
        assert_eq!(
            evidence.candidates[0].repository,
            "https://github.com/acme/example---skills"
        );
    }

    #[test]
    fn duplicate_lock_names_are_not_resolved_by_json_order() {
        let text = r#"{"version":1,"skills":{"example":{"source":"owner/one","sourceType":"github","skillPath":"skills/example/SKILL.md"},"example":{"source":"owner/two","sourceType":"github","skillPath":"skills/example/SKILL.md"}}}"#;
        assert!(serde_json::from_str::<LockFile>(text).is_err());
    }

    #[test]
    fn root_table_and_pinned_skill_attribution_are_parsed() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        let skill = root.join("skills/slice-issues");
        write(
            &root.join("ATTRIBUTION.md"),
            "| Skill | Source | License |\n|---|---|---|\n| `slice-issues` | https://github.com/mattpocock/skills/tree/2ab958093e83e0ec752e6c1c5932da465bf23e0c/skills/engineering/to-tickets | MIT |\n",
        );
        write(
            &skill.join("ATTRIBUTION.md"),
            "- Upstream: https://github.com/mattpocock/skills\n- Source snapshot: https://github.com/mattpocock/skills/tree/2ab958093e83e0ec752e6c1c5932da465bf23e0c/skills/engineering/to-tickets\n",
        );
        let evidence = read_evidence(root, &skill, "slice-issues");
        assert_eq!(
            evidence.candidates,
            vec![OriginHint {
                repository: "https://github.com/mattpocock/skills".into(),
                subdirectory: Some("skills/engineering/to-tickets".into())
            }]
        );
        assert!(evidence.problems.is_empty());
    }

    #[test]
    fn evidence_equality_detects_a_pinned_attribution_revision_change() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        let skill = root.join("skills/example");
        let attribution = skill.join("ATTRIBUTION.md");
        write(
            &attribution,
            "- Source snapshot: https://github.com/owner/repo/tree/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/skills/example\n",
        );
        let before = read_evidence(root, &skill, "example");
        write(
            &attribution,
            "- Source snapshot: https://github.com/owner/repo/tree/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb/skills/example\n",
        );
        let after = read_evidence(root, &skill, "example");
        assert_eq!(before.candidates, after.candidates);
        assert_ne!(before, after);
    }

    #[test]
    fn malformed_recognized_attribution_blocks_adoption() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        let skill = root.join("skills/example");
        write(
            &root.join("ATTRIBUTION.md"),
            "| Skill | Source |\n|---|---|\n| `example` | https://example.invalid/not-github |\n",
        );
        write(
            &skill.join("ATTRIBUTION.md"),
            "- License: https://example.invalid/license\n- Upstream: not a URL\n",
        );
        let evidence = read_evidence(root, &skill, "example");
        assert!(evidence.candidates.is_empty());
        assert!(
            evidence
                .problems
                .iter()
                .any(|problem| problem.contains("root attribution"))
        );
        assert!(
            evidence
                .problems
                .iter()
                .any(|problem| problem.contains("skill attribution"))
        );
    }

    #[test]
    fn root_table_does_not_leak_to_an_agent_specific_same_name_skill() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        let common = root.join("skills/example");
        let agent_specific = root.join(".claude/skills/example");
        write(
            &root.join("ATTRIBUTION.md"),
            "| Skill | Source |\n|---|---|\n| `example` | https://github.com/owner/common |\n",
        );
        write(
            &common.join("SKILL.md"),
            "---\nname: example\ndescription: common\n---\n",
        );
        write(
            &agent_specific.join("SKILL.md"),
            "---\nname: example\ndescription: agent\n---\n",
        );
        assert_eq!(
            read_evidence(root, &common, "example").candidates,
            vec![OriginHint {
                repository: "https://github.com/owner/common".into(),
                subdirectory: None,
            }]
        );
        assert!(
            read_evidence(root, &agent_specific, "example")
                .candidates
                .is_empty()
        );
    }

    #[test]
    fn root_table_refuses_two_common_skill_shapes_with_the_same_name() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        let nested = root.join("skills/example");
        let direct = root.join("example");
        write(
            &root.join("ATTRIBUTION.md"),
            "| Skill | Source |\n|---|---|\n| `example` | https://github.com/owner/common |\n",
        );
        write(&nested.join("SKILL.md"), "skill");
        write(&direct.join("SKILL.md"), "skill");
        let evidence = read_evidence(root, &nested, "example");
        assert!(evidence.candidates.is_empty());
        assert!(
            evidence
                .problems
                .iter()
                .any(|problem| problem.contains("cannot distinguish"))
        );
    }

    #[test]
    fn matching_v1_lock_is_a_hint_not_a_revision() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        let skill = root.join("catalogs/first-party/codex/skills/slice-issues");
        write(
            &root.join("skills-lock.json"),
            r#"{"version":1,"skills":{"slice-issues":{"source":"brian-bell/agent-skills","ref":"main","sourceType":"github","skillPath":"catalogs/first-party/codex/skills/slice-issues/SKILL.md"}}}"#,
        );
        let evidence = read_evidence(root, &skill, "slice-issues");
        assert_eq!(
            evidence.candidates,
            vec![OriginHint {
                repository: "https://github.com/brian-bell/agent-skills".into(),
                subdirectory: Some("catalogs/first-party/codex/skills/slice-issues".into())
            }]
        );
    }

    #[test]
    fn lock_path_mismatch_does_not_supply_origin() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        let skill = root.join("skills/slice-issues");
        write(
            &root.join("skills-lock.json"),
            r#"{"version":1,"skills":{"slice-issues":{"source":"brian-bell/agent-skills","sourceType":"github","skillPath":"skills/other/SKILL.md"}}}"#,
        );
        let evidence = read_evidence(root, &skill, "slice-issues");
        assert!(evidence.candidates.is_empty());
        assert!(
            evidence
                .problems
                .iter()
                .any(|problem| problem.contains("exact skill path"))
        );
    }

    #[test]
    fn evidence_refuses_more_than_128_distinct_candidates() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        let skill = root.join("skills/example");
        let mut table = String::from("| Skill | Source |\n|---|---|\n");
        for number in 0..=MAX_EVIDENCE_CANDIDATES {
            table.push_str(&format!(
                "| `example` | https://github.com/owner-{number}/repository |\n"
            ));
        }
        write(&root.join("ATTRIBUTION.md"), &table);
        let evidence = read_evidence(root, &skill, "example");
        assert!(evidence.candidates.is_empty());
        assert_eq!(evidence.problems, vec![CANDIDATE_LIMIT_PROBLEM]);
    }

    #[test]
    fn resolution_keeps_unknown_paths_separate_from_known_roots_and_conflicts() {
        let repository = "https://github.com/owner/repo";
        let origin = |path: &str| Origin::from_input(repository, path).unwrap();
        let mut evidence = Evidence::default();
        assert!(evidence.resolve(origin("arbitrary/path")).is_ok());
        evidence.candidates.push(OriginHint {
            repository: repository.into(),
            subdirectory: None,
        });
        assert_eq!(
            evidence.suggested_fields(),
            [repository.into(), String::new(), String::new()]
        );
        assert!(Origin::from_input(repository, " ").is_err());
        assert!(evidence.resolve(origin(".")).is_ok());
        evidence.candidates.push(OriginHint {
            repository: repository.into(),
            subdirectory: Some(".".into()),
        });
        assert!(evidence.is_ambiguous());
        assert_eq!(
            evidence.suggested_fields(),
            [String::new(), String::new(), String::new()]
        );
        assert!(evidence.resolve(origin("arbitrary/path")).is_err());
        assert!(evidence.resolve(origin(".")).is_ok());
        evidence.candidates.push(OriginHint {
            repository: repository.into(),
            subdirectory: Some("known/path".into()),
        });
        assert!(evidence.resolve(origin("known/path")).is_ok());
        assert!(
            evidence
                .resolve(Origin::from_input("https://github.com/other/repo", ".").unwrap())
                .is_err()
        );
        evidence.problems.push("unreadable evidence".into());
        assert!(
            evidence
                .resolve(origin("known/path"))
                .unwrap_err()
                .contains("incomplete")
        );
    }

    #[test]
    fn form_normalization_does_not_repair_persisted_origins() {
        let normalized = Origin::from_input(" https://github.com/owner/repo ", " . ").unwrap();
        assert_eq!(normalized.repository(), "https://github.com/owner/repo");
        assert_eq!(normalized.subdirectory(), ".");
        assert!(Origin::new(" https://github.com/owner/repo ".into(), ".".into()).is_err());
        assert!(Origin::new("https://github.com/owner/repo".into(), " . ".into()).is_err());
    }

    #[test]
    fn absent_unreadable_and_malformed_evidence_stay_distinct() {
        let temp = tempdir().unwrap();
        let skill = temp.path().join("skills/example");
        fs::create_dir_all(&skill).unwrap();
        let absent = read_evidence(temp.path(), &skill, "example");
        assert!(absent.problems.is_empty());
        fs::create_dir(skill.join("ATTRIBUTION.md")).unwrap();
        let unreadable = read_evidence(temp.path(), &skill, "example");
        assert!(unreadable.problems[0].contains("unreadable"));
        fs::remove_dir(skill.join("ATTRIBUTION.md")).unwrap();
        write(&skill.join("ATTRIBUTION.md"), "Upstream: unsupported");
        let malformed = read_evidence(temp.path(), &skill, "example");
        assert!(malformed.problems[0].contains("malformed or unsupported"));
        assert_ne!(absent, unreadable);
        assert_ne!(unreadable, malformed);
    }

    #[test]
    fn origin_validation_requires_a_safe_tracking_branch() {
        assert!(validate_update_ref("refs/heads/main").is_ok());
        assert!(validate_update_ref("main").is_err());
        assert!(validate_update_ref("refs/heads/../main").is_err());
        assert!(validate_update_ref("refs/heads/release..next").is_err());
        assert!(validate_update_ref("refs/heads/.private").is_err());
        assert!(Origin::new("https://github.com/owner/repo".into(), "../example".into()).is_err());
    }
}
