use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
};

use serde::Deserialize;
use serde_yaml_ng::Value;
use thiserror::Error;

const MAX_SKILL_MD_BYTES: usize = 1024 * 1024;
const MAX_SKILL_DIRECTORY_ENTRIES: usize = 1_024;
const MAX_SOURCE_SCAN_ENTRIES: usize = 16_384;
const MAX_SOURCE_SCAN_BYTES: usize = 32 * 1024 * 1024;
const MAX_INSTALLATION_SCAN_ENTRIES: usize = 262_144;
const MAX_INSTALLATION_SCAN_BYTES: usize = 32 * 1024 * 1024;

pub(crate) struct InspectionBudget {
    remaining_entries: usize,
    remaining_bytes: usize,
}

impl InspectionBudget {
    pub(crate) fn source_scan() -> Self {
        Self {
            remaining_entries: MAX_SOURCE_SCAN_ENTRIES,
            remaining_bytes: MAX_SOURCE_SCAN_BYTES,
        }
    }

    /// A pass over the agent installation roots.
    ///
    /// Three roots of bounded width, each entry validated in place, so the
    /// aggregate allowance is wider than a single source scan's.
    pub(crate) fn installation_scan() -> Self {
        Self {
            remaining_entries: MAX_INSTALLATION_SCAN_ENTRIES,
            remaining_bytes: MAX_INSTALLATION_SCAN_BYTES,
        }
    }

    /// A budget with nothing left, for exercising the fail-closed paths.
    #[cfg(test)]
    pub(crate) fn exhausted() -> Self {
        Self {
            remaining_entries: 0,
            remaining_bytes: 0,
        }
    }

    pub(crate) fn consume_entry(&mut self) -> bool {
        if self.remaining_entries == 0 {
            return false;
        }
        self.remaining_entries -= 1;
        true
    }

