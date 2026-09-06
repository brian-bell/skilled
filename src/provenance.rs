//! Bounded, local provenance evidence and content baselines.
//!
//! This module never contacts an origin or asks Git whether a worktree is
//! clean. Its digest deliberately covers the bytes presently in a skill
//! directory: ignored and untracked entries are part of it. The only excluded
//! entries are `.git` directories (or gitfiles) at any level, which are Git
//! metadata rather than skill content. Unix opens use no-follow, nonblocking
//! descriptors; other platforms retain the conservative before-and-after
//! metadata checks but do not claim descriptor-pinned traversal.

use std::{
    collections::BTreeSet,
    ffi::OsStr,
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
};

use serde::Deserialize;
use sha2::{Digest, Sha256};

const BASELINE_VERSION: u32 = 1;
const MAX_ENTRIES: usize = 16_384;
const MAX_BYTES: usize = 32 * 1024 * 1024;
const MAX_DEPTH: usize = 32;
const MAX_PATH_BYTES: usize = 16 * 1024;
const MAX_SYMLINK_BYTES: usize = 16 * 1024;
const MAX_EVIDENCE_BYTES: usize = 1024 * 1024;
const MAX_EVIDENCE_CANDIDATES: usize = 128;
const CANDIDATE_LIMIT_PROBLEM: &str = "provenance evidence exceeds 128 distinct origin candidates";

/// An origin identified by a supported attribution format.
///
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct Origin {
    pub repository: String,
    pub subdirectory: String,
}

/// Evidence found without accessing the network.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Evidence {
    pub candidates: Vec<Origin>,
    pub problems: Vec<String>,
    fingerprints: Vec<(PathBuf, Option<String>)>,
}

/// A versioned whole-directory content baseline.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Baseline {
    pub version: u32,
    pub digest: String,
}

