use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::{
    AgentKind, Error, Result,
    operations::{Receipt, ReceiptOperation},
    source::{
        CatalogClassification, CatalogProposal, Compatibility, InspectedSource, RegisteredSource,
        SourcePreview, contains_revision, inspect_local_source,
    },
    updates::{CachedUpdateCheck, RepositoryUpdateVerdict},
    validation::InspectionBudget,
};

const SCHEMA_VERSION: i64 = 9;
const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
const UPDATE_CHECK_UPSERT: &str = "INSERT INTO source_update_checks
        (source_id, checked_at, local_revision, local_reference, upstream_ref,
         upstream_revision, merge_base, ahead, behind, dirty, dirty_known, verdict, detail)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
     ON CONFLICT(source_id) DO UPDATE SET
        checked_at=excluded.checked_at, local_revision=excluded.local_revision,
        local_reference=excluded.local_reference, upstream_ref=excluded.upstream_ref,
        upstream_revision=excluded.upstream_revision,
        merge_base=excluded.merge_base, ahead=excluded.ahead, behind=excluded.behind,
        dirty=excluded.dirty, dirty_known=excluded.dirty_known,
        verdict=excluded.verdict, detail=excluded.detail
     WHERE excluded.checked_at >= source_update_checks.checked_at";
const UPDATE_CHECK_UPSERT_IF_NEWER: &str = "INSERT INTO source_update_checks
        (source_id, checked_at, local_revision, local_reference, upstream_ref,
         upstream_revision, merge_base, ahead, behind, dirty, dirty_known, verdict, detail)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
     ON CONFLICT(source_id) DO UPDATE SET
        checked_at=excluded.checked_at, local_revision=excluded.local_revision,
        local_reference=excluded.local_reference, upstream_ref=excluded.upstream_ref,
        upstream_revision=excluded.upstream_revision,
        merge_base=excluded.merge_base, ahead=excluded.ahead, behind=excluded.behind,
        dirty=excluded.dirty, dirty_known=excluded.dirty_known,
        verdict=excluded.verdict, detail=excluded.detail
     WHERE excluded.checked_at > source_update_checks.checked_at";

pub(crate) struct Store {
    connection: Connection,
}

