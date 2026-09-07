//! Bounded, local provenance evidence and content baselines.
//!
//! This module never contacts an origin or asks Git whether a worktree is
//! clean. Its digest deliberately covers the bytes presently in a skill
//! directory: ignored and untracked entries are part of it. The only excluded
//! entries are `.git` directories (or gitfiles) at any level, which are Git
//! metadata rather than skill content. Unix opens use no-follow, nonblocking
//! descriptors; other platforms retain the conservative before-and-after
//! metadata checks but do not claim descriptor-pinned traversal.

use sha2::{Digest, Sha256};
use std::{
    ffi::OsStr,
    fs,
    io::{self, Read},
    path::Path,
};

const BASELINE_VERSION: u32 = 1;
const MAX_ENTRIES: usize = 16_384;
const MAX_BYTES: usize = 32 * 1024 * 1024;
const MAX_DEPTH: usize = 32;
const MAX_PATH_BYTES: usize = 16 * 1024;
const MAX_SYMLINK_BYTES: usize = 16 * 1024;
/// A versioned whole-directory content baseline.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Baseline {
    pub version: u32,
    pub digest: String,
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
pub(super) fn open_file_without_following(path: &Path) -> Result<fs::File, String> {
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
pub(super) fn open_file_without_following(path: &Path) -> Result<fs::File, String> {
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
pub(super) fn open_file_without_following(path: &Path) -> Result<fs::File, String> {
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
    use tempfile::tempdir;
    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    // Frozen against PR 68 before extracting the walker. Includes empty and
    // nested directories, binary/untracked content, executable bits
    // and dangling links, and both forms of excluded Git metadata.
    #[cfg(unix)]
    #[test]
    fn baseline_v1_golden_fixture() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let temp = tempdir().unwrap();
        let root = temp.path().join("fixture");
        write(&root.join("SKILL.md"), "fixture\n");
        write(&root.join("nested/run"), "#!/bin/sh\n");
        fs::write(root.join("binary"), [0, 255, 10]).unwrap();
        fs::create_dir(root.join("empty")).unwrap();
        write(&root.join(".git/config"), "excluded");
        write(&root.join("nested/.git"), "gitdir: excluded");
        for path in [root.clone(), root.join("nested"), root.join("empty")] {
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        for path in [root.join("SKILL.md"), root.join("binary")] {
            fs::set_permissions(path, fs::Permissions::from_mode(0o644)).unwrap();
        }
        fs::set_permissions(root.join("nested/run"), fs::Permissions::from_mode(0o755)).unwrap();
        symlink("../missing", root.join("dangling")).unwrap();
        let baseline = directory_hash(&root).unwrap();
        assert_eq!(baseline.version, 1);
        #[cfg(target_os = "macos")]
        assert_eq!(
            baseline.digest,
            "3f80d8d03e40238c0b1e859c0333f4ddc111f6eea953993e380a84ecd124fcf8"
        );
        #[cfg(target_os = "linux")]
        assert_eq!(
            baseline.digest,
            "fbc1b78a5d5b389a154e54317aaf8a2b87f208dcb81923bb668406ce25b1b7db"
        );
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        assert_eq!(
            baseline.digest,
            "7bd20811314d1f494833302d88faacf46b1d7e7e2e18b2535daa54a69cab82f1"
        );
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
}
