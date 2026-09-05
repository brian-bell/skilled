use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
};

use crate::{
    AgentKind, Error, Result,
    operations::{Receipt, ReceiptOperation},
    resolution::VariantRef,
    source::{
        CatalogClassification, CatalogProposal, Compatibility, InspectedSource, RegisteredSource,
        SourcePreview, contains_revision, inspect_local_source,
    },
    updates::{CachedUpdateCheck, RepositoryUpdateVerdict},
    validation::InspectionBudget,
};

const SCHEMA_VERSION: i64 = 11;
const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
/// Record one check, unless the stored row was written under a later
/// generation.
///
/// Ordering is the `generation` column's job alone. `checked_at` states when
/// the check ran and is carried across unchanged by a record that reports on
/// an earlier check — a post-apply verification — so comparing it here would
/// decline exactly the writes that matter most: the verification would lose to
/// whatever a second Skilled process recorded while the preview was open, and
/// the store would report that it had been saved.
const UPDATE_CHECK_UPSERT: &str = "INSERT INTO source_update_checks
        (source_id, checked_at, generation, local_revision, local_reference, upstream_ref,
         upstream_revision, merge_base, ahead, behind, dirty, dirty_known, verdict, detail)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
     ON CONFLICT(source_id) DO UPDATE SET
        checked_at=excluded.checked_at, generation=excluded.generation,
        local_revision=excluded.local_revision,
        local_reference=excluded.local_reference, upstream_ref=excluded.upstream_ref,
        upstream_revision=excluded.upstream_revision,
        merge_base=excluded.merge_base, ahead=excluded.ahead, behind=excluded.behind,
        dirty=excluded.dirty, dirty_known=excluded.dirty_known,
        verdict=excluded.verdict, detail=excluded.detail
     WHERE excluded.generation >= source_update_checks.generation";
/// The same write for a batch, where a repeated generation is a stale worker's
/// result rather than a restatement of the stored row.
const UPDATE_CHECK_UPSERT_IF_NEWER: &str = "INSERT INTO source_update_checks
        (source_id, checked_at, generation, local_revision, local_reference, upstream_ref,
         upstream_revision, merge_base, ahead, behind, dirty, dirty_known, verdict, detail)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
     ON CONFLICT(source_id) DO UPDATE SET
        checked_at=excluded.checked_at, generation=excluded.generation,
        local_revision=excluded.local_revision,
        local_reference=excluded.local_reference, upstream_ref=excluded.upstream_ref,
        upstream_revision=excluded.upstream_revision,
        merge_base=excluded.merge_base, ahead=excluded.ahead, behind=excluded.behind,
        dirty=excluded.dirty, dirty_known=excluded.dirty_known,
        verdict=excluded.verdict, detail=excluded.detail
     WHERE excluded.generation > source_update_checks.generation";

