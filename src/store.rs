use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

use crate::{
    AgentKind, Error, Result,
    operations::{Receipt, ReceiptFailure},
    source::{
        CatalogClassification, CatalogProposal, Compatibility, InspectedSource, RegisteredSource,
        SourcePreview, contains_revision, inspect_local_source,
    },
    validation::InspectionBudget,
};

const SCHEMA_VERSION: i64 = 5;

pub(crate) struct Store {
    connection: Connection,
    database_path: PathBuf,
    #[cfg(test)]
    fail_next: std::cell::RefCell<Option<MetadataOperation>>,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MetadataOperation {
    CompleteSetup,
    ResetSetup,
    RegisterSource,
    ReadSources,
    ReadReceipts,
    RecordReceipt,
}

pub(crate) struct StartupState {
    pub(crate) setup_complete: bool,
    pub(crate) agent_selections: Option<[bool; 3]>,
    pub(crate) sources: Vec<RegisteredSource>,
}

impl Store {
    /// Open only a physical application-data directory and regular database
    /// leaf, without following either checked leaf as a symbolic link.
    ///
    /// Ancestors retain ordinary operating-system resolution so a symlinked
    /// home remains supported. `SQLITE_OPEN_NOFOLLOW` is defense in depth for
    /// the database leaf; one pathname window remains between the filesystem
    /// classification and SQLite's open.
    pub(crate) fn open(data_dir: &Path) -> Result<Self> {
        let new_data_dir = match fs::symlink_metadata(data_dir) {
            Ok(metadata) => {
                if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
                    return Err(unsafe_metadata_leaf(
                        data_dir,
                        "application-data path is not a physical directory",
                    ));
                }
                false
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir_all(data_dir)?;
                let metadata = fs::symlink_metadata(data_dir)?;
                if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
                    return Err(unsafe_metadata_leaf(
                        data_dir,
                        "created application-data path is not a physical directory",
                    ));
                }
                true
            }
            Err(error) => return Err(error.into()),
        };
        let database_path = data_dir.join("skilled.sqlite3");
        // SQLite's NOFOLLOW flag rejects symlinks in the full pathname on
        // platforms such as macOS (where `/var` itself is commonly a link).
        // Canonicalizing the already-classified physical directory resolves
        // only the explicitly-supported ancestor chain; the database leaf is
        // still appended afterwards and therefore never followed here.
        let sqlite_database_path = data_dir.canonicalize()?.join("skilled.sqlite3");
        let mut flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW;
        match fs::symlink_metadata(&database_path) {
            Ok(metadata) => {
                if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
                    return Err(unsafe_metadata_leaf(
                        &database_path,
                        "database path is not a regular file",
                    ));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && new_data_dir => {
                flags |= OpenFlags::SQLITE_OPEN_CREATE;
            }
            Err(error) => return Err(error.into()),
        }
        let mut connection = Connection::open_with_flags(&sqlite_database_path, flags)?;
        migrate(&mut connection, &sqlite_database_path)?;
        connection.execute_batch("PRAGMA foreign_keys = ON;")?;
        Ok(Self {
            connection,
            database_path,
            #[cfg(test)]
            fail_next: std::cell::RefCell::new(None),
        })
    }

    pub(crate) fn database_path(&self) -> &Path {
        &self.database_path
    }

    /// Read every startup value without refreshing source rows in SQLite.
    pub(crate) fn load_startup_state_read_only(&self) -> Result<StartupState> {
        Ok(StartupState {
            setup_complete: self.setup_complete()?,
            agent_selections: self.agent_selections()?,
            sources: self.load_registered_sources(false)?,
        })
    }

