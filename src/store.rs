use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OptionalExtension, params};

use crate::{
    AgentKind, Error, Result,
    operations::Receipt,
    source::{
        CatalogClassification, CatalogProposal, Compatibility, InspectedSource, RegisteredSource,
        SourcePreview, contains_revision, inspect_local_source,
    },
    validation::InspectionBudget,
};

const SCHEMA_VERSION: i64 = 5;

pub(crate) struct Store {
    connection: Connection,
}

impl Store {
    pub(crate) fn open(data_dir: &Path) -> Result<Self> {
        fs::create_dir_all(data_dir)?;
        let mut connection = Connection::open(data_dir.join("skilled.sqlite3"))?;
        migrate(&mut connection)?;
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
        for (index, agent) in ["claude-code", "codex", "opencode"].iter().enumerate() {
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
        let transaction = self.connection.transaction()?;
        for (agent, selected) in ["claude-code", "codex", "opencode"]
            .into_iter()
            .zip(selections)
        {
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

    /// Record that Skilled created one particular link.
    ///
    /// One statement, and therefore its own transaction: the receipt is written
    /// the moment the link exists, so a crash between two targets still leaves
    /// the store describing exactly what is on disk.
    pub(crate) fn record_receipt(&self, receipt: &Receipt) -> Result<()> {
        self.connection.execute(
            "INSERT INTO operation_receipts
                (created_at, operation, agent, skill_name, link_path, link_target,
                 source_id, catalog_relative_path, variant_relative_path)
             VALUES (?1, 'install', ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                current_timestamp(),
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
    /// A row naming an agent this build does not know is an error rather than a
    /// row to skip: a receipt Skilled cannot read is ownership it would go on to
    /// deny, and denying it would let the next plan treat its own link as a
    /// stranger's.
    pub(crate) fn receipts(&self) -> Result<Vec<Receipt>> {
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
                    Ok(true) => {
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

fn migrate(connection: &mut Connection) -> Result<()> {
    let current_version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if current_version > SCHEMA_VERSION {
        return Err(crate::Error::UnsupportedSchema {
            found: current_version,
            supported: SCHEMA_VERSION,
        });
    }

    if current_version < 1 {
        let transaction = connection.transaction()?;
        transaction.execute_batch(
            "CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY NOT NULL,
                value TEXT NOT NULL
             );
             PRAGMA user_version = 1;",
        )?;
        transaction.commit()?;
    }
    if current_version < 2 {
        let transaction = connection.transaction()?;
        transaction.execute_batch(
            "CREATE TABLE configured_agents (
                agent TEXT PRIMARY KEY NOT NULL,
                selected INTEGER NOT NULL CHECK (selected IN (0, 1))
             );
             PRAGMA user_version = 2;",
        )?;
        transaction.commit()?;
    }
    if current_version < 3 {
        let transaction = connection.transaction()?;
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
        transaction.commit()?;
    }
    if current_version < 4 {
        let transaction = connection.transaction()?;
        transaction.execute_batch(
            "ALTER TABLE source_repositories ADD COLUMN
                dirty_known INTEGER NOT NULL DEFAULT 1 CHECK (dirty_known IN (0, 1));
             PRAGMA user_version = 4;",
        )?;
        transaction.commit()?;
    }
    if current_version < 5 {
        let transaction = connection.transaction()?;
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
                UNIQUE (agent, link_path, created_at)
             );
             PRAGMA user_version = 5;",
        )?;
        transaction.commit()?;
    }
    Ok(())
}
