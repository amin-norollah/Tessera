//! SQLite project catalog (P4-F7 provenance, gate G4.5): lightweight run
//! metadata only — a config's text, its seed, and where its keyframe/event
//! artifacts live on disk. The trace data itself stays in open Parquet files
//! (P4-N4, portable/queryable outside Tessera); the catalog just indexes
//! where to find them, and never depends on `tessera-config` (staying below
//! it in the dependency direction) — a run's config travels as opaque text.

use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::RecordError;

/// A run already registered in the catalog.
#[derive(Clone, Debug, PartialEq)]
pub struct RunRecord {
    pub id: i64,
    pub name: String,
    /// The run's config, verbatim (e.g. the TOML text `tessera-config` would
    /// parse) — opaque to this crate, kept only for provenance/reproduction.
    pub config_text: String,
    pub seed: u64,
    pub keyframe_dir: Option<String>,
    pub event_store: Option<String>,
    /// Unix timestamp (seconds) the run was registered, supplied by the
    /// caller rather than stamped here, so registration stays a pure,
    /// deterministically testable operation.
    pub created_unix: i64,
}

/// A run to register; the catalog assigns `id`.
#[derive(Clone, Debug)]
pub struct NewRun {
    pub name: String,
    pub config_text: String,
    pub seed: u64,
    pub keyframe_dir: Option<String>,
    pub event_store: Option<String>,
    pub created_unix: i64,
}

/// The project run catalog, backed by a single SQLite file.
pub struct Catalog {
    conn: Connection,
}

impl Catalog {
    /// Opens (creating if needed) the catalog at `path`, ensuring its schema
    /// exists.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, RecordError> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS runs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                config_text TEXT NOT NULL,
                seed INTEGER NOT NULL,
                keyframe_dir TEXT,
                event_store TEXT,
                created_unix INTEGER NOT NULL
            );",
        )?;
        Ok(Self { conn })
    }

    /// Registers a new run, returning its assigned id.
    pub fn register_run(&self, run: &NewRun) -> Result<i64, RecordError> {
        self.conn.execute(
            "INSERT INTO runs (name, config_text, seed, keyframe_dir, event_store, created_unix)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                run.name,
                run.config_text,
                run.seed as i64,
                run.keyframe_dir,
                run.event_store,
                run.created_unix,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Looks up one run by id, or `None` if it isn't registered.
    pub fn get_run(&self, id: i64) -> Result<Option<RunRecord>, RecordError> {
        self.conn
            .query_row(
                "SELECT id, name, config_text, seed, keyframe_dir, event_store, created_unix
                 FROM runs WHERE id = ?1",
                params![id],
                Self::row_to_run,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Every registered run, oldest first.
    pub fn list_runs(&self) -> Result<Vec<RunRecord>, RecordError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, config_text, seed, keyframe_dir, event_store, created_unix
             FROM runs ORDER BY id",
        )?;
        let rows = stmt.query_map([], Self::row_to_run)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    fn row_to_run(row: &Row) -> rusqlite::Result<RunRecord> {
        Ok(RunRecord {
            id: row.get(0)?,
            name: row.get(1)?,
            config_text: row.get(2)?,
            seed: row.get::<_, i64>(3)? as u64,
            keyframe_dir: row.get(4)?,
            event_store: row.get(5)?,
            created_unix: row.get(6)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_path(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "tessera_record_catalog_test_{name}_{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        path
    }

    fn new_run(name: &str, seed: u64) -> NewRun {
        NewRun {
            name: name.to_string(),
            config_text: "seed = 1\n".to_string(),
            seed,
            keyframe_dir: Some("/runs/1/keyframes".to_string()),
            event_store: Some("/runs/1/events.parquet".to_string()),
            created_unix: 1_700_000_000,
        }
    }

    #[test]
    fn registers_and_looks_up_a_run_with_provenance_intact() {
        let path = scratch_path("register");
        let cat = Catalog::open(&path).unwrap();
        let id = cat.register_run(&new_run("baseline", 42)).unwrap();

        let run = cat
            .get_run(id)
            .unwrap()
            .expect("just-registered run exists");
        assert_eq!(run.name, "baseline");
        assert_eq!(run.seed, 42);
        assert_eq!(run.config_text, "seed = 1\n");
        assert_eq!(run.keyframe_dir.as_deref(), Some("/runs/1/keyframes"));
        assert_eq!(run.event_store.as_deref(), Some("/runs/1/events.parquet"));
        assert_eq!(run.created_unix, 1_700_000_000);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn unknown_run_id_is_none_not_an_error() {
        let path = scratch_path("unknown");
        let cat = Catalog::open(&path).unwrap();
        assert_eq!(cat.get_run(999).unwrap(), None);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn lists_runs_oldest_first() {
        let path = scratch_path("list");
        let cat = Catalog::open(&path).unwrap();
        let a = cat.register_run(&new_run("a", 1)).unwrap();
        let b = cat.register_run(&new_run("b", 2)).unwrap();

        let runs = cat.list_runs().unwrap();
        assert_eq!(runs.iter().map(|r| r.id).collect::<Vec<_>>(), vec![a, b]);
        assert_eq!(
            runs.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            vec!["a", "b"]
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn reopening_the_same_file_preserves_registered_runs() {
        let path = scratch_path("reopen");
        {
            let cat = Catalog::open(&path).unwrap();
            cat.register_run(&new_run("persisted", 7)).unwrap();
        }
        let cat = Catalog::open(&path).unwrap();
        let runs = cat.list_runs().unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].name, "persisted");

        let _ = std::fs::remove_file(&path);
    }
}