    pub(crate) fn setup_complete(&self) -> Result<bool> {
        let value = self
            .connection
            .query_row(
                "SELECT value FROM settings WHERE key = 'setup_complete'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(value.as_deref() == Some("true"))
    }

    pub(crate) fn set_setup_complete(&self, complete: bool) -> Result<()> {
        #[cfg(test)]
        self.fail_if(MetadataOperation::ResetSetup)?;
        self.connection.execute(
            "INSERT INTO settings (key, value) VALUES ('setup_complete', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![if complete { "true" } else { "false" }],
        )?;
        Ok(())
    }

    pub(crate) fn agent_selections(&self) -> Result<Option<[bool; 3]>> {
        let mut selections = [false; 3];
        for (index, agent) in AGENT_IDENTIFIERS.iter().enumerate() {
            let selected = self
                .connection
                .query_row(
                    "SELECT selected FROM configured_agents WHERE agent = ?1",
                    params![agent],
                    |row| row.get::<_, bool>(0),
                )
                .optional()?;
            let Some(selected) = selected else {
                return Ok(None);
            };
            selections[index] = selected;
        }
        Ok(Some(selections))
    }

    pub(crate) fn complete_setup(&mut self, selections: [bool; 3]) -> Result<()> {
        #[cfg(test)]
        self.fail_if(MetadataOperation::CompleteSetup)?;
        let transaction = self.connection.transaction()?;
        for (agent, selected) in AGENT_IDENTIFIERS.into_iter().zip(selections) {
            transaction.execute(
                "INSERT INTO configured_agents (agent, selected) VALUES (?1, ?2)
                 ON CONFLICT(agent) DO UPDATE SET selected = excluded.selected",
                params![agent, selected],
            )?;
        }
        transaction.execute(
            "INSERT INTO settings (key, value) VALUES ('setup_complete', 'true')
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Check every path against the representation the receipt table stores.
    ///
    /// The install guard calls this before creating anything, then
    /// [`Self::record_receipt`] repeats it so no caller can bypass the table's
    /// contract and turn a path conversion into a post-write surprise.
    pub(crate) fn ensure_receipt_recordable(&self, receipt: &Receipt) -> Result<()> {
        stored_path(receipt.link_path())?;
        stored_path(receipt.link_target())?;
        receipt
            .catalog_relative_path()
            .map(stored_path)
            .transpose()?;
        receipt
            .variant_relative_path()
            .map(stored_path)
            .transpose()?;
        Ok(())
    }

    /// Record that Skilled created one particular link.
    ///
    /// One statement, and therefore its own transaction: the receipt is written
    /// the moment the link exists, so a crash between two targets still leaves
    /// the store describing exactly what is on disk.
    ///
    /// A receipt identical to one already recorded is left alone rather than
    /// refused. Timestamps are whole seconds, so a link removed and put back
    /// inside one second would otherwise fail its insert and leave Skilled
    /// reporting that it does not own something it just created — when the
    /// receipt it needs is already there.
    pub(crate) fn record_receipt(
        &self,
        receipt: &Receipt,
    ) -> std::result::Result<(), ReceiptFailure> {
        #[cfg(test)]
        self.fail_if(MetadataOperation::RecordReceipt)
            .map_err(|error| {
                ReceiptFailure::Metadata(crate::MetadataFailure::new(
                    self.database_path.clone(),
                    error.to_string(),
                ))
            })?;
        self.ensure_receipt_recordable(receipt)
            .map_err(|error| ReceiptFailure::Other(error.to_string()))?;
        let link_path = stored_path(receipt.link_path())
            .map_err(|error| ReceiptFailure::Other(error.to_string()))?;
        let link_target = stored_path(receipt.link_target())
            .map_err(|error| ReceiptFailure::Other(error.to_string()))?;
        let catalog_relative_path = receipt
            .catalog_relative_path()
            .map(stored_path)
            .transpose()
            .map_err(|error| ReceiptFailure::Other(error.to_string()))?;
        let variant_relative_path = receipt
            .variant_relative_path()
            .map(stored_path)
            .transpose()
            .map_err(|error| ReceiptFailure::Other(error.to_string()))?;
        self.connection
            .execute(
                "INSERT INTO operation_receipts
                (created_at, operation, agent, skill_name, link_path, link_target,
                 source_id, catalog_relative_path, variant_relative_path)
             VALUES (?1, 'install', ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT DO NOTHING",
                params![
                    current_timestamp(),
                    agent_identifier(receipt.agent()),
                    receipt.skill_name(),
                    link_path,
                    link_target,
                    receipt.source_id(),
                    catalog_relative_path,
                    variant_relative_path,
                ],
            )
            .map_err(|error| {
                ReceiptFailure::Metadata(crate::MetadataFailure::new(
                    self.database_path.clone(),
                    error.to_string(),
                ))
            })?;
        Ok(())
    }

    /// Every ownership receipt, oldest first.
    ///
    /// A row naming an agent this build does not know is an error rather than a
    /// row to skip: a receipt Skilled cannot read is ownership it would go on to
    /// deny, and denying it would let the next plan treat its own link as a
    /// stranger's.
    pub(crate) fn receipts(&self) -> Result<Vec<Receipt>> {
        #[cfg(test)]
        self.fail_if(MetadataOperation::ReadReceipts)?;
        let mut statement = self.connection.prepare(
            "SELECT agent, skill_name, link_path, link_target, source_id,
                    catalog_relative_path, variant_relative_path
             FROM operation_receipts ORDER BY created_at, id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .map(
                |(
                    agent,
                    skill_name,
                    link_path,
                    link_target,
                    source_id,
                    catalog_relative_path,
                    variant_relative_path,
                )| {
                    Ok(Receipt::new(
                        agent_kind(&agent)?,
                        skill_name,
                        PathBuf::from(link_path),
                        PathBuf::from(link_target),
                        source_id,
                        catalog_relative_path.map(PathBuf::from),
                        variant_relative_path.map(PathBuf::from),
                    ))
                },
            )
            .collect()
    }

    pub(crate) fn register_source(&mut self, preview: &SourcePreview) -> Result<()> {
        #[cfg(test)]
        self.fail_if(MetadataOperation::RegisterSource)?;
        let source = preview.inspected();
        let canonical_path = path_text(source.git_top_level())?;
        let label = source
            .git_top_level()
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| Error::InvalidSourcePath(source.git_top_level().to_path_buf()))?;
        let scanned_at = current_timestamp();
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO source_repositories
                (label, canonical_path, remote_url, branch, head_revision, dirty, dirty_known, last_scan_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(canonical_path) DO UPDATE SET
                label = excluded.label,
                remote_url = excluded.remote_url,
                branch = excluded.branch,
                head_revision = excluded.head_revision,
                dirty = excluded.dirty,
                dirty_known = excluded.dirty_known,
                last_scan_at = excluded.last_scan_at",
            params![
                label,
                canonical_path,
                source.remote_url(),
                source.branch(),
                source.head(),
                source.dirty().unwrap_or(false),
                source.dirty().is_some(),
                scanned_at,
            ],
        )?;
        let source_id: i64 = transaction.query_row(
            "SELECT id FROM source_repositories WHERE canonical_path = ?1",
            params![canonical_path],
            |row| row.get(0),
        )?;
        transaction.execute(
            "DELETE FROM catalog_roots WHERE source_id = ?1",
            params![source_id],
        )?;
        for catalog in preview.catalogs() {
            if !catalog.included() {
                continue;
            }
            transaction.execute(
                "INSERT INTO catalog_roots
                    (source_id, relative_path, classification, claude_code, codex, opencode)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    source_id,
                    path_text(catalog.relative_path())?,
                    match catalog.classification() {
                        CatalogClassification::Common => "common",
                        CatalogClassification::AgentSpecific => "agent-specific",
                    },
                    catalog.compatibility().claude_code(),
                    catalog.compatibility().codex(),
                    catalog.compatibility().opencode(),
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn registered_sources(&self) -> Result<Vec<RegisteredSource>> {
        self.load_registered_sources(true)
    }

    fn load_registered_sources(&self, refresh: bool) -> Result<Vec<RegisteredSource>> {
        #[cfg(test)]
        self.fail_if(MetadataOperation::ReadSources)?;
        let mut statement = self.connection.prepare(
            "SELECT id, label, canonical_path, remote_url, branch, head_revision, dirty, dirty_known, last_scan_at
             FROM source_repositories ORDER BY label, canonical_path",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, bool>(6)?,
                row.get::<_, bool>(7)?,
                row.get::<_, i64>(8)?,
            ))
        })?;
        let stored = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        drop(statement);

        let mut sources = Vec::with_capacity(stored.len());
        for (id, label, path, remote_url, branch, head, dirty, dirty_known, last_scan_at) in stored
        {
            let git_top_level = PathBuf::from(path);
            let stored_inspected = InspectedSource::from_stored(
                git_top_level.clone(),
                branch,
                head,
                remote_url,
                dirty_known.then_some(dirty),
            );
            let (inspected, source_error, refreshed_at) = match inspect_local_source(&git_top_level)
            {
                Ok(current) if current.git_top_level() != git_top_level => (
                    stored_inspected.clone(),
                    Some(format!(
                        "source now resolves to a different Git checkout: {}",
                        current.git_top_level().display()
                    )),
                    last_scan_at,
                ),
                Ok(current) => match contains_revision(&git_top_level, stored_inspected.head()) {
                    Ok(true) if refresh => {
                        let refreshed_at = current_timestamp();
                        self.connection.execute(
                            "UPDATE source_repositories SET
                                remote_url = ?1,
                                branch = ?2,
                                head_revision = ?3,
                                dirty = ?4,
                                dirty_known = ?5,
                                last_scan_at = ?6
                             WHERE id = ?7",
                            params![
                                current.remote_url(),
                                current.branch(),
                                current.head(),
                                current.dirty().unwrap_or(false),
                                current.dirty().is_some(),
                                refreshed_at,
                                id,
                            ],
                        )?;
                        (current, None, refreshed_at)
                    }
                    Ok(true) => (current, None, last_scan_at),
                    Ok(false) => (
                        stored_inspected.clone(),
                        Some("source path now contains a different Git checkout".to_owned()),
                        last_scan_at,
                    ),
                    Err(error) => (
                        stored_inspected.clone(),
                        Some(error.to_string()),
                        last_scan_at,
                    ),
                },
                Err(error) => (
                    stored_inspected.clone(),
                    Some(error.to_string()),
                    last_scan_at,
                ),
            };
            let mut catalog_statement = self.connection.prepare(
                "SELECT relative_path, classification, claude_code, codex, opencode
                 FROM catalog_roots WHERE source_id = ?1 ORDER BY relative_path",
            )?;
            let catalog_rows = catalog_statement.query_map(params![id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, bool>(2)?,
                    row.get::<_, bool>(3)?,
                    row.get::<_, bool>(4)?,
                ))
            })?;
            let mut catalogs = Vec::new();
            let mut budget = InspectionBudget::source_scan();
            for row in catalog_rows {
                let (relative_path, classification, claude_code, codex, opencode) = row?;
                let classification = match classification.as_str() {
                    "common" => CatalogClassification::Common,
                    "agent-specific" => CatalogClassification::AgentSpecific,
                    value => return Err(Error::InvalidCatalogClassification(value.to_owned())),
                };
                let relative_path = PathBuf::from(relative_path);
                let compatibility = Compatibility::from_flags(claude_code, codex, opencode);
                catalogs.push(match &source_error {
                    Some(error) => CatalogProposal::from_unavailable(
                        relative_path,
                        classification,
                        compatibility,
                        error,
                    ),
                    None => CatalogProposal::from_confirmed(
                        &git_top_level,
                        relative_path,
                        classification,
                        compatibility,
                        &mut budget,
                    ),
                });
            }
            sources.push(RegisteredSource::new(
                id,
                label,
                inspected,
                catalogs,
                refreshed_at,
                source_error,
            ));
        }
        Ok(sources)
    }

    #[cfg(test)]
    pub(crate) fn fail_next(&self, operation: MetadataOperation) {
        *self.fail_next.borrow_mut() = Some(operation);
    }

    #[cfg(test)]
    fn fail_if(&self, operation: MetadataOperation) -> Result<()> {
        if self.fail_next.borrow().as_ref() != Some(&operation) {
            return Ok(());
        }
        self.fail_next.borrow_mut().take();
        Err(Error::Database(rusqlite::Error::InvalidQuery))
    }
}

fn unsafe_metadata_leaf(path: &Path, message: &str) -> Error {
    Error::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!("{message}: {}", path.display()),
    ))
}

