use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};

use crate::trial::store::{TrialRecord, TrialStatus};

#[derive(Debug, Clone)]
pub struct TrialDb {
    path: PathBuf,
}

impl TrialDb {
    pub fn new<P: AsRef<Path>>(path: P) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    pub fn upsert_record(&self, record: &TrialRecord) -> std::io::Result<()> {
        self.with_conn(|conn| {
            let fields_json =
                serde_json::to_string(&record.fields).map_err(to_sql_conversion_error)?;
            conn.execute(
                r#"
                INSERT INTO trial_records (trial_id, status, elapsed_ms, error, fields_json)
                VALUES (?1, ?2, ?3, ?4, ?5)
                ON CONFLICT(trial_id) DO UPDATE SET
                    status = excluded.status,
                    elapsed_ms = excluded.elapsed_ms,
                    error = excluded.error,
                    fields_json = excluded.fields_json
                "#,
                params![
                    record.trial_id as i64,
                    record.status.as_str(),
                    record.elapsed_ms as i64,
                    record.error.as_ref(),
                    fields_json
                ],
            )?;
            Ok(())
        })
    }

    pub fn insert_epoch_record(&self, record: &TrialRecord) -> std::io::Result<()> {
        self.with_conn(|conn| {
            let fields_json =
                serde_json::to_string(&record.fields).map_err(to_sql_conversion_error)?;
            conn.execute(
                r#"
                INSERT INTO trial_epoch_records (trial_id, status, elapsed_ms, error, fields_json)
                VALUES (?1, ?2, ?3, ?4, ?5)
                "#,
                params![
                    record.trial_id as i64,
                    record.status.as_str(),
                    record.elapsed_ms as i64,
                    record.error.as_ref(),
                    fields_json
                ],
            )?;
            Ok(())
        })
    }

    pub fn load_record(&self, trial_id: usize) -> std::io::Result<Option<TrialRecord>> {
        self.with_conn(|conn| {
            conn.query_row(
                r#"
                SELECT trial_id, status, elapsed_ms, error, fields_json
                FROM trial_records
                WHERE trial_id = ?1
                "#,
                params![trial_id as i64],
                |row| {
                    let status = row.get::<_, String>(1)?;
                    let fields_json = row.get::<_, String>(4)?;
                    let fields: BTreeMap<String, String> =
                        serde_json::from_str(&fields_json).map_err(to_from_sql_error)?;
                    Ok(TrialRecord {
                        trial_id: row.get::<_, i64>(0)? as usize,
                        status: TrialStatus::parse(&status)
                            .ok_or_else(|| rusqlite::Error::InvalidQuery)?,
                        elapsed_ms: row.get::<_, i64>(2)? as u128,
                        error: row.get::<_, Option<String>>(3)?,
                        fields,
                    })
                },
            )
            .optional()
        })
    }

    pub fn load_records(&self) -> std::io::Result<Vec<TrialRecord>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                r#"
                SELECT trial_id, status, elapsed_ms, error, fields_json
                FROM trial_records
                ORDER BY trial_id ASC
                "#,
            )?;
            let rows = stmt.query_map([], |row| {
                let status = row.get::<_, String>(1)?;
                let fields_json = row.get::<_, String>(4)?;
                let fields: BTreeMap<String, String> =
                    serde_json::from_str(&fields_json).map_err(to_from_sql_error)?;
                Ok(TrialRecord {
                    trial_id: row.get::<_, i64>(0)? as usize,
                    status: TrialStatus::parse(&status)
                        .ok_or_else(|| rusqlite::Error::InvalidQuery)?,
                    elapsed_ms: row.get::<_, i64>(2)? as u128,
                    error: row.get::<_, Option<String>>(3)?,
                    fields,
                })
            })?;
            let mut records = Vec::new();
            for record in rows {
                records.push(record?);
            }
            Ok(records)
        })
    }

    pub fn load_epoch_records(&self) -> std::io::Result<Vec<TrialRecord>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                r#"
                SELECT trial_id, status, elapsed_ms, error, fields_json
                FROM trial_epoch_records
                ORDER BY trial_id ASC, row_id ASC
                "#,
            )?;
            let rows = stmt.query_map([], |row| {
                let status = row.get::<_, String>(1)?;
                let fields_json = row.get::<_, String>(4)?;
                let fields: BTreeMap<String, String> =
                    serde_json::from_str(&fields_json).map_err(to_from_sql_error)?;
                Ok(TrialRecord {
                    trial_id: row.get::<_, i64>(0)? as usize,
                    status: TrialStatus::parse(&status)
                        .ok_or_else(|| rusqlite::Error::InvalidQuery)?,
                    elapsed_ms: row.get::<_, i64>(2)? as u128,
                    error: row.get::<_, Option<String>>(3)?,
                    fields,
                })
            })?;
            let mut records = Vec::new();
            for record in rows {
                records.push(record?);
            }
            Ok(records)
        })
    }

    pub fn load_project_config(&self) -> std::io::Result<Option<String>> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT value FROM project_metadata WHERE key = ?1",
                params![PROJECT_CONFIG_KEY],
                |row| row.get(0),
            )
            .optional()
        })
    }

    pub fn save_project_config(&self, config: &str) -> std::io::Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                r#"
                INSERT INTO project_metadata (key, value)
                VALUES (?1, ?2)
                ON CONFLICT(key) DO UPDATE SET value = excluded.value
                "#,
                params![PROJECT_CONFIG_KEY, config],
            )?;
            Ok(())
        })
    }

    pub fn has_any_trials(&self) -> std::io::Result<bool> {
        self.with_conn(|conn| {
            let has_records: Option<i64> = conn
                .query_row("SELECT 1 FROM trial_records LIMIT 1", [], |row| row.get(0))
                .optional()?;
            if has_records.is_some() {
                return Ok(true);
            }
            let has_epochs: Option<i64> = conn
                .query_row("SELECT 1 FROM trial_epoch_records LIMIT 1", [], |row| {
                    row.get(0)
                })
                .optional()?;
            Ok(has_epochs.is_some())
        })
    }

    pub fn next_trial_id(&self) -> std::io::Result<usize> {
        self.with_conn(|conn| {
            let max_id: Option<i64> =
                conn.query_row("SELECT MAX(trial_id) FROM trial_records", [], |row| {
                    row.get(0)
                })?;
            Ok(max_id.map_or(0, |id| id as usize + 1))
        })
    }

    fn with_conn<T, F>(&self, op: F) -> std::io::Result<T>
    where
        F: FnOnce(&Connection) -> rusqlite::Result<T>,
    {
        let conn = Connection::open(&self.path).map_err(to_io_error)?;
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;
            CREATE TABLE IF NOT EXISTS trial_records (
                trial_id INTEGER PRIMARY KEY,
                status TEXT NOT NULL,
                elapsed_ms INTEGER NOT NULL,
                error TEXT,
                fields_json TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS trial_epoch_records (
                row_id INTEGER PRIMARY KEY AUTOINCREMENT,
                trial_id INTEGER NOT NULL,
                status TEXT NOT NULL,
                elapsed_ms INTEGER NOT NULL,
                error TEXT,
                fields_json TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS project_metadata (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            "#,
        )
        .map_err(to_io_error)?;
        op(&conn).map_err(to_io_error)
    }
}

const PROJECT_CONFIG_KEY: &str = "project_config_toml";

fn to_io_error(err: rusqlite::Error) -> std::io::Error {
    std::io::Error::other(err)
}

fn to_sql_conversion_error(err: serde_json::Error) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(err))
}

fn to_from_sql_error(err: serde_json::Error) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(err))
}
