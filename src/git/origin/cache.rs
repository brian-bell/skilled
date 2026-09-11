//! Single-flight, zero-idle-retention origin cache (Linux and macOS).
//!
//! The persistent manager and activity locks are never unlinked or explicitly
//! unlocked. Each Git child inherits the activity open-file description; an
//! independent exclusive lock must succeed before reclamation, including after
//! the worker dies. This protects cooperating Skilled/Git processes, not a
//! same-user program deliberately closing inherited descriptors or rewriting
//! the private manager. No timestamp or PID is used as proof of inactivity.
//!
//! A versioned receipt proves the root and repository directory identities.
//! Reclamation first renames the repository to a reserved quarantine name,
//! verifies that identity, and traverses held directories without following
//! links. Each emptied directory/file is moved without overwrite into a private
//! disposal slot and verified before unlinking. A replaced object is preserved
//! in that slot. An interrupted disposal is deliberately not auto-adopted.
//! This closes substitutions at the public names. Like the vendored exchange
//! boundary, it does not lock out a same-user writer inside private quarantine
//! between the final identity check and unlink. Mount/device changes, hard
//! links, special files and symbolic links are refused rather than reclaimed.

use super::RepositoryHandle;
use std::path::Path;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) struct Cache;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
impl Cache {
    pub(super) fn acquire(_path: &Path) -> Result<Self, String> {
        Err("origin cache reclamation requires Linux or macOS".into())
    }
    pub(super) fn repository(&self) -> Result<RepositoryHandle, String> {
        Err("origin cache reclamation is unsupported".into())
    }
    pub(super) fn finish(self) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) use supported::Cache;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod supported {
    use super::*;
    use crate::git::{bound_directory_entries, open_directory_at};
    use serde::{Deserialize, Serialize};
    use std::{
        ffi::{CString, OsStr},
        fs::{File, Metadata},
        io::{self, Read, Write},
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::{ffi::OsStrExt, fs::MetadataExt},
        },
        path::PathBuf,
    };

    const OWNER: &str = "owner.json";
    const GATE: &str = "manager.lock";
    const ACTIVITY: &str = "activity.lock";
    const RECEIPT: &str = "entry.json";
    const REPO: &str = "repository";
    const MARKER: &str = ".skilled-cache-generation";
    const RETIRED: &str = "retired";
    const DISPOSAL: &str = "disposal";
    const MAX_RECORD: u64 = 4096;
    const MAX_ENTRIES: usize = super::super::MAX_ENTRIES;
    const MAX_DEPTH: usize = super::super::MAX_DEPTH;

    #[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    struct Identity {
        device: u64,
        inode: u64,
    }

    impl Identity {
        fn of(metadata: &Metadata) -> Self {
            Self {
                device: metadata.dev(),
                inode: metadata.ino(),
            }
        }
        fn check(self, file: &File) -> io::Result<()> {
            if Self::of(&file.metadata()?) == self {
                Ok(())
            } else {
                Err(invalid("origin cache identity changed"))
            }
        }
    }

    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Owner {
        version: u8,
        root: Identity,
        gate: Identity,
        activity: Identity,
    }

    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Receipt {
        version: u8,
        root: Identity,
        repository: Identity,
        marker: Identity,
        generation: String,
    }

    pub(in super::super) struct Cache {
        parent: File,
        name: std::ffi::OsString,
        root: File,
        owner: Owner,
        gate: File,
        activity: File,
        path: PathBuf,
    }

    impl Cache {
        pub(in super::super) fn acquire(path: &Path) -> Result<Self, String> {
            Self::open(path).map_err(|error| format!(
                "origin cache unavailable at {}: {error}; retained entries are not deleted without proof",
                path.display()
            ))
        }

        fn open(path: &Path) -> io::Result<Self> {
            let parent_path = path
                .parent()
                .ok_or_else(|| invalid("cache has no data parent"))?
                .canonicalize()?;
            let name = path
                .file_name()
                .ok_or_else(|| invalid("cache has no plain name"))?;
            let parent_handle = RepositoryHandle::open(&parent_path).map_err(io::Error::other)?;
            let parent = parent_handle.directory;
            let created = match mkdir(&parent, name) {
                Ok(()) => true,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => false,
                Err(error) => return Err(error),
            };
            let root = open_directory_at(&parent, name)?;
            private(&root, true)?;
            if created {
                let gate = new_file(&root, GATE)?;
                let activity = new_file(&root, ACTIVITY)?;
                let owner = Owner {
                    version: 1,
                    root: Identity::of(&root.metadata()?),
                    gate: Identity::of(&gate.metadata()?),
                    activity: Identity::of(&activity.metadata()?),
                };
                write_record(&root, OWNER, &owner)?;
            }
            // An old origin-* directory, even an empty one, never establishes
            // authority. Interrupted manager initialization is also preserved.
            let owner: Owner = read_record(&root, OWNER).map_err(|error| invalid(format!(
                "unproven or legacy cache manager ({error}); inspect this directory before removing it manually"
            )))?;
            if owner.version != 1 {
                return Err(invalid("unsupported cache owner version"));
            }
            owner.root.check(&root)?;
            let gate = existing_file(&root, OsStr::new(GATE))?;
            owner.gate.check(&gate)?;
            lock(&gate, libc::LOCK_EX)?;
            let activity = existing_file(&root, OsStr::new(ACTIVITY))?;
            owner.activity.check(&activity)?;
            lock(&activity, libc::LOCK_EX)?;
            let cache = Self {
                parent,
                name: name.to_owned(),
                root,
                owner,
                gate,
                activity,
                path: parent_path.join(name),
            };
            cache.check_root()?;
            cache.reclaim()?;
            cache.allocate()?;
            Ok(cache)
        }

        fn check_root(&self) -> io::Result<()> {
            private(&self.root, true)?;
            self.owner
                .root
                .check(&open_directory_at(&self.parent, &self.name)?)?;
            self.owner
                .gate
                .check(&existing_file(&self.root, OsStr::new(GATE))?)?;
            self.owner
                .activity
                .check(&existing_file(&self.root, OsStr::new(ACTIVITY))?)?;
            self.owner.gate.check(&self.gate)?;
            let entries = bound_directory_entries(&self.root, 8)?
                .ok_or_else(|| invalid("unrecognized cache entries"))?;
            for entry in entries {
                if ![OWNER, GATE, ACTIVITY, RECEIPT, REPO, RETIRED]
                    .iter()
                    .any(|name| entry.name == *name)
                {
                    return Err(invalid(format!(
                        "unproven cache entry {:?} is retained",
                        entry.name
                    )));
                }
            }
            Ok(())
        }

        fn allocate(&self) -> io::Result<()> {
            self.check_root()?;
            mkdir(&self.root, OsStr::new(REPO))?;
            let repository = open_directory_at(&self.root, OsStr::new(REPO))?;
            let generation = super::super::unique_suffix();
            write_record(&repository, MARKER, &generation)?;
            let marker = existing_file(&repository, OsStr::new(MARKER))?;
            let receipt = Receipt {
                version: 1,
                root: self.owner.root,
                repository: Identity::of(&repository.metadata()?),
                marker: Identity::of(&marker.metadata()?),
                generation,
            };
            write_record(&self.root, RECEIPT, &receipt)?;
            Ok(())
        }

        pub(in super::super) fn repository(&self) -> Result<RepositoryHandle, String> {
            let get = || -> io::Result<RepositoryHandle> {
                self.check_root()?;
                let receipt = self.receipt()?;
                let directory = open_directory_at(&self.root, OsStr::new(REPO))?;
                self.prove_repository(&receipt, &directory)?;
                // The parent keeps the exclusive activity description while
                // reading; children inherit duplicates of that description.
                Ok(RepositoryHandle {
                    directory,
                    origin_lease: Some(self.activity.try_clone()?),
                    path: self.path.join(REPO),
                })
            };
            get().map_err(|error| {
                format!("cannot open origin cache {}: {error}", self.path.display())
            })
        }

        pub(in super::super) fn finish(self) -> Result<(), String> {
            let path = self.path.clone();
            self.finish_inner().map_err(|error| format!(
                "origin cache cleanup incomplete at {}: {error}; cache retained and further checks refuse until it is safely reclaimable",
                path.display()
            ))
        }

        fn finish_inner(mut self) -> io::Result<()> {
            // Open independently BEFORE dropping our copy. A duplicate would
            // share the child's lock and incorrectly report exclusivity.
            let next = existing_file(&self.root, OsStr::new(ACTIVITY))?;
            self.owner.activity.check(&next)?;
            drop(std::mem::replace(&mut self.activity, next));
            lock(&self.activity, libc::LOCK_EX)?;
            self.reclaim()
        }

        fn receipt(&self) -> io::Result<Receipt> {
            let receipt: Receipt = read_record(&self.root, RECEIPT)?;
            if receipt.version != 1 || receipt.root != self.owner.root {
                return Err(invalid("unproven cache receipt"));
            }
            Ok(receipt)
        }

        fn prove_repository(&self, receipt: &Receipt, directory: &File) -> io::Result<()> {
            receipt.repository.check(directory)?;
            let marker = existing_file(directory, OsStr::new(MARKER))?;
            receipt.marker.check(&marker)?;
            let generation: String = read_record_file(&marker)?;
            if generation != receipt.generation {
                return Err(invalid("origin cache generation changed"));
            }
            Ok(())
        }

        fn reclaim(&self) -> io::Result<()> {
            self.check_root()?;
            let entries = bound_directory_entries(&self.root, 8)?
                .ok_or_else(|| invalid("too many manager entries"))?;
            let has = |name: &str| entries.iter().any(|entry| entry.name == name);
            if !has(RECEIPT) {
                if has(REPO) || has(RETIRED) {
                    return Err(invalid("cache directory has no identity receipt"));
                }
                return Ok(());
            }
            let receipt = self.receipt()?;
            if has(REPO) && has(RETIRED) {
                return Err(invalid("multiple cache directories are retained"));
            }
            if has(REPO) {
                let repository = open_directory_at(&self.root, OsStr::new(REPO))?;
                self.prove_repository(&receipt, &repository)?;
                rename(
                    &self.root,
                    OsStr::new(REPO),
                    &self.root,
                    OsStr::new(RETIRED),
                )?;
            }
            if has(REPO) || has(RETIRED) {
                let retired = open_directory_at(&self.root, OsStr::new(RETIRED))?;
                self.prove_repository(&receipt, &retired)?;
                let mut budget = MAX_ENTRIES;
                // Preflight the whole tree before removing any entry. This
                // also refuses an oversized/crashed tree without partial work.
                let boundary = Boundary::of(&retired)?;
                walk_size(&retired, boundary, 0, &mut budget, true)?;
                budget = MAX_ENTRIES;
                self.clear(&retired, boundary, 0, &mut budget)?;
                self.prove_repository(&receipt, &retired)?;
                let marker = existing_file(&retired, OsStr::new(MARKER))?;
                self.dispose(&retired, OsStr::new(MARKER), &marker, false)?;
                self.dispose(&self.root, OsStr::new(RETIRED), &retired, true)?;
            }
            // A crash after removal but before receipt retirement is harmless:
            // there is no repository to reclaim, only this proven record.
            let record = existing_file(&self.root, OsStr::new(RECEIPT))?;
            self.dispose(&self.root, OsStr::new(RECEIPT), &record, false)?;
            self.root.sync_all()?;
            Ok(())
        }

        fn clear(
            &self,
            directory: &File,
            boundary: Boundary,
            depth: usize,
            budget: &mut usize,
        ) -> io::Result<()> {
            depth_check(depth)?;
            let entries = bound_directory_entries(directory, *budget)?
                .ok_or_else(|| invalid("cache entry budget exceeded"))?;
            for entry in entries {
                consume(budget)?;
                // Keep generation proof through partial cleanup/restarts.
                if depth == 0 && entry.name == MARKER {
                    continue;
                }
                let file = open_entry(directory, &entry.name)?;
                let metadata = checked_metadata(&file, boundary, true)?;
                if metadata.is_dir() {
                    self.clear(&file, boundary, depth + 1, budget)?;
                }
                self.dispose(directory, &entry.name, &file, metadata.is_dir())?;
            }
            Ok(())
        }

        fn dispose(
            &self,
            parent: &File,
            name: &OsStr,
            expected: &File,
            directory: bool,
        ) -> io::Result<()> {
            self.check_root()?;
            // No-replace means a previous interrupted disposal can never be
            // overwritten or auto-adopted. Verify AFTER moving, not just before.
            rename(parent, name, &self.root, OsStr::new(DISPOSAL))?;
            let displaced = open_entry(&self.root, OsStr::new(DISPOSAL))?;
            Identity::of(&expected.metadata()?).check(&displaced)?;
            if displaced.metadata()?.is_dir() != directory {
                return Err(invalid("cache disposal type changed"));
            }
            unlink(&self.root, OsStr::new(DISPOSAL), directory)
        }
    }

    pub(in super::super) fn create_template(handle: &RepositoryHandle) -> Result<(), String> {
        mkdir(&handle.directory, OsStr::new("empty-template"))
            .map_err(|error| format!("cannot create origin cache template: {error}"))
    }

    pub(in super::super) fn size(handle: &RepositoryHandle) -> Result<usize, String> {
        let measure = || -> io::Result<usize> {
            let named = RepositoryHandle::open(handle.path()).map_err(io::Error::other)?;
            Identity::of(&handle.directory.metadata()?).check(&named.directory)?;
            let mut budget = MAX_ENTRIES;
            walk_size(
                &handle.directory,
                Boundary::of(&handle.directory)?,
                0,
                &mut budget,
                false,
            )
        };
        measure().map_err(|error| format!("cannot inspect origin cache: {error}"))
    }

    fn walk_size(
        directory: &File,
        boundary: Boundary,
        depth: usize,
        budget: &mut usize,
        inactive: bool,
    ) -> io::Result<usize> {
        depth_check(depth)?;
        let mut size = 0usize;
        let entries = bound_directory_entries(directory, *budget)?
            .ok_or_else(|| invalid("cache entry budget exceeded"))?;
        for entry in entries {
            consume(budget)?;
            let file = match open_entry(directory, &entry.name) {
                Ok(file) => file,
                // Git renames temporary pack files while the monitor reads.
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            let metadata = checked_metadata(&file, boundary, inactive)?;
            let bytes = if metadata.is_dir() {
                walk_size(&file, boundary, depth + 1, budget, inactive)?
            } else {
                usize::try_from(metadata.len()).map_err(|_| invalid("cache size overflow"))?
            };
            size = size
                .checked_add(bytes)
                .ok_or_else(|| invalid("cache size overflow"))?;
        }
        Ok(size)
    }

    fn checked_metadata(file: &File, boundary: Boundary, inactive: bool) -> io::Result<Metadata> {
        let metadata = file.metadata()?;
        if Boundary::of(file)? != boundary
            || (!metadata.is_dir() && !metadata.is_file())
            || (inactive && metadata.is_file() && metadata.nlink() != 1)
        {
            return Err(invalid("cache contains a mount, special file or hard link"));
        }
        Ok(metadata)
    }
    #[derive(Clone, Copy, PartialEq, Eq)]
    struct Boundary {
        device: u64,
        #[cfg(target_os = "linux")]
        mount: u64,
    }
    impl Boundary {
        fn of(file: &File) -> io::Result<Self> {
            #[cfg(target_os = "linux")]
            let mount = {
                let mut stat = std::mem::MaybeUninit::<libc::statx>::zeroed();
                // AT_EMPTY_PATH asks about the held object; STATX_MNT_ID also
                // distinguishes bind mounts on the same device. Old kernels
                // that cannot prove this boundary refuse reclamation.
                if unsafe {
                    libc::statx(
                        file.as_raw_fd(),
                        c"".as_ptr(),
                        libc::AT_EMPTY_PATH,
                        libc::STATX_MNT_ID,
                        stat.as_mut_ptr(),
                    )
                } != 0
                {
                    return Err(io::Error::last_os_error());
                }
                let stat = unsafe { stat.assume_init() };
                if stat.stx_mask & libc::STATX_MNT_ID == 0 {
                    return Err(invalid("cache mount identity is unavailable"));
                }
                stat.stx_mnt_id
            };
            Ok(Self {
                device: file.metadata()?.dev(),
                #[cfg(target_os = "linux")]
                mount,
            })
        }
    }
    fn depth_check(depth: usize) -> io::Result<()> {
        if depth > MAX_DEPTH {
            Err(invalid("cache depth budget exceeded"))
        } else {
            Ok(())
        }
    }
    fn consume(budget: &mut usize) -> io::Result<()> {
        *budget = budget
            .checked_sub(1)
            .ok_or_else(|| invalid("cache entry budget exceeded"))?;
        Ok(())
    }
    fn private(file: &File, directory: bool) -> io::Result<()> {
        let metadata = file.metadata()?;
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
            || metadata.is_dir() != directory
            || (!directory && (!metadata.is_file() || metadata.nlink() != 1))
        {
            return Err(invalid(
                "cache manager is not a private owned directory or regular file",
            ));
        }
        Ok(())
    }
    fn invalid(message: impl Into<String>) -> io::Error {
        io::Error::other(message.into())
    }
    fn cstring(name: &OsStr) -> io::Result<CString> {
        CString::new(name.as_bytes()).map_err(|_| invalid("cache name contains NUL"))
    }
    fn mkdir(parent: &File, name: &OsStr) -> io::Result<()> {
        let name = cstring(name)?;
        if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    fn open_at(parent: &File, name: &OsStr, flags: i32) -> io::Result<File> {
        let name = cstring(name)?;
        let descriptor = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
                0o600,
            )
        };
        if descriptor < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(unsafe { File::from_raw_fd(descriptor) })
        }
    }
    // dev_t/ino_t differ between Linux and macOS (and target widths).
    #[allow(clippy::unnecessary_cast)]
    fn open_entry(parent: &File, name: &OsStr) -> io::Result<File> {
        let cname = cstring(name)?;
        let mut observed = std::mem::MaybeUninit::<libc::stat>::zeroed();
        if unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                cname.as_ptr(),
                observed.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        let observed = unsafe { observed.assume_init() };
        let kind = observed.st_mode & libc::S_IFMT;
        if kind != libc::S_IFDIR && kind != libc::S_IFREG {
            return Err(invalid(
                "cache contains a symbolic link or unsupported entry",
            ));
        }
        let file = open_at(
            parent,
            name,
            libc::O_RDONLY
                | if kind == libc::S_IFDIR {
                    libc::O_DIRECTORY
                } else {
                    0
                },
        )?;
        let metadata = file.metadata()?;
        if metadata.dev() != observed.st_dev as u64
            || metadata.ino() != observed.st_ino as u64
            || (!metadata.is_file() && !metadata.is_dir())
        {
            return Err(invalid("cache entry changed while opening"));
        }
        Ok(file)
    }
    fn existing_file(parent: &File, name: &OsStr) -> io::Result<File> {
        let file = open_at(parent, name, libc::O_RDWR)?;
        private(&file, false)?;
        Ok(file)
    }
    fn new_file(parent: &File, name: &str) -> io::Result<File> {
        open_at(
            parent,
            OsStr::new(name),
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
        )
    }
    fn lock(file: &File, operation: i32) -> io::Result<()> {
        // An unrelated thread can fork while we close our last copy. Its
        // close-on-exec duplicates briefly keep the old description alive.
        // Allow only that short handoff, never wait for a network operation.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(100);
        loop {
            if unsafe { libc::flock(file.as_raw_fd(), operation | libc::LOCK_NB) } == 0 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::WouldBlock || std::time::Instant::now() >= deadline {
                return Err(invalid(format!(
                    "origin cache is busy or its lease is unavailable: {error}"
                )));
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    fn write_record(parent: &File, name: &str, value: &impl Serialize) -> io::Result<()> {
        let bytes = serde_json::to_vec(value)?;
        let mut file = new_file(parent, name)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        parent.sync_all()
    }
    fn read_record<T: for<'a> Deserialize<'a>>(parent: &File, name: &str) -> io::Result<T> {
        let file = existing_file(parent, OsStr::new(name))?;
        read_record_file(&file)
    }
    fn read_record_file<T: for<'a> Deserialize<'a>>(file: &File) -> io::Result<T> {
        let mut bytes = Vec::new();
        file.take(MAX_RECORD + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_RECORD {
            return Err(invalid("cache record exceeds its budget"));
        }
        serde_json::from_slice(&bytes).map_err(io::Error::other)
    }
    fn unlink(parent: &File, name: &OsStr, directory: bool) -> io::Result<()> {
        let name = cstring(name)?;
        if unsafe {
            libc::unlinkat(
                parent.as_raw_fd(),
                name.as_ptr(),
                if directory { libc::AT_REMOVEDIR } else { 0 },
            )
        } == 0
        {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    fn rename(from: &File, name: &OsStr, to: &File, target: &OsStr) -> io::Result<()> {
        let name = cstring(name)?;
        let target = cstring(target)?;
        #[cfg(target_os = "linux")]
        let result = unsafe {
            libc::renameat2(
                from.as_raw_fd(),
                name.as_ptr(),
                to.as_raw_fd(),
                target.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        #[cfg(target_os = "macos")]
        let result = unsafe {
            libc::renameatx_np(
                from.as_raw_fd(),
                name.as_ptr(),
                to.as_raw_fd(),
                target.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::{
            fs,
            process::{Command, Stdio},
            time::{Duration, Instant},
        };
        use tempfile::TempDir;

        fn fixture() -> (TempDir, PathBuf) {
            let temp = TempDir::new().unwrap();
            let path = temp.path().join("cache");
            (temp, path)
        }
        fn payload(path: &Path) {
            fs::create_dir_all(path.join(REPO).join("objects/pack")).unwrap();
            fs::write(path.join(REPO).join("objects/pack/data"), b"cached objects").unwrap();
        }
        fn idle(path: &Path) {
            let mut entries = fs::read_dir(path)
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect::<Vec<_>>();
            entries.sort();
            assert_eq!(
                entries,
                [ACTIVITY, GATE, OWNER].map(std::ffi::OsString::from)
            );
        }
        fn refused(path: &Path) -> String {
            match Cache::acquire(path) {
                Ok(_) => panic!("cache acquisition should refuse"),
                Err(error) => error,
            }
        }

        #[test]
        fn repeated_checks_reclaim_all_idle_content_and_keep_bounded_metadata() {
            let (_temp, path) = fixture();
            for _ in 0..20 {
                let cache = Cache::acquire(&path).unwrap();
                payload(&path);
                cache.finish().unwrap();
                idle(&path);
            }
        }

        #[test]
        fn crash_residual_is_reclaimed_before_the_next_allocation() {
            let (_temp, path) = fixture();
            let cache = Cache::acquire(&path).unwrap();
            payload(&path);
            drop(cache); // Same descriptor release as a dead worker.
            let next = Cache::acquire(&path).unwrap();
            assert!(!path.join(REPO).join("objects").exists());
            next.finish().unwrap();
            idle(&path);
        }

        #[test]
        fn crash_after_quarantine_or_repository_removal_recovers() {
            for after_removal in [false, true] {
                let (_temp, path) = fixture();
                let cache = Cache::acquire(&path).unwrap();
                rename(
                    &cache.root,
                    OsStr::new(REPO),
                    &cache.root,
                    OsStr::new(RETIRED),
                )
                .unwrap();
                if after_removal {
                    fs::remove_file(path.join(RETIRED).join(MARKER)).unwrap();
                    fs::remove_dir(path.join(RETIRED)).unwrap();
                }
                drop(cache);
                Cache::acquire(&path).unwrap().finish().unwrap();
                idle(&path);
            }
        }

        #[test]
        fn active_readers_and_competing_managers_prevent_reclamation() {
            let (_temp, path) = fixture();
            let cache = Cache::acquire(&path).unwrap();
            payload(&path);
            let reader = cache.repository().unwrap();
            assert!(refused(&path).contains("busy"));
            assert!(cache.finish().unwrap_err().contains("busy"));
            assert!(path.join(REPO).join("objects/pack/data").exists());
            assert!(refused(&path).contains("busy"));
            drop(reader);
            Cache::acquire(&path).unwrap().finish().unwrap();
            idle(&path);
        }

        #[test]
        fn inherited_activity_lease_survives_the_direct_child() {
            let (_temp, path) = fixture();
            let cache = Cache::acquire(&path).unwrap();
            let reader = cache.repository().unwrap();
            let mut child = Command::new("sh");
            // The shell exits, leaving its descendant holding the inherited
            // lease. Neither its parent's exit nor a worker cancellation is
            // proof of inactivity.
            crate::git::bind_to_handle(&mut child, &reader);
            let output = child
                .args(["-c", "sleep 30 >/dev/null 2>&1 & echo $!"])
                .stdout(Stdio::piped())
                .output()
                .unwrap();
            assert!(output.status.success());
            let pid: i32 = String::from_utf8(output.stdout)
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            drop(reader);
            let result = cache.finish();
            let acquisition = refused(&path);
            // Always stop the fixture before asserting to avoid a leaked child.
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
            assert!(result.unwrap_err().contains("busy"));
            assert!(acquisition.contains("busy"));
            wait_for_reclaim(&path);
        }

        fn wait_for_reclaim(path: &Path) {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                match Cache::acquire(path) {
                    Ok(cache) => {
                        cache.finish().unwrap();
                        idle(path);
                        return;
                    }
                    Err(error) => {
                        assert!(Instant::now() < deadline, "{error}");
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
            }
        }

        #[test]
        fn lease_process_fixture() {
            let Some(path) = std::env::var_os("SKILLED_CACHE_LEASE_TEST") else {
                return;
            };
            let path = PathBuf::from(path);
            let cache = Cache::acquire(&path).unwrap();
            let reader = cache.repository().unwrap();
            let mut command = Command::new("sleep");
            crate::git::bind_to_handle(&mut command, &reader);
            let mut child = command
                .arg("30")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            fs::write(
                path.parent().unwrap().join("child-pid"),
                child.id().to_string(),
            )
            .unwrap();
            // The outer test kills this parent while its child is alive.
            child.wait().unwrap();
            drop(reader);
            cache.finish().unwrap();
        }

        #[test]
        fn parent_process_death_does_not_release_the_child_lease() {
            let (temp, path) = fixture();
            let mut parent = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "git::origin::cache::supported::tests::lease_process_fixture",
                    "--nocapture",
                ])
                .env("SKILLED_CACHE_LEASE_TEST", &path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            let pid: i32 = loop {
                if let Ok(pid) = fs::read_to_string(temp.path().join("child-pid"))
                    && let Ok(pid) = pid.parse()
                {
                    break pid;
                }
                if Instant::now() > deadline {
                    let _ = parent.kill();
                    let _ = parent.wait();
                    panic!("lease fixture did not start");
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            parent.kill().unwrap();
            parent.wait().unwrap();
            let acquisition = refused(&path);
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
            assert!(acquisition.contains("busy"));
            wait_for_reclaim(&path);
        }

        #[test]
        fn legacy_unmarked_and_redirected_managers_are_never_adopted() {
            let (temp, path) = fixture();
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o700))
                .unwrap();
            fs::create_dir(path.join("origin-old")).unwrap();
            fs::write(path.join("origin-old/sentinel"), b"keep").unwrap();
            assert!(refused(&path).contains("legacy"));
            assert_eq!(fs::read(path.join("origin-old/sentinel")).unwrap(), b"keep");
            let alias = temp.path().join("alias");
            std::os::unix::fs::symlink(&path, &alias).unwrap();
            assert!(Cache::acquire(&alias).is_err());
            assert!(!path.join(OWNER).exists());
        }

        #[test]
        fn missing_malformed_and_oversized_receipts_preserve_the_repository() {
            for record in [
                None,
                Some(b"{}".to_vec()),
                Some(vec![b'x'; MAX_RECORD as usize + 1]),
            ] {
                let (_temp, path) = fixture();
                let cache = Cache::acquire(&path).unwrap();
                payload(&path);
                drop(cache);
                if let Some(record) = record {
                    fs::write(path.join(RECEIPT), record).unwrap();
                } else {
                    fs::remove_file(path.join(RECEIPT)).unwrap();
                }
                for _ in 0..3 {
                    assert!(!refused(&path).is_empty());
                }
                assert_eq!(
                    fs::read(path.join(REPO).join("objects/pack/data")).unwrap(),
                    b"cached objects"
                );
                assert!(!path.join(RETIRED).exists());
            }
        }

        #[test]
        fn changed_repository_root_and_lock_identities_refuse() {
            for name in [REPO, GATE, ACTIVITY] {
                let (temp, path) = fixture();
                let cache = Cache::acquire(&path).unwrap();
                payload(&path);
                drop(cache);
                fs::rename(path.join(name), temp.path().join("original")).unwrap();
                if name == REPO {
                    fs::create_dir(path.join(name)).unwrap();
                } else {
                    fs::write(path.join(name), b"").unwrap();
                    fs::set_permissions(
                        path.join(name),
                        std::os::unix::fs::PermissionsExt::from_mode(0o600),
                    )
                    .unwrap();
                }
                assert!(refused(&path).contains("identity"));
                assert!(temp.path().join("original").exists());
            }
            let (temp, path) = fixture();
            let cache = Cache::acquire(&path).unwrap();
            payload(&path);
            fs::rename(&path, temp.path().join("original")).unwrap();
            fs::create_dir(&path).unwrap();
            fs::write(path.join("sentinel"), b"keep").unwrap();
            assert!(cache.finish().is_err());
            assert_eq!(fs::read(path.join("sentinel")).unwrap(), b"keep");
            assert!(temp.path().join("original").join(REPO).exists());
        }

        #[test]
        fn nested_symlinks_hardlinks_and_special_files_preserve_outside_content() {
            for kind in ["symlink", "hardlink", "fifo"] {
                let (temp, path) = fixture();
                let cache = Cache::acquire(&path).unwrap();
                payload(&path);
                let outside = temp.path().join("outside");
                fs::write(&outside, b"outside sentinel").unwrap();
                let target = path.join(REPO).join("objects/unsafe");
                match kind {
                    "symlink" => std::os::unix::fs::symlink(&outside, &target).unwrap(),
                    "hardlink" => fs::hard_link(&outside, &target).unwrap(),
                    _ => {
                        let name = cstring(target.as_os_str()).unwrap();
                        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
                    }
                }
                assert!(cache.finish().is_err());
                assert!(!refused(&path).is_empty());
                assert_eq!(fs::read(&outside).unwrap(), b"outside sentinel");
                assert!(path.join(RETIRED).join("objects/pack/data").exists());
            }
        }

        #[test]
        fn substitution_at_disposal_preserves_the_displaced_stranger() {
            let (temp, path) = fixture();
            let cache = Cache::acquire(&path).unwrap();
            let repository = open_directory_at(&cache.root, OsStr::new(REPO)).unwrap();
            fs::write(path.join(REPO).join("victim"), b"owned").unwrap();
            let expected = open_entry(&repository, OsStr::new("victim")).unwrap();
            fs::rename(path.join(REPO).join("victim"), temp.path().join("original")).unwrap();
            fs::write(path.join(REPO).join("victim"), b"stranger").unwrap();
            assert!(
                cache
                    .dispose(&repository, OsStr::new("victim"), &expected, false)
                    .is_err()
            );
            assert_eq!(fs::read(path.join(DISPOSAL)).unwrap(), b"stranger");
            assert!(cache.finish().is_err());
            assert!(refused(&path).contains("disposal"));
            assert_eq!(fs::read(temp.path().join("original")).unwrap(), b"owned");
        }

        #[test]
        fn interrupted_disposal_never_gets_adopted_on_retry() {
            let (_temp, path) = fixture();
            let cache = Cache::acquire(&path).unwrap();
            payload(&path);
            fs::write(path.join(DISPOSAL), b"unknown interrupted deletion").unwrap();
            drop(cache);
            for _ in 0..3 {
                assert!(refused(&path).contains("disposal"));
            }
            assert_eq!(
                fs::read(path.join(DISPOSAL)).unwrap(),
                b"unknown interrupted deletion"
            );
            assert!(path.join(REPO).join("objects/pack/data").exists());
        }

        #[test]
        fn active_accounting_allows_git_pack_publication_but_cleanup_refuses_hardlinks() {
            let (_temp, path) = fixture();
            let cache = Cache::acquire(&path).unwrap();
            payload(&path);
            let reader = cache.repository().unwrap();
            let original = path.join(REPO).join("objects/pack/data");
            let published = path.join(REPO).join("objects/pack/final");
            fs::hard_link(&original, &published).unwrap();
            assert!(
                size(&reader).is_ok(),
                "live Git can link a pack before unlinking its temporary name"
            );
            let held = open_entry(&reader.directory, OsStr::new("objects")).unwrap();
            let boundary = Boundary::of(&held).unwrap();
            let file = File::open(&original).unwrap();
            assert!(checked_metadata(&file, boundary, true).is_err());
            fs::remove_file(&original).unwrap();
            assert!(size(&reader).is_ok());
            fs::remove_file(&published).unwrap();
            assert!(
                checked_metadata(&file, boundary, false).is_ok(),
                "an opened temporary can disappear during monitoring"
            );
            assert!(checked_metadata(&file, boundary, true).is_err());
            drop(reader);
            cache.finish().unwrap();
        }

        #[test]
        fn directory_inode_reuse_alone_cannot_authorize_reclamation() {
            let (temp, path) = fixture();
            let cache = Cache::acquire(&path).unwrap();
            let mut receipt = cache.receipt().unwrap();
            fs::rename(path.join(REPO), temp.path().join("original")).unwrap();
            fs::create_dir(path.join(REPO)).unwrap();
            let replacement = open_directory_at(&cache.root, OsStr::new(REPO)).unwrap();
            write_record(&replacement, MARKER, &"another generation").unwrap();
            let marker = existing_file(&replacement, OsStr::new(MARKER)).unwrap();
            // Model filesystem inode reuse deterministically, even for both
            // objects: the independent generation contents must still agree.
            receipt.repository = Identity::of(&replacement.metadata().unwrap());
            receipt.marker = Identity::of(&marker.metadata().unwrap());
            fs::write(path.join(RECEIPT), serde_json::to_vec(&receipt).unwrap()).unwrap();
            fs::write(path.join(REPO).join("sentinel"), b"stranger").unwrap();
            assert!(cache.finish().unwrap_err().contains("generation"));
            assert_eq!(
                fs::read(path.join(REPO).join("sentinel")).unwrap(),
                b"stranger"
            );
        }

        #[test]
        fn partial_tree_cleanup_keeps_generation_proof_for_retry() {
            let (_temp, path) = fixture();
            let cache = Cache::acquire(&path).unwrap();
            payload(&path);
            rename(
                &cache.root,
                OsStr::new(REPO),
                &cache.root,
                OsStr::new(RETIRED),
            )
            .unwrap();
            fs::remove_file(path.join(RETIRED).join("objects/pack/data")).unwrap();
            drop(cache);
            Cache::acquire(&path).unwrap().finish().unwrap();
            idle(&path);
        }

        #[test]
        fn traversal_budgets_and_mount_boundaries_refuse() {
            let (_temp, path) = fixture();
            let cache = Cache::acquire(&path).unwrap();
            payload(&path);
            let reader = cache.repository().unwrap();
            let boundary = Boundary::of(&reader.directory).unwrap();
            assert!(
                walk_size(
                    &reader.directory,
                    boundary,
                    MAX_DEPTH + 1,
                    &mut { MAX_ENTRIES },
                    true
                )
                .is_err()
            );
            assert!(walk_size(&reader.directory, boundary, 0, &mut 1, true).is_err());
            let mut wrong = boundary;
            wrong.device = boundary.device.wrapping_add(1);
            assert!(walk_size(&reader.directory, wrong, 0, &mut { MAX_ENTRIES }, true).is_err());
            #[cfg(target_os = "linux")]
            {
                let wrong = Boundary {
                    mount: boundary.mount.wrapping_add(1),
                    ..boundary
                };
                assert!(
                    walk_size(&reader.directory, wrong, 0, &mut { MAX_ENTRIES }, true).is_err()
                );
            }
            drop(reader);
            cache.finish().unwrap();
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) use supported::{create_template, size};

#[cfg(all(test, not(any(target_os = "linux", target_os = "macos"))))]
#[test]
fn unsupported_reclamation_refuses_without_creating_a_cache() {
    let temporary = tempfile::TempDir::new().unwrap();
    let root = temporary.path().join("cache");
    assert!(Cache::acquire(&root).is_err());
    assert!(!root.exists());
}