fn path_text(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| Error::InvalidSourcePath(path.to_path_buf()))
}

/// A path stored outside the source tables, where "not a portable catalog path"
/// would be the wrong thing to say about it.
fn stored_path(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| Error::UnrepresentablePath(path.to_path_buf()))
}

/// The stored spelling of an agent, shared by every table that names one.
const AGENT_IDENTIFIERS: [&str; 3] = ["claude-code", "codex", "opencode"];

fn agent_identifier(agent: AgentKind) -> &'static str {
    AGENT_IDENTIFIERS[agent.index()]
}

fn agent_kind(identifier: &str) -> Result<AgentKind> {
    AgentKind::ALL
        .into_iter()
        .find(|agent| agent_identifier(*agent) == identifier)
        .ok_or_else(|| Error::InvalidStoredAgent(identifier.to_owned()))
}

fn current_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(all(test, unix))]
mod tests {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};

    use super::*;

    #[test]
    fn an_ownership_receipt_requires_representable_paths_before_it_can_be_written() {
        let temporary = tempfile::tempdir().expect("temporary data directory");
        let store = Store::open(&temporary.path().join("data")).expect("open store");
        let link_path = PathBuf::from(OsString::from_vec(b"link-\xff".to_vec()));
        let receipt = Receipt::new(
            AgentKind::ClaudeCode,
            "portable".to_owned(),
            link_path.clone(),
            PathBuf::from("/source/portable"),
            None,
            None,
            None,
        );

        let result = store.ensure_receipt_recordable(&receipt);

        assert!(matches!(
            result,
            Err(Error::UnrepresentablePath(path)) if path == link_path
        ));
    }
}