/// Validates the GitHub URL and portable subdirectory the user confirms.
pub fn validate_origin(origin: &Origin) -> Result<(), String> {
    if !valid_repository(&origin.repository) {
        return Err("origin repository must be an https://github.com/owner/name URL".into());
    }
    if !valid_subdirectory(&origin.subdirectory) {
        return Err("origin subdirectory is not a safe relative path".into());
    }
    Ok(())
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

/// Computes a deterministic SHA-256 digest of all content in a directory.
/// It is fail-closed for resource exhaustion, unsupported entry types, links,
/// and observations that change while being read.
pub fn directory_hash(path: &Path) -> Result<Baseline, String> {
    let root = fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect skill directory {}: {error}", path.display()))?;
    if !root.file_type().is_dir() {
        return Err(format!(
            "skill baseline root is not a directory: {}",
            path.display()
        ));
    }
    let mut state = HashState::new();
    state.entry(Path::new(""), b'D', executable(&root), &[])?;
    hash_directory(path, Path::new(""), 0, &mut state)?;
    ensure_unchanged(path, &root)?;
    Ok(Baseline {
        version: BASELINE_VERSION,
        digest: format!("{:x}", state.hasher.finalize()),
    })
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
        .filter(|origin| !origin.subdirectory.is_empty())
        .map(|origin| origin.repository.clone())
        .collect();
    for origin in origins {
        if !origin.subdirectory.is_empty() || !pinned_repositories.contains(&origin.repository) {
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
        Origin {
            repository: format!("https://github.com/{}", entry.source),
            subdirectory: skill_path
                .strip_prefix(source_root)
                .ok()
                .and_then(path_to_slash)
                .filter(|path| !path.is_empty())
                .unwrap_or_else(|| ".".into()),
        },
    );
}

fn push_candidate(evidence: &mut Evidence, origin: Origin) {
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
        return Err(io::Error::other("evidence is not a regular file"));
    }
    if before.len() > MAX_EVIDENCE_BYTES as u64 {
        return Err(io::Error::other("evidence exceeds the byte limit"));
    }
    let mut file = open_file_without_following(path).map_err(io::Error::other)?;
    let mut bytes = Vec::with_capacity(before.len() as usize);
    file.by_ref()
        .take((MAX_EVIDENCE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_EVIDENCE_BYTES {
        return Err(io::Error::other("evidence exceeds the byte limit"));
    }
    let after = fs::symlink_metadata(path)?;
    if before.file_type() != after.file_type()
        || before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
    {
        return Err(io::Error::other("evidence changed while reading"));
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

fn parse_origin_url(text: &str) -> Option<Origin> {
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
        return Some(Origin {
            repository: format!("https://github.com/{repository}"),
            subdirectory: String::new(),
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
    Some(Origin {
        repository: format!("https://github.com/{repository}"),
        subdirectory,
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

struct HashState {
    hasher: Sha256,
    entries: usize,
    bytes: usize,
}
impl HashState {
    fn new() -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"skilled-directory-baseline-v1\0");
        hasher.update(baseline_platform_tag());
        hasher.update(b"\0");
        Self {
            hasher,
            entries: 0,
            bytes: 0,
        }
    }
    fn entry(
        &mut self,
        path: &Path,
        kind: u8,
        executable: bool,
        bytes: &[u8],
    ) -> Result<(), String> {
        self.entries += 1;
        if self.entries > MAX_ENTRIES {
            return Err(format!("skill baseline exceeds {MAX_ENTRIES} entries"));
        }
        let path = path_bytes(path)?;
        if path.len() > MAX_PATH_BYTES {
            return Err("skill baseline path is too long".into());
        }
        self.bytes = self
            .bytes
            .checked_add(bytes.len())
            .ok_or("skill baseline size overflow")?;
        if self.bytes > MAX_BYTES {
            return Err(format!("skill baseline exceeds {MAX_BYTES} bytes"));
        }
        self.hasher.update([kind, u8::from(executable)]);
        self.hasher.update((path.len() as u64).to_be_bytes());
        self.hasher.update(&path);
        self.hasher.update((bytes.len() as u64).to_be_bytes());
        self.hasher.update(bytes);
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn baseline_platform_tag() -> &'static [u8] {
    b"linux"
}
#[cfg(target_os = "macos")]
fn baseline_platform_tag() -> &'static [u8] {
    b"macos"
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn baseline_platform_tag() -> &'static [u8] {
    b"other"
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn hash_directory(
    root: &Path,
    relative: &Path,
    depth: usize,
    state: &mut HashState,
) -> Result<(), String> {
    let path = root.join(relative);
    let before = fs::symlink_metadata(&path)
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
    let held = open_directory_without_following(&path)?;
    ensure_unchanged_metadata(
        &path,
        &before,
        &held
            .metadata()
            .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?,
    )?;
    hash_directory_bound(root, &held, relative, depth, state)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn hash_directory_bound(
    root: &Path,
    directory: &fs::File,
    relative: &Path,
    depth: usize,
    state: &mut HashState,
) -> Result<(), String> {
    let directory_before = stat_file(directory)?;
    if depth > MAX_DEPTH {
        return Err(format!(
            "skill baseline exceeds {MAX_DEPTH} directory levels"
        ));
    }
    let mut entries = crate::git::bound_directory_entries(directory, MAX_ENTRIES)
        .map_err(|error| format!("cannot read {}: {error}", root.join(relative).display()))?
        .ok_or_else(|| format!("skill baseline exceeds {MAX_ENTRIES} entries"))?
        .into_iter()
        .map(|entry| entry.name)
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| os_bytes(left).cmp(os_bytes(right)));
    for name in entries {
        if name == OsStr::new(".git") {
            continue;
        }
        let child_relative = relative.join(&name);
        let before = stat_at(directory, &name)?;
        match before.kind() {
            libc::S_IFDIR => {
                state.entry(&child_relative, b'D', before.executable(), &[])?;
                let child = crate::git::open_directory_at(directory, &name).map_err(|error| {
                    format!(
                        "cannot open {}: {error}",
                        root.join(&child_relative).display()
                    )
                })?;
                if !before.same(&stat_file(&child)?) {
                    return Err(format!(
                        "skill baseline changed while reading: {}",
                        root.join(&child_relative).display()
                    ));
                }
                hash_directory_bound(root, &child, &child_relative, depth + 1, state)?;
            }
            libc::S_IFREG => {
                hash_file_bound(root, directory, &name, &child_relative, &before, state)?
            }
            libc::S_IFLNK => {
                let target = read_link_at(directory, &name)?;
                if target.len() > MAX_SYMLINK_BYTES {
                    return Err("skill baseline symlink target is too long".into());
                }
                if !before.same(&stat_at(directory, &name)?) {
                    return Err(format!(
                        "skill baseline changed while reading: {}",
                        root.join(&child_relative).display()
                    ));
                }
                state.entry(&child_relative, b'L', before.executable(), &target)?;
            }
            _ => {
                return Err(format!(
                    "skill baseline contains unsupported entry: {}",
                    root.join(&child_relative).display()
                ));
            }
        }
    }
    if !directory_before.same(&stat_file(directory)?) {
        return Err(format!(
            "skill baseline directory changed while reading: {}",
            root.join(relative).display()
        ));
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn hash_file_bound(
    root: &Path,
    parent: &fs::File,
    name: &OsStr,
    relative: &Path,
    before: &UnixStat,
    state: &mut HashState,
) -> Result<(), String> {
    let mut file = open_file_at(parent, name)?;
    if !before.same(&stat_file(&file)?) {
        return Err(format!(
            "skill baseline changed while reading: {}",
            root.join(relative).display()
        ));
    }
    let mut content = Vec::new();
    file.by_ref()
        .take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut content)
        .map_err(|error| format!("cannot read {}: {error}", root.join(relative).display()))?;
    if !before.same(&stat_file(&file)?) {
        return Err(format!(
            "skill baseline changed while reading: {}",
            root.join(relative).display()
        ));
    }
    state.entry(relative, b'F', before.executable(), &content)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Clone, Copy)]
struct UnixStat(libc::stat);

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl UnixStat {
    fn kind(self) -> libc::mode_t {
        self.0.st_mode & libc::S_IFMT
    }
    fn executable(self) -> bool {
        self.0.st_mode & 0o111 != 0
    }
    fn same(self, other: &Self) -> bool {
        self.0.st_dev == other.0.st_dev
            && self.0.st_ino == other.0.st_ino
            && self.0.st_mode == other.0.st_mode
            && self.0.st_size == other.0.st_size
            && stat_times_equal(&self.0, &other.0)
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn stat_times_equal(left: &libc::stat, right: &libc::stat) -> bool {
    left.st_mtime == right.st_mtime
        && left.st_mtime_nsec == right.st_mtime_nsec
        && left.st_ctime == right.st_ctime
        && left.st_ctime_nsec == right.st_ctime_nsec
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn stat_at(parent: &fs::File, name: &OsStr) -> Result<UnixStat, String> {
    use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};
    let name = std::ffi::CString::new(name.as_bytes())
        .map_err(|_| "skill baseline path contains a NUL byte")?;
    let mut stat = unsafe { std::mem::zeroed::<libc::stat>() };
    if unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            &mut stat,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(format!(
            "cannot inspect entry: {}",
            io::Error::last_os_error()
        ));
    }
    Ok(UnixStat(stat))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn stat_file(file: &fs::File) -> Result<UnixStat, String> {
    use std::os::fd::AsRawFd;
    let mut stat = unsafe { std::mem::zeroed::<libc::stat>() };
    if unsafe { libc::fstat(file.as_raw_fd(), &mut stat) } != 0 {
        return Err(format!(
            "cannot inspect held entry: {}",
            io::Error::last_os_error()
        ));
    }
    Ok(UnixStat(stat))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn open_file_at(parent: &fs::File, name: &OsStr) -> Result<fs::File, String> {
    use std::os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    };
    let name = std::ffi::CString::new(name.as_bytes())
        .map_err(|_| "skill baseline path contains a NUL byte")?;
    let descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if descriptor < 0 {
        return Err(format!(
            "cannot open entry without following links: {}",
            io::Error::last_os_error()
        ));
    }
    Ok(unsafe { fs::File::from_raw_fd(descriptor) })
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn read_link_at(parent: &fs::File, name: &OsStr) -> Result<Vec<u8>, String> {
    use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};
    let name = std::ffi::CString::new(name.as_bytes())
        .map_err(|_| "skill baseline path contains a NUL byte")?;
    let mut target = vec![0; MAX_SYMLINK_BYTES + 1];
    let length = unsafe {
        libc::readlinkat(
            parent.as_raw_fd(),
            name.as_ptr(),
            target.as_mut_ptr().cast(),
            target.len(),
        )
    };
    if length < 0 {
        return Err(format!(
            "cannot read link target: {}",
            io::Error::last_os_error()
        ));
    }
    let length = length as usize;
    if length == target.len() {
        return Err("skill baseline symlink target is too long".into());
    }
    target.truncate(length);
    Ok(target)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn hash_directory(
    root: &Path,
    relative: &Path,
    depth: usize,
    state: &mut HashState,
) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err(format!(
            "skill baseline exceeds {MAX_DEPTH} directory levels"
        ));
    }
    let directory = root.join(relative);
    let before = fs::symlink_metadata(&directory)
        .map_err(|error| format!("cannot inspect {}: {error}", directory.display()))?;
    let held = open_directory_without_following(&directory)?;
    ensure_unchanged_metadata(
        &directory,
        &before,
        &held
            .metadata()
            .map_err(|error| format!("cannot inspect {}: {error}", directory.display()))?,
    )?;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let mut entries = crate::git::bound_directory_entries(&held, MAX_ENTRIES)
        .map_err(|error| format!("cannot read {}: {error}", directory.display()))?
        .ok_or_else(|| format!("skill baseline exceeds {MAX_ENTRIES} entries"))?
        .into_iter()
        .map(|entry| entry.name)
        .collect::<Vec<_>>();
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let mut entries = {
        let mut entries = Vec::new();
        for entry in fs::read_dir(&directory)
            .map_err(|error| format!("cannot read {}: {error}", directory.display()))?
        {
            if entries.len() == MAX_ENTRIES {
                return Err(format!("skill baseline exceeds {MAX_ENTRIES} entries"));
            }
            entries.push(
                entry
                    .map_err(|error| format!("cannot read {}: {error}", directory.display()))?
                    .file_name(),
            );
        }
        entries
    };
    entries.sort_by(|left, right| os_bytes(left).cmp(os_bytes(right)));
    for name in entries {
        if name == OsStr::new(".git") {
            continue;
        }
        let child_relative = relative.join(&name);
        let child = root.join(&child_relative);
        let metadata = fs::symlink_metadata(&child)
            .map_err(|error| format!("cannot inspect {}: {error}", child.display()))?;
        let file_type = metadata.file_type();
        if file_type.is_dir() {
            state.entry(&child_relative, b'D', executable(&metadata), &[])?;
            hash_directory(root, &child_relative, depth + 1, state)?;
        } else if file_type.is_file() {
            hash_file(&child, &child_relative, &metadata, state)?;
        } else if file_type.is_symlink() {
            let target = fs::read_link(&child)
                .map_err(|error| format!("cannot read link {}: {error}", child.display()))?;
            let target = path_bytes(&target)?;
            if target.len() > MAX_SYMLINK_BYTES {
                return Err("skill baseline symlink target is too long".into());
            }
            let after = fs::symlink_metadata(&child)
                .map_err(|error| format!("cannot inspect {}: {error}", child.display()))?;
            ensure_unchanged_metadata(&child, &metadata, &after)?;
            state.entry(&child_relative, b'L', executable(&metadata), &target)?;
        } else {
            return Err(format!(
                "skill baseline contains unsupported entry: {}",
                child.display()
            ));
        }
    }
    ensure_unchanged(&directory, &before)?;
    ensure_unchanged_metadata(
        &directory,
        &before,
        &held
            .metadata()
            .map_err(|error| format!("cannot inspect {}: {error}", directory.display()))?,
    )
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn hash_file(
    path: &Path,
    relative: &Path,
    before: &fs::Metadata,
    state: &mut HashState,
) -> Result<(), String> {
    let mut file = open_file_without_following(path)?;
    ensure_unchanged_metadata(
        path,
        before,
        &file
            .metadata()
            .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?,
    )?;
    let mut content = Vec::new();
    file.by_ref()
        .take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut content)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let after = fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
    ensure_unchanged_metadata(path, before, &after)?;
    state.entry(relative, b'F', executable(before), &content)
}

fn ensure_unchanged(path: &Path, before: &fs::Metadata) -> Result<(), String> {
    let after = fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
    ensure_unchanged_metadata(path, before, &after)
}

fn ensure_unchanged_metadata(
    path: &Path,
    before: &fs::Metadata,
    after: &fs::Metadata,
) -> Result<(), String> {
    if before.file_type() != after.file_type()
        || before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
        || executable(before) != executable(after)
        || !same_file_identity(before, after)
    {
        Err(format!(
            "skill baseline changed while reading: {}",
            path.display()
        ))
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}
#[cfg(unix)]
fn same_file_identity(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    before.dev() == after.dev()
        && before.ino() == after.ino()
        && before.ctime() == after.ctime()
        && before.ctime_nsec() == after.ctime_nsec()
}
#[cfg(not(unix))]
fn same_file_identity(_: &fs::Metadata, _: &fs::Metadata) -> bool {
    true
}
#[cfg(unix)]
fn open_directory_without_following(path: &Path) -> Result<fs::File, String> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| {
            format!(
                "cannot open directory {} without following links: {error}",
                path.display()
            )
        })
}
#[cfg(windows)]
fn open_directory_without_following(path: &Path) -> Result<fs::File, String> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    };
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|error| format!("cannot open directory {}: {error}", path.display()))
}

#[cfg(not(any(unix, windows)))]
fn open_directory_without_following(path: &Path) -> Result<fs::File, String> {
    fs::File::open(path)
        .map_err(|error| format!("cannot open directory {}: {error}", path.display()))
}
#[cfg(not(unix))]
fn executable(_: &fs::Metadata) -> bool {
    false
}
#[cfg(unix)]
fn open_file_without_following(path: &Path) -> Result<fs::File, String> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| {
            format!(
                "cannot read {} without following links: {error}",
                path.display()
            )
        })
}
#[cfg(windows)]
fn open_file_without_following(path: &Path) -> Result<fs::File, String> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|error| format!("cannot open file {}: {error}", path.display()))?;
    if !file
        .metadata()
        .map_err(|error| error.to_string())?
        .is_file()
    {
        return Err(format!("not a regular file: {}", path.display()));
    }
    Ok(file)
}

#[cfg(not(any(unix, windows)))]
fn open_file_without_following(path: &Path) -> Result<fs::File, String> {
    // The lstat checks before and after the read reject a replacement with a
    // link. Windows' standard library opens reparse points through this path;
    // this build has no descriptor-level no-follow primitive available here.
    fs::File::open(path).map_err(|error| format!("cannot read {}: {error}", path.display()))
}
fn os_bytes(value: &OsStr) -> &[u8] {
    value.as_encoded_bytes()
}
fn path_bytes(path: &Path) -> Result<Vec<u8>, String> {
    let bytes = os_bytes(path.as_os_str());
    if bytes.contains(&0) {
        Err("skill baseline path contains a NUL byte".into())
    } else {
        Ok(bytes.to_vec())
    }
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
            vec![Origin {
                repository: "https://github.com/mattpocock/skills".into(),
                subdirectory: "skills/engineering/to-tickets".into()
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
            vec![Origin {
                repository: "https://github.com/owner/common".into(),
                subdirectory: String::new(),
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
            vec![Origin {
                repository: "https://github.com/brian-bell/agent-skills".into(),
                subdirectory: "catalogs/first-party/codex/skills/slice-issues".into()
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
    fn directory_digest_is_stable_includes_links_and_excludes_git() {
        let temp = tempdir().unwrap();
        let skill = temp.path().join("skill");
        write(&skill.join("SKILL.md"), "one");
        write(&skill.join("nested/file"), "two");
        write(&skill.join(".git/config"), "ignored");
        #[cfg(unix)]
        std::os::unix::fs::symlink("nested/file", skill.join("alias")).unwrap();
        let first = directory_hash(&skill).unwrap();
        let second = directory_hash(&skill).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.version, 1);
        assert_eq!(first.digest.len(), 64);
        write(&skill.join(".git/config"), "still ignored");
        assert_eq!(first, directory_hash(&skill).unwrap());
        write(&skill.join("nested/file"), "changed");
        assert_ne!(first, directory_hash(&skill).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn digest_includes_modes_and_raw_link_targets_without_following_them() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let temp = tempdir().unwrap();
        let skill = temp.path().join("skill");
        write(&skill.join("SKILL.md"), "one");
        symlink("missing-one", skill.join("alias")).unwrap();
        let first = directory_hash(&skill).unwrap();
        symlink("missing-two", skill.join("replacement")).unwrap();
        fs::remove_file(skill.join("alias")).unwrap();
        fs::rename(skill.join("replacement"), skill.join("alias")).unwrap();
        assert_ne!(first, directory_hash(&skill).unwrap());
        let mut permissions = fs::metadata(skill.join("SKILL.md")).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(skill.join("SKILL.md"), permissions).unwrap();
        assert_ne!(first, directory_hash(&skill).unwrap());
    }

    #[test]
    fn digest_includes_ignored_content_and_refuses_excessive_depth() {
        let temp = tempdir().unwrap();
        let skill = temp.path().join("skill");
        write(&skill.join("SKILL.md"), "one");
        write(&skill.join("ignored-by-git.log"), "first");
        let first = directory_hash(&skill).unwrap();
        write(&skill.join("ignored-by-git.log"), "second");
        assert_ne!(first, directory_hash(&skill).unwrap());
        let mut nested = skill;
        for number in 0..=MAX_DEPTH {
            nested = nested.join(number.to_string());
        }
        write(&nested.join("too-deep"), "x");
        assert!(directory_hash(temp.path().join("skill").as_path()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn digest_refuses_fifo_without_blocking() {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};

        let temp = tempdir().unwrap();
        let skill = temp.path().join("skill");
        write(&skill.join("SKILL.md"), "one");
        let fifo = skill.join("stream");
        let fifo = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: the C string is NUL-terminated and names a fresh test path.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(directory_hash(&skill).is_err());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn bound_walker_keeps_reading_the_held_directory_after_its_path_is_replaced() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().unwrap();
        let skill = temp.path().join("skill");
        let moved = temp.path().join("moved");
        let impostor = temp.path().join("impostor");
        write(&skill.join("SKILL.md"), "held");
        write(&impostor.join("SKILL.md"), "impostor");
        let held = open_directory_without_following(&skill).unwrap();
        fs::rename(&skill, &moved).unwrap();
        symlink(&impostor, &skill).unwrap();

        let mut state = HashState::new();
        state
            .entry(
                Path::new(""),
                b'D',
                executable(&held.metadata().unwrap()),
                &[],
            )
            .unwrap();
        hash_directory_bound(temp.path(), &held, Path::new(""), 0, &mut state).unwrap();
        let held_digest = Baseline {
            version: BASELINE_VERSION,
            digest: format!("{:x}", state.hasher.finalize()),
        };
        assert_eq!(held_digest, directory_hash(&moved).unwrap());
        assert_ne!(held_digest, directory_hash(&impostor).unwrap());
    }

    #[test]
    fn origin_validation_requires_a_safe_tracking_branch() {
        assert!(validate_update_ref("refs/heads/main").is_ok());
        assert!(validate_update_ref("main").is_err());
        assert!(validate_update_ref("refs/heads/../main").is_err());
        assert!(validate_update_ref("refs/heads/release..next").is_err());
        assert!(validate_update_ref("refs/heads/.private").is_err());
        assert!(
            validate_origin(&Origin {
                repository: "https://github.com/owner/repo".into(),
                subdirectory: "../example".into()
            })
            .is_err()
        );
    }
}