impl Store {
    pub(crate) fn open(data_dir: &Path) -> Result<Self> {
        fs::create_dir_all(data_dir)?;
        let mut connection = Connection::open(data_dir.join("skilled.sqlite3"))?;
        connection.busy_timeout(BUSY_TIMEOUT)?;
        migrate(&mut connection)?;
        let _: String = connection.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
        connection.execute_batch("PRAGMA foreign_keys = ON;")?;
        Ok(Self { connection })
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
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
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
    pub(crate) fn record_receipt(&self, receipt: &Receipt) -> Result<()> {
        self.ensure_receipt_recordable(receipt)?;
        self.connection.execute(
            "INSERT INTO operation_receipts
                (created_at, operation, agent, skill_name, link_path, link_target,
                 source_id, catalog_relative_path, variant_relative_path)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT DO NOTHING",
            params![
                current_timestamp(),
                receipt.operation().identifier(),
                agent_identifier(receipt.agent()),
                receipt.skill_name(),
                stored_path(receipt.link_path())?,
                stored_path(receipt.link_target())?,
                receipt.source_id(),
                receipt
                    .catalog_relative_path()
                    .map(stored_path)
                    .transpose()?,
                receipt
                    .variant_relative_path()
                    .map(stored_path)
                    .transpose()?,
            ],
        )?;
        Ok(())
    }

    /// Every ownership receipt, oldest first.
    ///
    /// The last element is the most recent. `id` is the stable tiebreaker for
    /// receipts written in the same whole second, and migrations preserve it.
    ///
    /// A row naming an agent this build does not know is an error rather than a
    /// row to skip: a receipt Skilled cannot read is ownership it would go on to
    /// deny, and denying it would let the next plan treat its own link as a
    /// stranger's.
    pub(crate) fn receipts(&self) -> Result<Vec<Receipt>> {
        let mut statement = self.connection.prepare(
            "SELECT operation, agent, skill_name, link_path, link_target, source_id,
                    catalog_relative_path, variant_relative_path
             FROM operation_receipts ORDER BY created_at, id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<i64>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
            ))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .map(
                |(
                    operation,
                    agent,
                    skill_name,
                    link_path,
                    link_target,
                    source_id,
                    catalog_relative_path,
                    variant_relative_path,
                )| {
                    Ok(Receipt::new(
                        ReceiptOperation::from_identifier(&operation)?,
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
        let source = preview.inspected();
        let canonical_path = path_text(source.git_top_level())?;
        let label = source
            .git_top_level()
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| Error::InvalidSourcePath(source.git_top_level().to_path_buf()))?;
        let scanned_at = current_timestamp();
        let repository_identity = source
            .repository_identity()
            .map(|identity| identity.storage_key())
            .transpose()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO source_repositories
                (label, canonical_path, remote_url, branch, head_revision, dirty, dirty_known,
                 last_scan_at, repository_identity)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(canonical_path) DO UPDATE SET
                label = excluded.label,
                remote_url = excluded.remote_url,
                branch = excluded.branch,
                head_revision = excluded.head_revision,
                dirty = excluded.dirty,
                dirty_known = excluded.dirty_known,
                last_scan_at = excluded.last_scan_at,
                repository_identity = excluded.repository_identity",
            params![
                label,
                canonical_path,
                source.remote_url(),
                source.branch(),
                source.head(),
                source.dirty().unwrap_or(false),
                source.dirty().is_some(),
                scanned_at,
                repository_identity,
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

    pub(crate) fn update_checks(&self) -> Result<Vec<CachedUpdateCheck>> {
        let mut statement = self.connection.prepare(
            "SELECT source_id, checked_at, local_revision, local_reference, upstream_ref,
                    upstream_revision, merge_base, ahead, behind, dirty, dirty_known, verdict, detail
             FROM source_update_checks ORDER BY source_id",
        )?;
        let rows = statement.query_map([], |row| {
            let verdict: String = row.get(11)?;
            let verdict = RepositoryUpdateVerdict::parse(&verdict).ok_or_else(|| {
                rusqlite::Error::InvalidColumnType(
                    11,
                    "verdict".into(),
                    rusqlite::types::Type::Text,
                )
            })?;
            let ahead: i64 = row.get(7)?;
            let behind: i64 = row.get(8)?;
            Ok(CachedUpdateCheck {
                source_id: row.get(0)?,
                checked_at: row.get(1)?,
                local_revision: row.get(2)?,
                local_reference: row.get(3)?,
                upstream_ref: row.get(4)?,
                upstream_revision: row.get(5)?,
                merge_base: row.get(6)?,
                ahead: ahead as usize,
                behind: behind as usize,
                dirty: row.get(9)?,
                dirty_known: row.get(10)?,
                verdict,
                detail: row.get(12)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub(crate) fn record_update_check(&self, check: &CachedUpdateCheck) -> Result<()> {
        self.connection.execute(
            UPDATE_CHECK_UPSERT,
            params![
                check.source_id,
                check.checked_at,
                check.local_revision,
                check.local_reference,
                check.upstream_ref,
                check.upstream_revision,
                check.merge_base,
                check.ahead as i64,
                check.behind as i64,
                check.dirty,
                check.dirty_known,
                check.verdict.as_str(),
                check.detail,
            ],
        )?;
        Ok(())
    }

    pub(crate) fn record_update_checks(&mut self, checks: &[CachedUpdateCheck]) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for check in checks {
            transaction.execute(
                UPDATE_CHECK_UPSERT_IF_NEWER,
                params![
                    check.source_id,
                    check.checked_at,
                    check.local_revision,
                    check.local_reference,
                    check.upstream_ref,
                    check.upstream_revision,
                    check.merge_base,
                    check.ahead as i64,
                    check.behind as i64,
                    check.dirty,
                    check.dirty_known,
                    check.verdict.as_str(),
                    check.detail,
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn registered_sources(&self) -> Result<Vec<RegisteredSource>> {
        let mut statement = self.connection.prepare(
            "SELECT id, label, canonical_path, remote_url, branch, head_revision, dirty, dirty_known,
                    last_scan_at, repository_identity
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
                row.get::<_, Option<String>>(9)?,
            ))
        })?;
        let stored = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        drop(statement);

        let mut sources = Vec::with_capacity(stored.len());
        for (
            id,
            label,
            path,
            remote_url,
            branch,
            head,
            dirty,
            dirty_known,
            last_scan_at,
            stored_repository_identity,
        ) in stored
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
                Ok(current)
                    if stored_repository_identity.as_deref()
                        != current
                            .repository_identity()
                            .map(|identity| identity.storage_key())
                            .transpose()?
                            .as_deref()
                        && stored_repository_identity.is_some() =>
                {
                    (
                        stored_inspected.clone(),
                        Some("source path now contains a different Git checkout".to_owned()),
                        last_scan_at,
                    )
                }
                Ok(current) => match contains_revision(&git_top_level, stored_inspected.head()) {
                    Ok(true) => {
                        let refreshed_at = current_timestamp();
                        self.connection.execute(
                            "UPDATE source_repositories SET
                                remote_url = ?1,
                                branch = ?2,
                                head_revision = ?3,
                                dirty = ?4,
                                dirty_known = ?5,
                                last_scan_at = ?6,
                                repository_identity = COALESCE(repository_identity, ?7)
                             WHERE id = ?8",
                            params![
                                current.remote_url(),
                                current.branch(),
                                current.head(),
                                current.dirty().unwrap_or(false),
                                current.dirty().is_some(),
                                refreshed_at,
                                current
                                    .repository_identity()
                                    .map(|identity| identity.storage_key())
                                    .transpose()?,
                                id,
                            ],
                        )?;
                        (current, None, refreshed_at)
                    }
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
        let store = Store::open(temporary.path()).expect("open store");
        let link_path = PathBuf::from(OsString::from_vec(b"link-\xff".to_vec()));
        let receipt = Receipt::new(
            ReceiptOperation::Install,
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

    #[test]
    fn a_stale_batch_cannot_replace_a_newer_update_check() {
        let temporary = tempfile::tempdir().expect("temporary data directory");
        let mut store = Store::open(temporary.path()).expect("open store");
        store
            .connection
            .execute(
                "INSERT INTO source_repositories
                    (id, label, canonical_path, head_revision, dirty, dirty_known, last_scan_at)
                 VALUES (1, 'source', '/source', 'head', 0, 1, 0)",
                [],
            )
            .expect("source fixture");
        let check = |checked_at, detail: &str| CachedUpdateCheck {
            source_id: 1,
            checked_at,
            local_revision: "head".into(),
            local_reference: Some("refs/heads/main".into()),
            upstream_ref: None,
            upstream_revision: None,
            merge_base: None,
            ahead: 0,
            behind: 0,
            dirty: false,
            dirty_known: true,
            verdict: RepositoryUpdateVerdict::Blocked,
            detail: detail.into(),
        };
        store
            .record_update_check(&check(20, "newer"))
            .expect("newer check");
        store
            .record_update_checks(&[check(10, "older"), check(20, "same generation")])
            .expect("stale batch");

        let checks = store.update_checks().expect("stored checks");
        assert_eq!(checks[0].checked_at, 20);
        assert_eq!(checks[0].detail, "newer");
        store
            .record_update_check(&check(10, "older single"))
            .expect("stale single check");
        assert_eq!(
            store.update_checks().expect("checks after stale single")[0].detail,
            "newer"
        );
    }
}

fn migrate(connection: &mut Connection) -> Result<()> {
    // Version discovery and every migration share one write transaction. A
    // second process therefore reads the version only after any first opener
    // has finished upgrading, rather than acting on a value observed before
    // it waited for the migration lock.
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current_version: i64 =
        transaction.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if current_version > SCHEMA_VERSION {
        return Err(crate::Error::UnsupportedSchema {
            found: current_version,
            supported: SCHEMA_VERSION,
        });
    }

    if current_version < 1 {
        transaction.execute_batch(
            "CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY NOT NULL,
                value TEXT NOT NULL
             );
             PRAGMA user_version = 1;",
        )?;
    }
    if current_version < 2 {
        transaction.execute_batch(
            "CREATE TABLE configured_agents (
                agent TEXT PRIMARY KEY NOT NULL,
                selected INTEGER NOT NULL CHECK (selected IN (0, 1))
             );
             PRAGMA user_version = 2;",
        )?;
    }
    if current_version < 3 {
        transaction.execute_batch(
            "CREATE TABLE source_repositories (
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
             );
             PRAGMA user_version = 3;",
        )?;
    }
    if current_version < 4 {
        transaction.execute_batch(
            "ALTER TABLE source_repositories ADD COLUMN
                dirty_known INTEGER NOT NULL DEFAULT 1 CHECK (dirty_known IN (0, 1));
             PRAGMA user_version = 4;",
        )?;
    }
    if current_version < 5 {
        // `source_id` deliberately carries no foreign key: a receipt is spec 7
        // ownership evidence for a link Skilled put on disk, and forgetting the
        // source it came from must not erase the record of what is out there.
        // The columns beside it are the identity that evidence is *about*, kept
        // as text for the same reason — they have to remain readable after the
        // row they were copied from is gone.
        transaction.execute_batch(
            "CREATE TABLE operation_receipts (
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
                -- Receipts accumulate: a link removed and put back is two
                -- facts, not one. The constraint only rules out recording the
                -- identical fact twice, and the second of those is refused
                -- rather than failing the write it belongs to.
                UNIQUE (agent, link_path, link_target, created_at)
             );
             PRAGMA user_version = 5;",
        )?;
    }
    if current_version < 6 {
        // SQLite cannot alter either a CHECK or UNIQUE constraint in place.
        // Rebuilding inside this transaction preserves every existing receipt,
        // including its id: ordering by `(created_at, id)` is the public
        // oldest-to-newest contract used by ownership matching.
        transaction.execute_batch(
            "CREATE TABLE operation_receipts_v6 (
                id INTEGER PRIMARY KEY,
                created_at INTEGER NOT NULL,
                operation TEXT NOT NULL CHECK (operation IN ('install', 'repair')),
                agent TEXT NOT NULL,
                skill_name TEXT NOT NULL,
                link_path TEXT NOT NULL,
                link_target TEXT NOT NULL,
                source_id INTEGER,
                catalog_relative_path TEXT,
                variant_relative_path TEXT,
                UNIQUE (operation, agent, link_path, link_target, created_at)
             );
             INSERT INTO operation_receipts_v6
                (id, created_at, operation, agent, skill_name, link_path, link_target,
                 source_id, catalog_relative_path, variant_relative_path)
             SELECT id, created_at, operation, agent, skill_name, link_path, link_target,
                    source_id, catalog_relative_path, variant_relative_path
             FROM operation_receipts;
             DROP TABLE operation_receipts;
             ALTER TABLE operation_receipts_v6 RENAME TO operation_receipts;
             PRAGMA user_version = 6;",
        )?;
    }
    if current_version < 7 {
        transaction.execute_batch(
            "CREATE TABLE source_update_checks (
                source_id INTEGER PRIMARY KEY REFERENCES source_repositories(id) ON DELETE CASCADE,
                checked_at INTEGER NOT NULL,
                local_revision TEXT NOT NULL,
                upstream_ref TEXT,
                upstream_revision TEXT,
                merge_base TEXT,
                ahead INTEGER NOT NULL CHECK (ahead >= 0),
                behind INTEGER NOT NULL CHECK (behind >= 0),
                dirty INTEGER NOT NULL CHECK (dirty IN (0, 1)),
                dirty_known INTEGER NOT NULL CHECK (dirty_known IN (0, 1)),
                verdict TEXT NOT NULL CHECK (verdict IN ('up_to_date', 'ahead', 'available', 'blocked')),
                detail TEXT NOT NULL
             );
             PRAGMA user_version = 7;",
        )?;
    }
    if current_version < 8 {
        transaction.execute_batch(
            "ALTER TABLE source_update_checks ADD COLUMN local_reference TEXT;
             PRAGMA user_version = 8;",
        )?;
    }
    if current_version < 9 {
        transaction.execute_batch(
            "ALTER TABLE source_repositories ADD COLUMN repository_identity TEXT;
             PRAGMA user_version = 9;",
        )?;
    }
    transaction.commit()?;
    Ok(())
}