#[derive(Clone, Copy)]
struct Migration {
    version: i64,
    destructive: bool,
    sql: &'static str,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        destructive: false,
        sql: "CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY NOT NULL,
                value TEXT NOT NULL
              );",
    },
    Migration {
        version: 2,
        destructive: false,
        sql: "CREATE TABLE configured_agents (
                agent TEXT PRIMARY KEY NOT NULL,
                selected INTEGER NOT NULL CHECK (selected IN (0, 1))
              );",
    },
    Migration {
        version: 3,
        destructive: false,
        sql: "CREATE TABLE source_repositories (
                id INTEGER PRIMARY KEY,
                label TEXT NOT NULL,
                canonical_path TEXT NOT NULL UNIQUE,
                remote_url TEXT,
                branch TEXT,
                head_revision TEXT NOT NULL,
                dirty INTEGER NOT NULL CHECK (dirty IN (0, 1)),
                last_scan_at INTEGER NOT NULL
              );
              CREATE TABLE catalog_roots (
                id INTEGER PRIMARY KEY,
                source_id INTEGER NOT NULL REFERENCES source_repositories(id) ON DELETE CASCADE,
                relative_path TEXT NOT NULL,
                classification TEXT NOT NULL CHECK (classification IN ('common', 'agent-specific')),
                claude_code INTEGER NOT NULL CHECK (claude_code IN (0, 1)),
                codex INTEGER NOT NULL CHECK (codex IN (0, 1)),
                opencode INTEGER NOT NULL CHECK (opencode IN (0, 1)),
                UNIQUE(source_id, relative_path)
              );",
    },
    Migration {
        version: 4,
        destructive: false,
        sql: "ALTER TABLE source_repositories ADD COLUMN
                dirty_known INTEGER NOT NULL DEFAULT 1 CHECK (dirty_known IN (0, 1));",
    },
    Migration {
        version: 5,
        destructive: false,
        // `source_id` deliberately carries no foreign key: a receipt is
        // ownership evidence and must outlive the source it came from.
        sql: "CREATE TABLE operation_receipts (
                id INTEGER PRIMARY KEY,
                created_at INTEGER NOT NULL,
                operation TEXT NOT NULL CHECK (operation IN ('install')),
                agent TEXT NOT NULL,
                skill_name TEXT NOT NULL,
                link_path TEXT NOT NULL,
                link_target TEXT NOT NULL,
                source_id INTEGER,
                catalog_relative_path TEXT,
                variant_relative_path TEXT,
                UNIQUE (agent, link_path, link_target, created_at)
              );",
    },
];