pub(crate) struct Store {
    connection: Connection,
    database_path: PathBuf,
    read_only: bool,
    #[cfg(test)]
    fail_next: std::cell::RefCell<Option<MetadataOperation>>,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MetadataOperation {
    CompleteSetup,
    ResetSetup,
    RegisterSource,
    /// A request the store refuses before writing anything — a checkout path
    /// it cannot represent — as opposed to a failure of the store itself.
    RefuseSourceRequest,
    ReadSources,
    ReadReceipts,
    RecordReceipt,
}

/// One cross-process metadata mutation guard.
///
/// `BEGIN IMMEDIATE` reserves SQLite's single-writer slot before the caller
/// touches the filesystem. Every install target, uninstall finalization, and
/// Forget Source apply uses this guard, so a process cannot make a link active
/// while another process is deciding whether its ownership metadata is safe to
/// delete. Dropping the guard rolls the transaction back.
pub(crate) struct Mutation<'store> {
    transaction: Transaction<'store>,
    /// The injected failure the guard's own writes honour.
    ///
    /// Borrowed from the store rather than copied out of it so a test can
    /// arm the failure before the guard exists, which is where the interesting
    /// window is: a receipt that fails *after* its link is on disk.
    #[cfg(test)]
    fail_next: &'store std::cell::RefCell<Option<MetadataOperation>>,
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
            // `new_data_dir`, and not merely a missing database: a database
            // gone from an application-data directory Skilled did not just
            // create is metadata that was there and is not now, and starting a
            // fresh one over it would silently discard whatever the user still
            // believes Skilled knows. Degrading names the path instead. The
            // cost is that a first launch killed between `create_dir_all` and
            // SQLite's own create leaves an empty directory that degrades
            // until it is removed; that is the side of the trade to be on.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && new_data_dir => {
                flags |= OpenFlags::SQLITE_OPEN_CREATE;
            }
            Err(error) => return Err(error.into()),
        }
        let mut connection = Connection::open_with_flags(&sqlite_database_path, flags)?;
        connection.busy_timeout(BUSY_TIMEOUT)?;
        // `SQLITE_OPEN_READ_WRITE` is a request, not a guarantee: SQLite opens
        // a write-protected file read-only and reports success. A database
        // already at the current schema leaves migration nothing to write and
        // every startup read then succeeds, so without this the session would
        // reach `Ready` and offer installation over a store that can never
        // record a receipt — the link would be created before the write that
        // cannot happen was ever attempted. Asked at open, it is a metadata
        // failure like any other, and the session degrades to read-only.
        //
        // This answers for the database file, not for the journal and WAL
        // sidecars SQLite creates beside it: a writable file in a directory
        // that denies creation still opens read-write and fails at the first
        // transaction. Proving that needs a real write, which is the one thing
        // a read-only startup must not do, so it stays unproven here and the
        // failure surfaces where it happens — `StepOutcome::CreatedUnrecorded`
        // states a link whose receipt could not be written rather than hiding
        // it. Tracked as `skilled-2k3.22`.
        //
        // Recorded rather than raised. A store that cannot be written can
        // still be read, and every value it holds — the agent selection, the
        // registered sources — is one the read-only session is better off
        // knowing. `app::open_metadata` reads them and then takes this as one
        // more reason the session is degraded, alongside the values it found
        // invalid, so nothing readable is discarded to refuse a write.
        let read_only = connection.is_readonly(rusqlite::MAIN_DB)?;
        // Schema before semantics, deliberately. `app::open_metadata` is what
        // reads stored values and can declare the store unavailable, and it
        // reads them through the current schema — so a supported older
        // database holding an invalid value is migrated first and refused
        // afterwards. Nothing is lost by that ordering: an additive migration
        // adds schema and no value it carried stops being carried, and a
        // destructive one has already taken its backup. Validating first would
        // mean a semantic validator per historical schema version, and
        // undoing a migration that succeeded because an unrelated field is
        // malformed is the worse of the two. Recorded rather than reopened;
        // the decision itself is `skilled-2k3.23`.
        migrate(&mut connection, &sqlite_database_path)?;
        // Write-ahead logging is a write. A store that opened read-only can
        // still be read, and asking it to change its journal mode would turn
        // the degraded session this open deliberately allows back into a
        // failure to open at all.
        if !read_only {
            let _: String =
                connection.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
        }
        connection.execute_batch("PRAGMA foreign_keys = ON;")?;
        Ok(Self {
            connection,
            database_path,
            read_only,
            #[cfg(test)]
            fail_next: std::cell::RefCell::new(None),
        })
    }

    pub(crate) fn database_path(&self) -> &Path {
        &self.database_path
    }

    /// Whether SQLite opened the database read-only despite being asked for
    /// write access, which no write on this connection can recover from.
    pub(crate) fn read_only(&self) -> bool {
        self.read_only
    }

    pub(crate) fn begin_mutation(&mut self) -> Result<Mutation<'_>> {
        #[cfg(test)]
        let Self {
            connection,
            fail_next,
            ..
        } = self;
        #[cfg(not(test))]
        let Self { connection, .. } = self;
        Ok(Mutation {
            transaction: connection.transaction_with_behavior(TransactionBehavior::Immediate)?,
            #[cfg(test)]
            fail_next,
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
        match value.as_deref() {
            None | Some("false") => Ok(false),
            Some("true") => Ok(true),
            Some(value) => Err(Error::InvalidSetupMetadata(format!(
                "setup_complete holds {value:?} rather than true or false"
            ))),
        }
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
        let mut statement = self
            .connection
            .prepare("SELECT agent, selected FROM configured_agents ORDER BY agent")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut selections = [None; 3];
        let mut count = 0;
        for row in rows {
            let (identifier, selected) = row?;
            let agent = agent_kind(&identifier)?;
            selections[agent.index()] =
                Some(stored_boolean("configured_agents.selected", selected)?);
            count += 1;
        }
        if count == 0 {
            return Ok(None);
        }
        if count != AGENT_IDENTIFIERS.len() || selections.iter().any(Option::is_none) {
            return Err(Error::InvalidSetupMetadata(format!(
                "configured_agents contains {count} of {} required agents",
                AGENT_IDENTIFIERS.len()
            )));
        }
        Ok(Some(selections.map(|selected| {
            selected.expect("every supported agent was checked above")
        })))
    }

    pub(crate) fn complete_setup(&mut self, selections: [bool; 3]) -> Result<()> {
        #[cfg(test)]
        self.fail_if(MetadataOperation::CompleteSetup)?;
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
    /// The install guard calls this before acquiring its mutation transaction,
    /// then [`Mutation::record_receipt`] repeats it so no caller can bypass the
    /// table's contract and turn a path conversion into a post-write surprise.
    pub(crate) fn ensure_receipt_recordable(&self, receipt: &Receipt) -> Result<()> {
        ensure_receipt_recordable(receipt)
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
        #[cfg(test)]
        self.fail_if(MetadataOperation::ReadReceipts)?;
        receipts_on(&self.connection)
    }

    /// Required postconditions for forgetting: source, catalogs, and receipts absent.
    pub(crate) fn verify_source_forgotten(&self, source_id: i64) -> Result<[bool; 3]> {
        let source: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM source_repositories WHERE id = ?1",
            params![source_id],
            |row| row.get(0),
        )?;
        let catalogs: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM catalog_roots WHERE source_id = ?1",
            params![source_id],
            |row| row.get(0),
        )?;
        let receipts: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM operation_receipts WHERE source_id = ?1",
            params![source_id],
            |row| row.get(0),
        )?;
        Ok([source == 0, catalogs == 0, receipts == 0])
    }

    pub(crate) fn register_source(&mut self, preview: &SourcePreview) -> Result<()> {
        #[cfg(test)]
        self.fail_if(MetadataOperation::RegisterSource)?;
        #[cfg(test)]
        self.fail_if(MetadataOperation::RefuseSourceRequest)?;
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
        let existing_id = transaction
            .query_row(
                "SELECT id FROM source_repositories WHERE canonical_path = ?1",
                params![canonical_path],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        let source_id = match existing_id {
            Some(id) => id,
            None => {
                let id = transaction.query_row(
                    "SELECT next_id FROM source_id_sequence WHERE singleton = 1",
                    [],
                    |row| row.get::<_, i64>(0),
                )?;
                transaction.execute(
                    "UPDATE source_id_sequence SET next_id = next_id + 1
                     WHERE singleton = 1",
                    [],
                )?;
                id
            }
        };
        transaction.execute(
            "INSERT INTO source_repositories
                (id, label, canonical_path, remote_url, branch, head_revision, dirty, dirty_known,
                 last_scan_at, repository_identity)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
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
                source_id,
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
                    classification_text(catalog.classification()),
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
        Self::update_checks_on(&self.connection)
    }

    fn update_checks_on(connection: &Connection) -> Result<Vec<CachedUpdateCheck>> {
        let mut statement = connection.prepare(
            "SELECT source_id, checked_at, generation, local_revision, local_reference,
                    upstream_ref, upstream_revision, merge_base, ahead, behind, dirty,
                    dirty_known, verdict, detail
             FROM source_update_checks ORDER BY source_id",
        )?;
        let rows = statement.query_map([], |row| {
            let verdict: String = row.get(12)?;
            let verdict = RepositoryUpdateVerdict::parse(&verdict).ok_or_else(|| {
                rusqlite::Error::InvalidColumnType(
                    12,
                    "verdict".into(),
                    rusqlite::types::Type::Text,
                )
            })?;
            let ahead: i64 = row.get(8)?;
            let behind: i64 = row.get(9)?;
            Ok(CachedUpdateCheck {
                source_id: row.get(0)?,
                checked_at: row.get(1)?,
                generation: row.get(2)?,
                local_revision: row.get(3)?,
                local_reference: row.get(4)?,
                upstream_ref: row.get(5)?,
                upstream_revision: row.get(6)?,
                merge_base: row.get(7)?,
                ahead: ahead as usize,
                behind: behind as usize,
                dirty: row.get(10)?,
                dirty_known: row.get(11)?,
                verdict,
                detail: row.get(13)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Hand out `count` update-check generations no other allocation will
    /// reuse, and return the first of them.
    ///
    /// The generation orders a check's write, and only that — when the check
    /// ran is `checked_at`, which nothing compares. The conditional upsert
    /// behind a recorded check keeps an older result from displacing a newer
    /// one, so two records that share a generation make one of them disappear
    /// into a store that reported success. A counter held in each process cannot
    /// prevent that — two Skilled processes running checks after the clock has
    /// moved behind the last stored value will each hand out the same number —
    /// so the high-water mark lives beside the data it orders and is advanced
    /// under the same immediate transaction that reads it. `floor` is what the
    /// wall clock offers; the reservation takes it only when it is genuinely
    /// ahead of everything already handed out.
    pub(crate) fn reserve_update_check_generations(
        &mut self,
        floor: i64,
        count: usize,
    ) -> Result<i64> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let reserved: Option<i64> = transaction
            .query_row(
                "SELECT value FROM settings WHERE key = 'update_check_generation'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .and_then(|value| value.parse().ok());
        // The rows themselves are the other high-water mark, and on a database
        // written before this setting existed they are the only one. Reading
        // the setting alone would hand a store that already holds later checks
        // an earlier generation, and the conditional upsert would then decline
        // every new check while reporting that it succeeded.
        let recorded: Option<i64> = transaction.query_row(
            "SELECT MAX(generation) FROM source_update_checks",
            [],
            |row| row.get(0),
        )?;
        let held = reserved.unwrap_or(0).max(recorded.unwrap_or(0));
        let first = floor.max(held.saturating_add(1));
        let last = first.saturating_add(i64::try_from(count.max(1)).unwrap_or(i64::MAX) - 1);
        transaction.execute(
            "INSERT INTO settings (key, value) VALUES ('update_check_generation', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![last.to_string()],
        )?;
        transaction.commit()?;
        Ok(first)
    }

    pub(crate) fn record_update_check(&self, check: &CachedUpdateCheck) -> Result<()> {
        Self::record_update_check_on(&self.connection, check)
    }

    /// A successful verification has two ordering generations: its upstream reading belongs
    /// to the plan, but its proof that the write succeeded is new. Clear an
    /// earlier failed-write observation using the latter, while retaining the
    /// former on the replacement so a concurrent explicit check can still win
    /// even if it has reserved its generation but not yet saved its result.
    /// The immediate transaction prevents another writer from changing the
    /// inspected row between that decision and the conditional upsert.
    pub(crate) fn record_verified_update_check(
        &mut self,
        check: &CachedUpdateCheck,
        completion_generation: i64,
    ) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let earlier_failure = Self::update_checks_on(&transaction)?
            .into_iter()
            .any(|stored| {
                stored.source_id == check.source_id
                    && stored.generation < completion_generation
                    && stored.findings().iter().any(|finding| {
                        matches!(
                            finding.code(),
                            "update.apply_failed"
                                | "update.verification_failed"
                                | "update.verification_incomplete"
                        )
                    })
            });
        if earlier_failure {
            transaction.execute(
                "DELETE FROM source_update_checks WHERE source_id = ?1",
                [check.source_id],
            )?;
        }
        Self::record_update_check_on(&transaction, check)?;
        transaction.commit()?;
        Ok(())
    }

    fn record_update_check_on(connection: &Connection, check: &CachedUpdateCheck) -> Result<()> {
        connection.execute(
            UPDATE_CHECK_UPSERT,
            params![
                check.source_id,
                check.checked_at,
                check.generation,
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
                    check.generation,
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
        self.load_registered_sources(true)
    }

    pub(crate) fn load_registered_sources(&self, refresh: bool) -> Result<Vec<RegisteredSource>> {
        #[cfg(test)]
        self.fail_if(MetadataOperation::ReadSources)?;
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
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
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
            let dirty = stored_boolean("source_repositories.dirty", dirty)?;
            let dirty_known = stored_boolean("source_repositories.dirty_known", dirty_known)?;
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
                // Head containment stays the gate for every row, the
                // identity-less ones included. It is the only sameness
                // evidence a pre-schema-9 row has: waving such a row through
                // without it would let a wholly different repository standing
                // at the path load without an error and be installed from,
                // and a row that recorded its identity is treated exactly the
                // same way when the stored head vanishes.
                Ok(current) => match contains_revision(&git_top_level, stored_inspected.head()) {
                    Ok(true) if refresh => {
                        let refreshed_at = current_timestamp();
                        // The stored identity is never written here. A row
                        // registered after schema 9 recorded it at
                        // registration and the mismatch check above already
                        // held the standing checkout to it; a row from before
                        // schema 9 stored none, and adopting whatever stands
                        // at the path would let a replacement clone that
                        // merely contains the stored head become the
                        // registered repository for every later update.
                        // Re-registration is the only way an identity is
                        // recorded (skilled-t0f).
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
            // A row from before schema 9 proves no identity, and the one the
            // live inspection just read describes whatever is standing at the
            // path — possibly a replacement clone that contains the stored
            // head. Handing it out as the source's identity would let the
            // update pipeline treat the standing checkout as the registered
            // one, so the source carries none and updates refuse until the
            // user re-registers the checkout (skilled-t0f).
            let inspected = if stored_repository_identity.is_none() {
                inspected.without_repository_identity()
            } else {
                inspected
            };
            let mut catalog_statement = self.connection.prepare(
                "SELECT relative_path, classification, claude_code, codex, opencode
                 FROM catalog_roots WHERE source_id = ?1 ORDER BY relative_path",
            )?;
            let catalog_rows = catalog_statement.query_map(params![id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })?;
            let mut catalogs = Vec::new();
            let mut budget = InspectionBudget::source_scan();
            for row in catalog_rows {
                let (relative_path, classification, claude_code, codex, opencode) = row?;
                let claude_code = stored_boolean("catalog_roots.claude_code", claude_code)?;
                let codex = stored_boolean("catalog_roots.codex", codex)?;
                let opencode = stored_boolean("catalog_roots.opencode", opencode)?;
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
        Err(match operation {
            MetadataOperation::RefuseSourceRequest => {
                Error::InvalidSourcePath(self.database_path.clone())
            }
            _ => Error::Database(rusqlite::Error::InvalidQuery),
        })
    }
}

fn unsafe_metadata_leaf(path: &Path, message: &str) -> Error {
    Error::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!("{message}: {}", path.display()),
    ))
}

impl Mutation<'_> {
    /// Record ownership in the transaction that already covers link creation.
    ///
    /// A receipt identical to one already recorded is left alone rather than
    /// refused. Timestamps are whole seconds, so a link removed and put back
    /// inside one second would otherwise fail its insert and leave Skilled
    /// reporting that it does not own something it just created — when the
    /// receipt it needs is already there.
    pub(crate) fn record_receipt(&self, receipt: &Receipt) -> Result<()> {
        #[cfg(test)]
        self.fail_if(MetadataOperation::RecordReceipt)?;
        record_receipt_on(&self.transaction, receipt)
    }

    #[cfg(test)]
    fn fail_if(&self, operation: MetadataOperation) -> Result<()> {
        if self.fail_next.borrow().as_ref() != Some(&operation) {
            return Ok(());
        }
        self.fail_next.borrow_mut().take();
        Err(Error::Database(rusqlite::Error::InvalidQuery))
    }

    pub(crate) fn receipts(&self) -> Result<Vec<Receipt>> {
        receipts_on(&self.transaction)
    }

    /// Recheck that a stale install plan still names registered metadata while
    /// holding the same guard that will cover its link and receipt writes.
    pub(crate) fn source_is_registered(&self, source_id: i64) -> Result<bool> {
        self.transaction
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM source_repositories WHERE id = ?1
                 )",
                params![source_id],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    /// Recheck the exact registration a confirmed install or repair plan named.
    ///
    /// [`Self::source_is_registered`] answers only that the identifier survived,
    /// and identifiers survive a re-registration of the same checkout: between
    /// the preview and this guard, a catalog can be excluded, reclassified, or
    /// have an agent removed from its compatibility declaration while the source
    /// row keeps its id. The transaction cannot invalidate a change committed
    /// before it began, so the plan is compared with what is registered now.
    ///
    /// Every field the ownership receipt is about to record is compared: the
    /// source it names, the checkout that source stands at, and the catalog root
    /// beneath it with the classification and compatibility the plan selected
    /// under. The variant directory itself is not registration — candidates are
    /// scan results rather than metadata — and the caller revalidates it against
    /// the filesystem immediately before this.
    ///
    /// This compares the row the plan chose, not the rows it chose from. A
    /// source another process registers after the preview can offer a competing
    /// variant of the same name, which these queries cannot see;
    /// [`Self::registry_fingerprint`] is what covers that, and both are asked
    /// under this guard.
    pub(crate) fn variant_registration_matches(
        &self,
        variant: &VariantRef,
        checkout: &Path,
    ) -> Result<bool> {
        let source = self
            .transaction
            .query_row(
                "SELECT label, canonical_path FROM source_repositories WHERE id = ?1",
                params![variant.source_id()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        if source != Some((variant.source_label().to_owned(), path_text(checkout)?)) {
            return Ok(false);
        }
        let catalog = self
            .transaction
            .query_row(
                "SELECT classification, claude_code, codex, opencode
                 FROM catalog_roots WHERE source_id = ?1 AND relative_path = ?2",
                params![
                    variant.source_id(),
                    path_text(variant.catalog_relative_path())?
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, bool>(1)?,
                        row.get::<_, bool>(2)?,
                        row.get::<_, bool>(3)?,
                    ))
                },
            )
            .optional()?;
        let compatibility = variant.compatibility();
        Ok(catalog
            == Some((
                classification_text(variant.classification()).to_owned(),
                compatibility.claude_code(),
                compatibility.codex(),
                compatibility.opencode(),
            )))
    }

    /// The registry the guard is holding still, as one comparable value.
    ///
    /// See [`RegistryFingerprint`] for what it covers and why it is a digest.
    pub(crate) fn registry_fingerprint(&self) -> Result<RegistryFingerprint> {
        registry_fingerprint_on(&self.transaction)
    }

    /// Recheck the complete stored registration represented by a Forget plan.
    /// Candidate scans are not metadata and are intentionally excluded; every
    /// source-row and catalog-root field the store persists is compared while
    /// the mutation guard prevents a concurrent registration update.
    pub(crate) fn source_matches(&self, expected: &RegisteredSource) -> Result<bool> {
        let current = self
            .transaction
            .query_row(
                "SELECT label, canonical_path, remote_url, branch, head_revision,
                        dirty, dirty_known, last_scan_at
                 FROM source_repositories WHERE id = ?1",
                params![expected.id()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, bool>(5)?,
                        row.get::<_, bool>(6)?,
                        row.get::<_, i64>(7)?,
                    ))
                },
            )
            .optional()?;
        let expected_dirty = expected.dirty();
        let expected_source = (
            expected.label().to_owned(),
            path_text(expected.git_top_level())?,
            expected.remote_url().map(str::to_owned),
            expected.branch().map(str::to_owned),
            expected.head().to_owned(),
            expected_dirty.unwrap_or(false),
            expected_dirty.is_some(),
            expected.last_scan_at(),
        );
        if current.as_ref() != Some(&expected_source) {
            return Ok(false);
        }

        let mut statement = self.transaction.prepare(
            "SELECT relative_path, classification, claude_code, codex, opencode
             FROM catalog_roots WHERE source_id = ?1 ORDER BY relative_path",
        )?;
        let rows = statement.query_map(params![expected.id()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, bool>(2)?,
                row.get::<_, bool>(3)?,
                row.get::<_, bool>(4)?,
            ))
        })?;
        let current_catalogs = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        let mut expected_catalogs = expected
            .catalogs()
            .iter()
            .map(|catalog| {
                Ok((
                    path_text(catalog.relative_path())?,
                    classification_text(catalog.classification()).to_owned(),
                    catalog.compatibility().claude_code(),
                    catalog.compatibility().codex(),
                    catalog.compatibility().opencode(),
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        expected_catalogs.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(current_catalogs == expected_catalogs)
    }

    /// Delete exactly the ownership facts whose link was positively verified
    /// gone while this guard prevents a concurrent receipt writer from
    /// committing a replacement fact before the deletion.
    pub(crate) fn delete_receipts_for_link(
        &self,
        agent: AgentKind,
        link_path: &Path,
        link_target: &Path,
    ) -> Result<usize> {
        Ok(self.transaction.execute(
            "DELETE FROM operation_receipts
             WHERE agent = ?1 AND link_path = ?2 AND link_target = ?3",
            params![
                agent_identifier(agent),
                stored_path(link_path)?,
                stored_path(link_target)?,
            ],
        )?)
    }

    /// Remove one source's private metadata inside the guard that already
    /// covered the caller's exact receipt-set check and filesystem reprobe.
    pub(crate) fn forget_source(&self, source_id: i64) -> Result<usize> {
        self.transaction.execute(
            "DELETE FROM operation_receipts WHERE source_id = ?1",
            params![source_id],
        )?;
        Ok(self.transaction.execute(
            "DELETE FROM source_repositories WHERE id = ?1",
            params![source_id],
        )?)
    }

    pub(crate) fn commit(self) -> Result<()> {
        self.transaction.commit().map_err(Into::into)
    }
}

fn ensure_receipt_recordable(receipt: &Receipt) -> Result<()> {
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

fn record_receipt_on(connection: &Connection, receipt: &Receipt) -> Result<()> {
    ensure_receipt_recordable(receipt)?;
    connection.execute(
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

fn receipts_on(connection: &Connection) -> Result<Vec<Receipt>> {
    let mut statement = connection.prepare(
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

fn path_text(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| Error::InvalidSourcePath(path.to_path_buf()))
}

/// One value standing for every registration fact a spec 6.4 selection is
/// decided over.
///
/// A plan records the fingerprint of the registry it was planned against, and
/// the mutation guard compares it with the registry as the store holds it at
/// the moment of writing. [`Mutation::variant_registration_matches`] compares
/// the row the plan *chose*; this compares the rows it chose *from*, which is
/// what a competing variant of the same name arrives as. Without it a source or
/// catalog another process registered after the preview could leave a confirmed
/// plan writing a link a fresh selection would report as a duplicate conflict,
/// or resolve to a different variant altogether (`skilled-g64`).
///
/// It is a digest of the registration rows rather than a counter writers bump.
/// A counter would have to be advanced by every future write path, and one that
/// forgot would fail open — the guard would pass while the registry had moved
/// under it. A digest cannot be forgotten: it is derived from the same columns
/// the selection reads, on both sides. It also does not refuse for changes that
/// change nothing, so a re-registration that rewrites a row identically, and an
/// in-memory registry a caller narrowed to match a commit that already
/// happened, both still apply.
///
/// Scope is registration metadata only, and deliberately so. The scanned skill
/// candidates a selection also narrows over are filesystem observations rather
/// than stored rows: a skill directory added under an already-registered
/// catalog after the preview changes the same selection without changing a
/// single row here. The apply guard revalidates the chosen variant's own
/// directory against the filesystem immediately before writing, but it does not
/// re-scan the registry's other catalogs, and this fingerprint does not stand
/// for what such a scan would find. Closing that would mean walking every
/// registered catalog while the metadata mutation guard is held — unbounded
/// filesystem work inside a transaction that exists to freeze metadata, which
/// is the line `apply_repair_target` already draws — so it belongs to a
/// separate decision rather than to this one.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct RegistryFingerprint(u64);

impl RegistryFingerprint {
    /// The fingerprint of the registry as a planner holds it in memory.
    ///
    /// Every field is the one the row it was loaded from carries, so this and
    /// [`registry_fingerprint_on`] agree exactly while nothing has changed. The
    /// volatile columns a scan refresh rewrites — head, dirtiness, last scan —
    /// are deliberately absent: they move without changing what any agent
    /// resolves, and including them would refuse ordinary installs.
    pub(crate) fn of_registry(sources: &[RegisteredSource]) -> Self {
        Self::of_records(sources.iter().flat_map(|source| {
            std::iter::once(source_fingerprint_record(
                source.id(),
                source.label(),
                &source.git_top_level().to_string_lossy(),
            ))
            .chain(source.catalogs().iter().map(|catalog| {
                let compatibility = catalog.compatibility();
                catalog_fingerprint_record(
                    source.id(),
                    &catalog.relative_path().to_string_lossy(),
                    classification_text(catalog.classification()),
                    compatibility.claude_code(),
                    compatibility.codex(),
                    compatibility.opencode(),
                )
            }))
        }))
    }

    /// Order is not registry state, so the records are sorted before hashing:
    /// two readings of the same rows fingerprint alike however each arrived.
    fn of_records(records: impl Iterator<Item = RegistryRecord>) -> Self {
        use std::hash::{Hash, Hasher};

        let mut records: Vec<RegistryRecord> = records.collect();
        records.sort_unstable();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        records.hash(&mut hasher);
        Self(hasher.finish())
    }
}

/// Derived hashing preserves field boundaries even when a label or path
/// contains control characters; concatenating with a separator would let two
/// different registrations spell the same record before hashing.
#[derive(Eq, Hash, Ord, PartialEq, PartialOrd)]
enum RegistryRecord {
    Source(i64, String, String),
    Catalog(i64, String, String, bool, bool, bool),
}

fn source_fingerprint_record(id: i64, label: &str, canonical_path: &str) -> RegistryRecord {
    RegistryRecord::Source(id, label.to_owned(), canonical_path.to_owned())
}

fn catalog_fingerprint_record(
    source_id: i64,
    relative_path: &str,
    classification: &str,
    claude_code: bool,
    codex: bool,
    opencode: bool,
) -> RegistryRecord {
    RegistryRecord::Catalog(
        source_id,
        relative_path.to_owned(),
        classification.to_owned(),
        claude_code,
        codex,
        opencode,
    )
}

/// The fingerprint of the registry as the database holds it right now.
///
/// Takes a connection so the store and the mutation guard read it the same way:
/// inside the guard's transaction it is the registry no other writer can change
/// before the receipt commits.
fn registry_fingerprint_on(connection: &Connection) -> Result<RegistryFingerprint> {
    let mut source_statement =
        connection.prepare("SELECT id, label, canonical_path FROM source_repositories")?;
    let sources = source_statement
        .query_map([], |row| {
            Ok(source_fingerprint_record(
                row.get::<_, i64>(0)?,
                &row.get::<_, String>(1)?,
                &row.get::<_, String>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut catalog_statement = connection.prepare(
        "SELECT source_id, relative_path, classification, claude_code, codex, opencode
         FROM catalog_roots",
    )?;
    let catalogs = catalog_statement
        .query_map([], |row| {
            Ok(catalog_fingerprint_record(
                row.get::<_, i64>(0)?,
                &row.get::<_, String>(1)?,
                &row.get::<_, String>(2)?,
                row.get::<_, i64>(3)? != 0,
                row.get::<_, i64>(4)? != 0,
                row.get::<_, i64>(5)? != 0,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(RegistryFingerprint::of_records(
        sources.into_iter().chain(catalogs),
    ))
}

/// The one spelling of a classification the `catalog_roots` CHECK constraint
/// accepts, shared by everything that writes or compares that column.
fn classification_text(classification: CatalogClassification) -> &'static str {
    match classification {
        CatalogClassification::Common => "common",
        CatalogClassification::AgentSpecific => "agent-specific",
    }
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

fn stored_boolean(field: &'static str, value: i64) -> Result<bool> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        value => Err(Error::InvalidStoredBoolean { field, value }),
    }
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
    fn registry_fingerprints_distinguish_separators_inside_source_fields() {
        let first = source_fingerprint_record(1, "library", "/one\u{1f}/two");
        let second = source_fingerprint_record(1, "library\u{1f}/one", "/two");
        assert_ne!(
            RegistryFingerprint::of_records(std::iter::once(first)),
            RegistryFingerprint::of_records(std::iter::once(second)),
            "moving a separator between a label and a path must change the registry"
        );
    }

    #[test]
    fn an_ownership_receipt_requires_representable_paths_before_it_can_be_written() {
        let temporary = tempfile::tempdir().expect("temporary data directory");
        let store = Store::open(&temporary.path().join("data")).expect("open store");
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

    /// Two Skilled processes checking the same registry must not be handed the
    /// same generation, or the conditional upsert drops whichever finishes
    /// second while telling it the write succeeded. Two stores over one data
    /// directory are the two processes: the reservation is the only shared
    /// state ordering them, and a clock that has moved backwards — the floor
    /// here — must not be able to hand out a value twice.
    #[test]
    fn reserved_generations_are_never_handed_out_twice() {
        let temporary = tempfile::tempdir().expect("temporary data directory");
        let data_dir = temporary.path().join("data");
        let mut first = Store::open(&data_dir).expect("open store");
        let mut second = Store::open(&data_dir).expect("open second store");

        let a = first
            .reserve_update_check_generations(1_000, 3)
            .expect("first reservation");
        let b = second
            .reserve_update_check_generations(1_000, 2)
            .expect("second reservation");
        let c = first
            .reserve_update_check_generations(500, 1)
            .expect("reservation behind the clock");

        assert_eq!(a, 1_000);
        assert!(b > a + 2, "{b} overlaps the block starting at {a}");
        assert!(c > b + 1, "{c} overlaps the block starting at {b}");
    }

    /// A database written before the reservation setting existed still holds
    /// the generations its checks were recorded under, and they are the only
    /// high-water mark there is. Reading the setting alone would hand the
    /// first reservation after the upgrade an earlier value, and the
    /// conditional upsert would then decline every new check while reporting
    /// that it had been stored.
    #[test]
    fn a_reservation_never_falls_behind_the_checks_already_recorded() {
        let temporary = tempfile::tempdir().expect("temporary data directory");
        let mut store = Store::open(&temporary.path().join("data")).expect("open store");
        store
            .connection
            .execute(
                "INSERT INTO source_repositories
                    (id, label, canonical_path, head_revision, dirty, dirty_known, last_scan_at)
                 VALUES (1, 'source', '/source', 'head', 0, 1, 0)",
                [],
            )
            .expect("source fixture");
        store
            .connection
            .execute(
                "INSERT INTO source_update_checks
                    (source_id, checked_at, generation, local_revision, ahead, behind, dirty,
                     dirty_known, verdict, detail)
                 VALUES (1, 9_000, 9_000, 'head', 0, 0, 0, 1, 'up_to_date', '')",
                [],
            )
            .expect("check recorded before the setting existed");

        // The clock is well behind what the stored check was written under.
        let reserved = store
            .reserve_update_check_generations(100, 1)
            .expect("reservation");

        assert!(reserved > 9_000, "{reserved} is not past the recorded 9000");
    }

    #[test]
    fn a_stale_batch_cannot_replace_a_newer_update_check() {
        let temporary = tempfile::tempdir().expect("temporary data directory");
        let mut store = Store::open(&temporary.path().join("data")).expect("open store");
        store
            .connection
            .execute(
                "INSERT INTO source_repositories
                    (id, label, canonical_path, head_revision, dirty, dirty_known, last_scan_at)
                 VALUES (1, 'source', '/source', 'head', 0, 1, 0)",
                [],
            )
            .expect("source fixture");
        let check = |generation, detail: &str| CachedUpdateCheck {
            source_id: 1,
            // Deliberately not the generation: what orders these writes is the
            // generation alone, and a displayed time that runs the other way
            // must not change the outcome.
            checked_at: 1_000 - generation,
            generation,
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
        assert_eq!(checks[0].generation, 20);
        assert_eq!(checks[0].checked_at, 980);
        assert_eq!(checks[0].detail, "newer");
        store
            .record_update_check(&check(10, "older single"))
            .expect("stale single check");
        assert_eq!(
            store.update_checks().expect("checks after stale single")[0].detail,
            "newer"
        );
    }

    #[test]
    fn verified_results_clear_only_earlier_failures_and_keep_check_ordering() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let mut store = Store::open(&temporary.path().join("data")).expect("store");
        store
            .connection
            .execute(
                "INSERT INTO source_repositories
             (id, label, canonical_path, head_revision, dirty, dirty_known, last_scan_at)
             VALUES (1, 'source', '/source', 'head', 0, 1, 0)",
                [],
            )
            .expect("source");
        let mut check = CachedUpdateCheck {
            source_id: 1,
            checked_at: 1000,
            generation: 10,
            local_revision: "head".into(),
            local_reference: Some("refs/heads/main".into()),
            upstream_ref: None,
            upstream_revision: None,
            merge_base: None,
            ahead: 0,
            behind: 0,
            dirty: false,
            dirty_known: true,
            verdict: RepositoryUpdateVerdict::UpToDate,
            detail: String::new(),
        };
        for code in [
            "update.apply_failed",
            "update.verification_failed",
            "update.verification_incomplete",
        ] {
            let mut failure = check.clone();
            failure.generation = 20;
            failure.verdict = RepositoryUpdateVerdict::Blocked;
            failure.detail = crate::updates::encode_findings(&[crate::inventory::Finding::new(
                code,
                crate::inventory::FindingSeverity::Critical,
                "earlier failure".into(),
            )]);
            store
                .record_update_check(&failure)
                .expect("earlier failure");
            store
                .record_verified_update_check(&check, 30)
                .expect("later success");
            let stored = &store.update_checks().unwrap()[0];
            assert_eq!(stored.verdict, RepositoryUpdateVerdict::UpToDate);
            assert_eq!(stored.generation, 10);
        }
        // A check reserved before verification finished can persist after it.
        let mut available = check.clone();
        available.generation = 20;
        available.verdict = RepositoryUpdateVerdict::Available;
        available.behind = 1;
        store
            .record_update_checks(&[available])
            .expect("delayed explicit check");
        store
            .record_verified_update_check(&check, 30)
            .expect("success after explicit check");
        assert_eq!(
            store.update_checks().unwrap()[0].verdict,
            RepositoryUpdateVerdict::Available
        );

        // A failure observed after this verification must not be cleared.
        let mut later = check.clone();
        later.generation = 40;
        later.verdict = RepositoryUpdateVerdict::Blocked;
        later.detail = "update.verification_failed|later failure".into();
        store.record_update_check(&later).expect("later failure");
        store
            .record_verified_update_check(&check, 30)
            .expect("delayed success");
        assert_eq!(store.update_checks().unwrap()[0].generation, 40);
        // Unrelated blocked checks are still explicit observations, not an
        // earlier failed write this success can answer for.
        check.generation = 50;
        check.verdict = RepositoryUpdateVerdict::Blocked;
        check.detail = "source.fetch_failed|network failed".into();
        store.record_update_check(&check).expect("blocked check");
        let mut success = check.clone();
        success.generation = 10;
        success.verdict = RepositoryUpdateVerdict::UpToDate;
        success.detail.clear();
        store
            .record_verified_update_check(&success, 60)
            .expect("success");
        assert_eq!(store.update_checks().unwrap()[0].detail, check.detail);
    }

    #[test]
    fn mutation_guards_serialize_independent_store_connections() {
        let temporary = tempfile::tempdir().expect("temporary data directory");
        let data_dir = temporary.path().join("data");
        let mut first = Store::open(&data_dir).expect("first store");
        let mut second = Store::open(&data_dir).expect("second store");

        let guard = first.begin_mutation().expect("first mutation guard");
        assert!(
            second.begin_mutation().is_err(),
            "a second writer must not enter while the first can touch the filesystem"
        );

        drop(guard);
        let guard = second
            .begin_mutation()
            .expect("guard becomes available after rollback");
        guard.commit().expect("commit empty guard");
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
    Migration {
        version: 6,
        // `DROP TABLE`, and therefore destructive: the rebuild below is the
        // one migration in this list that cannot be undone by reverting to an
        // older build, so it is the one that earns a backup.
        destructive: true,
        // SQLite cannot alter either a CHECK or UNIQUE constraint in place.
        // Rebuilding inside this transaction preserves every existing receipt,
        // including its id: ordering by `(created_at, id)` is the public
        // oldest-to-newest contract used by ownership matching.
        //
        // The same version introduces the source-ID sequence: source IDs are
        // durable identities carried by previews and receipts, and SQLite may
        // reuse a deleted INTEGER PRIMARY KEY, so allocate them from a
        // monotonic sequence that Forget Source never rewinds.
        sql: "CREATE TABLE operation_receipts_v6 (
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
              CREATE TABLE source_id_sequence (
                singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                next_id INTEGER NOT NULL CHECK (next_id > 0)
              );
              INSERT INTO source_id_sequence (singleton, next_id)
              SELECT 1, COALESCE(MAX(id), 0) + 1 FROM source_repositories;",
    },
    Migration {
        version: 7,
        destructive: false,
        sql: "CREATE TABLE source_update_checks (
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
              );",
    },
    Migration {
        version: 8,
        destructive: false,
        sql: "ALTER TABLE source_update_checks ADD COLUMN local_reference TEXT;",
    },
    Migration {
        version: 9,
        destructive: false,
        sql: "ALTER TABLE source_repositories ADD COLUMN repository_identity TEXT;",
    },
    Migration {
        version: 10,
        destructive: false,
        // Main introduced the monotonic source-ID sequence as schema 6 while
        // the update branch already used schemas 7 through 9. Existing
        // databases from either line therefore need this idempotent join
        // migration, while a fresh database already has the table from v6.
        sql: "CREATE TABLE IF NOT EXISTS source_id_sequence (
                singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                next_id INTEGER NOT NULL CHECK (next_id > 0)
              );
              INSERT OR IGNORE INTO source_id_sequence (singleton, next_id)
              SELECT 1, COALESCE(MAX(id), 0) + 1 FROM source_repositories;",
    },
    Migration {
        version: 11,
        destructive: false,
        // `checked_at` used to order these writes as well as date them, which
        // is why the seed is the value each row already holds: those are the
        // generations its checks were recorded under, and
        // `reserve_update_check_generations` reads them as its high-water
        // mark. Seeding a constant instead would put every upgraded row at the
        // same point in the order, where one of them displaces the rest.
        sql: "ALTER TABLE source_update_checks
                ADD COLUMN generation INTEGER NOT NULL DEFAULT 0;
              UPDATE source_update_checks SET generation = checked_at;",
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
    let supported = migrations
        .last()
        .map_or(SCHEMA_VERSION, |migration| migration.version);
    let mut backup = None;
    // At most two passes: the first discovers what is pending, and a second is
    // taken only to re-read the version after the backup released the lock.
    loop {
        // Version discovery and every migration share one write transaction. A
        // second process therefore reads the version only after any first
        // opener has finished upgrading, rather than acting on a value
        // observed before it waited for the migration lock.
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current_version: i64 =
            transaction.query_row("PRAGMA user_version", [], |row| row.get(0))?;
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
            return Ok(backup);
        }
        // `VACUUM INTO` cannot run inside a transaction, so the lock is
        // released for the backup and taken again afterwards. Re-reading the
        // version is the point of taking it again: another process may have
        // migrated in the gap, which leaves a backup of a database that no
        // longer needed one — never a migration applied over an unknown state.
        if backup.is_none() && pending.iter().any(|migration| migration.destructive) {
            drop(transaction);
            backup = Some(backup_database(connection, database_path, current_version)?);
            continue;
        }
        for migration in pending {
            transaction.execute_batch(migration.sql)?;
            transaction.pragma_update(None, "user_version", migration.version)?;
        }
        transaction.commit()?;
        return Ok(backup);
    }
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
/// INTO`. The destination is then reserved with create-new semantics, which
/// is the check: SQLite accepts an *empty* file for `VACUUM INTO`, so a bare
/// absence test would let a zero-byte file that appeared after it be written
/// into rather than refused. `O_CREAT | O_EXCL` also refuses a symbolic link,
/// so the reserved object is the pathname's own regular file, created for the
/// owner alone. No file is ever replaced or removed.
///
/// One pathname window survives it, and it survives on purpose. The
/// reservation is closed before `VACUUM INTO` reopens the name, so anything
/// able to write the application-data directory could unlink the reservation
/// and leave another empty file for SQLite to populate — losing both the
/// no-overwrite rule and the owner-only mode. Holding the reservation open
/// does not close it: SQLite has no open-by-descriptor, so every mechanism
/// that could write this backup opens by pathname, `sqlite3_backup` included
/// — its destination is a `Connection`, and a `Connection` is opened by name.
/// The window is a property of the API rather than of this call.
///
/// What actually bounds it is the directory. An attacker who can write the
/// application-data directory can already read, replace, or delete the
/// database this backup is a copy of, so there is no privacy here left for
/// the backup to lose that they do not already have. It is the same class of
/// window as `skilled-cb2`, and narrowing it further is tracked as
/// `skilled-2k3.24`.
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
        let candidate_text = candidate
            .to_str()
            .ok_or_else(|| Error::UnrepresentablePath(candidate.clone()))?
            .to_owned();
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        // A backup holds every registered repository path and ownership
        // receipt the database holds. `VACUUM INTO` does not narrow the mode
        // of a file it finds, so the reservation is where the mode is set, and
        // it is set to the owner alone rather than to whatever the umask
        // happens to leave.
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        match options.open(&candidate) {
            Ok(reserved) => drop(reserved),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
        // A failed population leaves the reservation where it is. Removing it
        // would mean unlinking a pathname this call no longer holds open — and
        // so, if anything replaced it in between, unlinking something Skilled
        // did not create. An unused empty file is the cheaper of the two, and
        // the only one consistent with never unlinking.
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

    /// A check recorded before the ordering generation had a column of its own
    /// was ordered by the value it was dated with, so that value is the
    /// generation it carries. Seeding the column with anything else — zero,
    /// most obviously — would put every upgraded row behind the next check to
    /// be reserved, which is harmless, or ahead of it, which silently drops
    /// results.
    #[test]
    fn upgrading_seeds_each_checks_generation_from_the_value_that_ordered_it() {
        let temporary = tempfile::tempdir().expect("temporary data directory");
        let database = temporary.path().join("skilled.sqlite3");
        let mut connection = Connection::open(&database).expect("create database");
        let before: Vec<Migration> = MIGRATIONS
            .iter()
            .copied()
            .filter(|migration| migration.version < 11)
            .collect();
        migrate_with(&mut connection, &database, &before).expect("migrate to schema 10");
        connection
            .execute_batch(
                "INSERT INTO source_repositories
                    (id, label, canonical_path, head_revision, dirty, dirty_known, last_scan_at)
                 VALUES (1, 'source', '/source', 'head', 0, 1, 0);
                 INSERT INTO source_update_checks
                    (source_id, checked_at, local_revision, ahead, behind, dirty, dirty_known,
                     verdict, detail)
                 VALUES (1, 9000, 'head', 0, 0, 0, 1, 'up_to_date', '');",
            )
            .expect("check recorded before the generation column existed");

        migrate_with(&mut connection, &database, MIGRATIONS)
            .expect("migrate to the current schema");

        assert_eq!(
            connection
                .query_row("SELECT generation FROM source_update_checks", [], |row| row
                    .get::<_, i64>(0))
                .expect("read seeded generation"),
            9000
        );
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

    /// `VACUUM INTO` accepts an existing empty file, so an absence check alone
    /// would let a zero-byte occupant be written into. An occupied path is
    /// occupied whatever its size.
    #[test]
    fn an_empty_occupied_backup_candidate_is_refused_like_any_other() {
        let temporary = tempfile::tempdir().expect("temporary data directory");
        let database = temporary.path().join("skilled.sqlite3");
        let connection = Connection::open(&database).expect("create database");
        connection
            .execute_batch(
                "CREATE TABLE original (value TEXT); INSERT INTO original VALUES ('row');",
            )
            .expect("create database contents");
        let occupied = temporary.path().join("empty.backup");
        fs::write(&occupied, b"").expect("occupy backup candidate with an empty file");
        let mut attempt = 0;

        let backup = backup_database_with(&connection, &database, 0, |_| {
            attempt += 1;
            if attempt == 1 {
                "empty.backup".to_owned()
            } else {
                "fresh.backup".to_owned()
            }
        })
        .expect("create distinct backup");

        assert_eq!(backup, temporary.path().join("fresh.backup"));
        assert_eq!(
            fs::metadata(&occupied).expect("reread occupied file").len(),
            0
        );
        assert!(fs::metadata(&backup).expect("read backup").len() > 0);
    }

    /// A backup carries every repository path and ownership receipt the
    /// database carries, so it is not left at whatever the umask allows.
    #[cfg(unix)]
    #[test]
    fn a_backup_is_readable_by_its_owner_alone() {
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempfile::tempdir().expect("temporary data directory");
        let database = temporary.path().join("skilled.sqlite3");
        let connection = Connection::open(&database).expect("create database");
        connection
            .execute_batch("CREATE TABLE original (value TEXT); INSERT INTO original VALUES ('r');")
            .expect("create database contents");

        let backup = backup_database_with(&connection, &database, 0, |_| "owner.backup".to_owned())
            .expect("create backup");

        let mode = fs::metadata(&backup)
            .expect("read backup metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "backup mode {:o}", mode & 0o777);
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
