use std::{fs, path::Path};

use rusqlite::{Connection, OptionalExtension, params};

use crate::Result;

const SCHEMA_VERSION: i64 = 2;

pub(crate) struct Store {
    connection: Connection,
}

impl Store {
    pub(crate) fn open(data_dir: &Path) -> Result<Self> {
        fs::create_dir_all(data_dir)?;
        let mut connection = Connection::open(data_dir.join("skilled.sqlite3"))?;
        migrate(&mut connection)?;
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

    pub(crate) fn set_agent_selections(&mut self, selections: [bool; 3]) -> Result<()> {
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
        transaction.commit()?;
        Ok(())
    }
}

fn migrate(connection: &mut Connection) -> Result<()> {
    let current_version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;

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
    if current_version < SCHEMA_VERSION {
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
    Ok(())
}