fn migrate(connection: &mut Connection, database_path: &Path) -> Result<()> {
    migrate_with(connection, database_path, MIGRATIONS).map(|_| ())
}

/// Apply every pending migration in one transaction, after one consistent
/// backup of the original database when any pending step is destructive.
fn migrate_with(
    connection: &mut Connection,
    database_path: &Path,
    migrations: &[Migration],
) -> Result<Option<PathBuf>> {
    let current_version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let supported = migrations
        .last()
        .map_or(SCHEMA_VERSION, |migration| migration.version);
    if current_version > supported {
        return Err(Error::UnsupportedSchema {
            found: current_version,
            supported,
        });
    }
    let pending: Vec<Migration> = migrations
        .iter()
        .copied()
        .filter(|migration| migration.version > current_version)
        .collect();
    if pending.is_empty() {
        return Ok(None);
    }
    let backup = pending
        .iter()
        .any(|migration| migration.destructive)
        .then(|| backup_database(connection, database_path, current_version))
        .transpose()?;

    let transaction = connection.transaction()?;
    for migration in pending {
        transaction.execute_batch(migration.sql)?;
        transaction.pragma_update(None, "user_version", migration.version)?;
    }
    transaction.commit()?;
    Ok(backup)
}

static BACKUP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn backup_name(original_version: i64) -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let counter = BACKUP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!(
        "skilled.sqlite3.backup-v{original_version}-{timestamp}-{}-{counter}",
        std::process::id()
    )
}