    pub(crate) fn consume_bytes(&mut self, bytes: usize) -> bool {
        let Some(remaining) = self.remaining_bytes.checked_sub(bytes) else {
            return false;
        };
        self.remaining_bytes = remaining;
        true
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ValidatedSkill {
    name: String,
    description: String,
    body: String,
    unknown_fields: BTreeMap<String, Value>,
}

impl ValidatedSkill {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn body(&self) -> &str {
        &self.body
    }

    pub fn unknown_fields(&self) -> &BTreeMap<String, Value> {
        &self.unknown_fields
    }
}

#[derive(Debug, Error)]
pub enum PortableValidationError {
    #[error("skill directory cannot be read: {path:?}: {source}")]
    ReadDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("skill does not contain a readable file named exactly SKILL.md")]
    MissingSkillMd,
    #[error("SKILL.md cannot be read as UTF-8 text: {0}")]
    UnreadableSkillMd(#[source] std::io::Error),
    #[error("SKILL.md exceeds the {max_bytes}-byte inspection limit")]
    SkillMdTooLarge { max_bytes: usize },
    #[error("skill directory exceeds the {max_entries}-entry inspection limit")]
    SkillDirectoryTooLarge { max_entries: usize },
    #[error("source inspection exceeded its aggregate entry or byte limit")]
    SourceInspectionLimitExceeded,
    #[error("SKILL.md must begin with YAML frontmatter")]
    MissingFrontmatter,
    #[error("SKILL.md YAML frontmatter is not terminated")]
    UnterminatedFrontmatter,
    #[error("SKILL.md YAML frontmatter is invalid: {0}")]
    InvalidFrontmatter(#[from] serde_yaml_ng::Error),
    #[error(
        "skill name must be 1-64 lowercase ASCII letters or digits with single hyphen separators"
    )]
    InvalidName,
    #[error("skill name {name:?} does not match directory name {directory:?}")]
    NameMismatch { name: String, directory: String },
    #[error("skill description must be 1-1024 characters")]
    InvalidDescription,
}

#[derive(Deserialize)]
struct Frontmatter {
    name: Option<String>,
    description: Option<String>,
    #[serde(flatten)]
    unknown_fields: BTreeMap<String, Value>,
}

pub fn validate_portable_skill(
    skill_directory: &Path,
) -> Result<ValidatedSkill, PortableValidationError> {
    let mut budget = InspectionBudget::source_scan();
    validate_portable_skill_with_budget(skill_directory, &mut budget)
}

pub(crate) fn validate_portable_skill_with_budget(
    skill_directory: &Path,
    budget: &mut InspectionBudget,
) -> Result<ValidatedSkill, PortableValidationError> {
    let skill_md = exact_skill_md(skill_directory, budget)?;
    let canonical_directory = skill_directory
        .canonicalize()
        .map_err(PortableValidationError::UnreadableSkillMd)?;
    let canonical_skill_md = skill_md
        .canonicalize()
        .map_err(PortableValidationError::UnreadableSkillMd)?;
    if !canonical_skill_md.starts_with(&canonical_directory) {
        return Err(PortableValidationError::MissingSkillMd);
    }
    let content = read_bounded_skill_md(&canonical_skill_md, budget)?;
    let (frontmatter, body) = split_frontmatter(&content)?;
    let frontmatter: Frontmatter = serde_yaml_ng::from_str(frontmatter)?;
    let name = frontmatter
        .name
        .ok_or(PortableValidationError::InvalidName)?;
    if !valid_skill_name(&name) {
        return Err(PortableValidationError::InvalidName);
    }
    let directory_name = skill_directory
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or(PortableValidationError::InvalidName)?;
    if name != directory_name {
        return Err(PortableValidationError::NameMismatch {
            name,
            directory: directory_name.to_owned(),
        });
    }
    let description = frontmatter
        .description
        .ok_or(PortableValidationError::InvalidDescription)?;
    if !(1..=1024).contains(&description.chars().count()) {
        return Err(PortableValidationError::InvalidDescription);
    }
    Ok(ValidatedSkill {
        name: directory_name.to_owned(),
        description,
        body: body.to_owned(),
        unknown_fields: frontmatter.unknown_fields,
    })
}

pub(crate) fn valid_skill_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name.split('-').all(|segment| {
            !segment.is_empty()
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
}

fn exact_skill_md(
    skill_directory: &Path,
    budget: &mut InspectionBudget,
) -> Result<PathBuf, PortableValidationError> {
    let entries =
        fs::read_dir(skill_directory).map_err(|source| PortableValidationError::ReadDirectory {
            path: skill_directory.to_path_buf(),
            source,
        })?;
    let mut skill_md = None;
    for (index, entry) in entries.enumerate() {
        if index == MAX_SKILL_DIRECTORY_ENTRIES {
            return Err(PortableValidationError::SkillDirectoryTooLarge {
                max_entries: MAX_SKILL_DIRECTORY_ENTRIES,
            });
        }
        if !budget.consume_entry() {
            return Err(PortableValidationError::SourceInspectionLimitExceeded);
        }
        let entry = entry.map_err(|source| PortableValidationError::ReadDirectory {
            path: skill_directory.to_path_buf(),
            source,
        })?;
        if entry.file_name() == OsStr::new("SKILL.md")
            && entry
                .file_type()
                .is_ok_and(|file_type| file_type.is_file() && !file_type.is_symlink())
        {
            skill_md = Some(entry.path());
        }
    }
    skill_md.ok_or(PortableValidationError::MissingSkillMd)
}

fn read_bounded_skill_md(
    path: &Path,
    budget: &mut InspectionBudget,
) -> Result<String, PortableValidationError> {
    let file = File::open(path).map_err(PortableValidationError::UnreadableSkillMd)?;
    let mut bytes = Vec::new();
    file.take((MAX_SKILL_MD_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(PortableValidationError::UnreadableSkillMd)?;
    if !budget.consume_bytes(bytes.len()) {
        return Err(PortableValidationError::SourceInspectionLimitExceeded);
    }
    if bytes.len() > MAX_SKILL_MD_BYTES {
        return Err(PortableValidationError::SkillMdTooLarge {
            max_bytes: MAX_SKILL_MD_BYTES,
        });
    }
    String::from_utf8(bytes).map_err(|error| {
        PortableValidationError::UnreadableSkillMd(io::Error::new(
            io::ErrorKind::InvalidData,
            error,
        ))
    })
}

fn split_frontmatter(content: &str) -> Result<(&str, &str), PortableValidationError> {
    let remainder = content
        .strip_prefix("---\n")
        .or_else(|| content.strip_prefix("---\r\n"))
        .ok_or(PortableValidationError::MissingFrontmatter)?;
    if let Some((frontmatter, body)) = remainder
        .split_once("\n---\n")
        .or_else(|| remainder.split_once("\r\n---\r\n"))
    {
        return Ok((frontmatter, body));
    }
    remainder
        .strip_suffix("\n---")
        .or_else(|| remainder.strip_suffix("\r\n---"))
        .map(|frontmatter| (frontmatter, ""))
        .ok_or(PortableValidationError::UnterminatedFrontmatter)
}