fn valid_backup_component(name: &str) -> bool {
    let path = Path::new(name);
    matches!(
        path.components().collect::<Vec<_>>().as_slice(),
        [std::path::Component::Normal(_)]
    )
}

/// Create a consistent database backup at a unique final pathname.
///
/// The filename generator is constrained to one normal component and the
/// physical data-directory leaf is rechecked immediately before `VACUUM
/// INTO`. SQLite creates the final file and refuses an occupied one; no file
/// is replaced or removed. One pathname window remains between the recheck and
/// SQLite's destination open.
fn backup_database(
    connection: &Connection,
    database_path: &Path,
    original_version: i64,
) -> Result<PathBuf> {
    backup_database_with(connection, database_path, original_version, backup_name)
}

fn backup_database_with(
    connection: &Connection,
    database_path: &Path,
    original_version: i64,
    mut name_for: impl FnMut(i64) -> String,
) -> Result<PathBuf> {
    let data_dir = database_path.parent().ok_or_else(|| {
        unsafe_metadata_leaf(database_path, "database path has no parent directory")
    })?;
    let data_metadata = fs::symlink_metadata(data_dir)?;
    if !data_metadata.file_type().is_dir() || data_metadata.file_type().is_symlink() {
        return Err(unsafe_metadata_leaf(
            data_dir,
            "backup directory is not a physical directory",
        ));
    }
    let database_metadata = fs::symlink_metadata(database_path)?;
    if !database_metadata.file_type().is_file() || database_metadata.file_type().is_symlink() {
        return Err(unsafe_metadata_leaf(
            database_path,
            "database path is not a regular file",
        ));
    }

    for _ in 0..128 {
        let name = name_for(original_version);
        if !valid_backup_component(&name) {
            return Err(unsafe_metadata_leaf(
                Path::new(&name),
                "backup name is not one normal filename component",
            ));
        }
        let candidate = data_dir.join(name);
        match fs::symlink_metadata(&candidate) {
            Ok(_) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let candidate_text = candidate
            .to_str()
            .ok_or_else(|| Error::UnrepresentablePath(candidate.clone()))?;
        connection.execute("VACUUM INTO ?1", params![candidate_text])?;
        return Ok(candidate);
    }
    Err(Error::Io(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not choose an unused metadata backup path",
    )))
}

#[cfg(test)]
mod migration_tests {
    use super::*;

    #[test]
    fn a_destructive_migration_backs_up_the_original_database_first() {
        let temporary = tempfile::tempdir().expect("temporary data directory");
        let database = temporary.path().join("skilled.sqlite3");
        let mut connection = Connection::open(&database).expect("create database");
        connection
            .execute_batch(
                "CREATE TABLE legacy (value TEXT NOT NULL);\n\
                 INSERT INTO legacy VALUES ('before');",
            )
            .expect("create original schema");
        let steps = [
            Migration {
                version: 1,
                destructive: false,
                sql: "CREATE TABLE additive (value TEXT);",
            },
            Migration {
                version: 2,
                destructive: true,
                sql: "DROP TABLE legacy; CREATE TABLE replacement (value TEXT);",
            },
        ];

        let backup = migrate_with(&mut connection, &database, &steps)
            .expect("migrate with backup")
            .expect("backup path");

        assert!(backup.file_name().unwrap().to_string_lossy().contains("v0"));
        let backup_connection =
            Connection::open_with_flags(&backup, OpenFlags::SQLITE_OPEN_READ_ONLY)
                .expect("open backup");
        assert_eq!(
            backup_connection
                .query_row("SELECT value FROM legacy", [], |row| row
                    .get::<_, String>(0))
                .expect("read original row"),
            "before"
        );
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .expect("read migrated version"),
            2
        );
        assert!(connection.prepare("SELECT * FROM legacy").is_err());
    }

    #[test]
    fn a_failing_destructive_step_rolls_back_the_complete_pending_sequence() {
        let temporary = tempfile::tempdir().expect("temporary data directory");
        let database = temporary.path().join("skilled.sqlite3");
        let mut connection = Connection::open(&database).expect("create database");
        connection
            .execute_batch("CREATE TABLE legacy (value TEXT); INSERT INTO legacy VALUES ('kept');")
            .expect("create original schema");
        let steps = [
            Migration {
                version: 1,
                destructive: false,
                sql: "CREATE TABLE additive (value TEXT);",
            },
            Migration {
                version: 2,
                destructive: true,
                sql: "DROP TABLE legacy; THIS IS NOT SQL;",
            },
        ];

        assert!(migrate_with(&mut connection, &database, &steps).is_err());

        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .expect("read original version"),
            0
        );
        assert_eq!(
            connection
                .query_row("SELECT value FROM legacy", [], |row| row
                    .get::<_, String>(0))
                .expect("read retained row"),
            "kept"
        );
        assert!(connection.prepare("SELECT * FROM additive").is_err());
        assert_eq!(backup_files(temporary.path()).len(), 1);
    }

    #[test]
    fn additive_migrations_do_not_create_a_backup() {
        let temporary = tempfile::tempdir().expect("temporary data directory");
        let database = temporary.path().join("skilled.sqlite3");
        let mut connection = Connection::open(&database).expect("create database");
        let steps = [Migration {
            version: 1,
            destructive: false,
            sql: "CREATE TABLE additive (value TEXT);",
        }];

        assert_eq!(
            migrate_with(&mut connection, &database, &steps).expect("migrate additive schema"),
            None
        );
        assert!(backup_files(temporary.path()).is_empty());
    }

    #[test]
    fn a_backup_failure_applies_no_migration() {
        let temporary = tempfile::tempdir().expect("temporary data directory");
        let database = temporary.path().join("skilled.sqlite3");
        let mut connection = Connection::open(&database).expect("create database");
        let steps = [Migration {
            version: 1,
            destructive: true,
            sql: "CREATE TABLE replacement (value TEXT);",
        }];

        let missing_database = temporary.path().join("missing.sqlite3");
        assert!(migrate_with(&mut connection, &missing_database, &steps).is_err());
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .expect("read original version"),
            0
        );
        assert!(connection.prepare("SELECT * FROM replacement").is_err());
    }

    #[test]
    fn backup_names_are_exactly_one_normal_filename_component() {
        assert!(valid_backup_component("skilled.sqlite3.backup-v0-1-2-3"));
        for unsafe_name in ["", ".", "..", "../backup", "nested/backup", "/backup"] {
            assert!(!valid_backup_component(unsafe_name), "{unsafe_name:?}");
        }
    }

    #[test]
    fn an_occupied_backup_candidate_is_left_untouched_and_a_distinct_name_is_used() {
        let temporary = tempfile::tempdir().expect("temporary data directory");
        let database = temporary.path().join("skilled.sqlite3");
        let connection = Connection::open(&database).expect("create database");
        connection
            .execute_batch(
                "CREATE TABLE original (value TEXT); INSERT INTO original VALUES ('row');",
            )
            .expect("create database contents");
        let occupied = temporary.path().join("occupied.backup");
        let occupied_bytes = b"already here\n";
        fs::write(&occupied, occupied_bytes).expect("occupy backup candidate");
        let mut attempt = 0;

        let backup = backup_database_with(&connection, &database, 0, |_| {
            attempt += 1;
            if attempt == 1 {
                "occupied.backup".to_owned()
            } else {
                "fresh.backup".to_owned()
            }
        })
        .expect("create distinct backup");

        assert_eq!(backup, temporary.path().join("fresh.backup"));
        assert_eq!(
            fs::read(&occupied).expect("reread occupied file"),
            occupied_bytes
        );
        assert!(backup.exists());
    }

    fn backup_files(directory: &Path) -> Vec<PathBuf> {
        fs::read_dir(directory)
            .expect("read data directory")
            .map(|entry| entry.expect("read backup entry").path())
            .filter(|path| {
                path.file_name().is_some_and(|name| {
                    name.to_string_lossy()
                        .starts_with("skilled.sqlite3.backup-")
                })
            })
            .collect()
    }
}
