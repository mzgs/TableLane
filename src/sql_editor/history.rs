use crate::settings::{Settings, read_json, write_json};
use serde::{Deserialize, Serialize};
use std::{io, path::PathBuf};

const HISTORY_LIMIT: usize = 100;

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Entry {
    pub(super) id: u64,
    pub(super) connection: String,
    pub(super) database: String,
    pub(super) sql: String,
    pub(super) status: String,
    pub(super) failed: bool,
}

pub(super) struct History {
    entries: Vec<Entry>,
    path: Option<PathBuf>,
    pub(super) error: Option<String>,
}

impl gpui_kit::Global for History {}

impl History {
    #[cfg(test)]
    pub(super) fn for_test() -> Self {
        Self {
            entries: Vec::new(),
            path: None,
            error: None,
        }
    }

    pub(super) fn load() -> Self {
        let result = Settings::path()
            .map(|path| path.with_file_name("sql-history.json"))
            .and_then(Self::read);
        result.unwrap_or_else(|error| Self {
            entries: Vec::new(),
            path: None,
            error: Some(format!(
                "Couldn’t load SQL history: {error}. History saving is disabled until restart."
            )),
        })
    }

    fn read(path: PathBuf) -> io::Result<Self> {
        let mut entries: Vec<Entry> = match read_json(&path) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error),
        };
        entries.drain(..entries.len().saturating_sub(HISTORY_LIMIT));
        Ok(Self {
            entries,
            path: Some(path),
            error: None,
        })
    }

    pub(super) fn entries(&self, connection: &str, database: &str) -> impl Iterator<Item = &Entry> {
        self.entries
            .iter()
            .rev()
            .filter(move |entry| entry.connection == connection && entry.database == database)
    }

    pub(super) fn record(
        &mut self,
        connection: String,
        database: String,
        sql: String,
        status: String,
        failed: bool,
    ) {
        let id = self
            .entries
            .iter()
            .map(|entry| entry.id)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        self.entries.push(Entry {
            id,
            connection,
            database,
            sql,
            status,
            failed,
        });
        self.entries
            .drain(..self.entries.len().saturating_sub(HISTORY_LIMIT));
        if let Some(path) = &self.path {
            self.error = write_json(path, &self.entries).err().map(|error| {
                format!("Couldn’t save SQL history: {error}. Recent commands remain available for this session.")
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_persists_failed_runs_scopes_orders_bounds_and_preserves_invalid_files()
    -> io::Result<()> {
        let directory =
            std::env::temp_dir().join(format!("tablelane-history-test-{}", std::process::id()));
        std::fs::create_dir(&directory)?;
        let path = directory.join("sql-history.json");
        let result = (|| -> io::Result<()> {
            let mut history = History::read(path.clone())?;
            assert!(history.entries("Local", "app").next().is_none());
            history.record(
                "Other".into(),
                "app".into(),
                "SELECT 0".into(),
                "Success".into(),
                false,
            );
            history.record(
                "Local".into(),
                "other".into(),
                "SELECT 1".into(),
                "Success".into(),
                false,
            );
            history.record(
                "Local".into(),
                "app".into(),
                "SELECT '世界'".into(),
                "Success".into(),
                false,
            );
            history.record(
                "Local".into(),
                "app".into(),
                "bad SQL".into(),
                "Syntax error".into(),
                true,
            );
            let loaded = History::read(path.clone())?;
            let entries: Vec<_> = loaded.entries("Local", "app").collect();
            assert_eq!(entries.len(), 2);
            assert_eq!(entries[0].sql, "bad SQL");
            assert!(entries[0].failed);
            assert_eq!(entries[1].sql, "SELECT '世界'");
            for ix in 0..HISTORY_LIMIT {
                history.record(
                    "Local".into(),
                    "app".into(),
                    format!("SELECT {ix}"),
                    "Success".into(),
                    false,
                );
            }
            assert_eq!(History::read(path.clone())?.entries.len(), HISTORY_LIMIT);
            assert_eq!(
                history.entries("Local", "app").next().unwrap().sql,
                "SELECT 99"
            );
            std::fs::write(&path, "invalid history")?;
            assert!(History::read(path.clone()).is_err());
            assert_eq!(std::fs::read_to_string(&path)?, "invalid history");
            history.path = Some(directory.clone());
            history.record(
                "Local".into(),
                "app".into(),
                "SELECT 100".into(),
                "Success".into(),
                false,
            );
            assert!(history.error.is_some());
            assert_eq!(
                history.entries("Local", "app").next().unwrap().sql,
                "SELECT 100"
            );
            Ok(())
        })();
        std::fs::remove_dir_all(directory)?;
        result
    }
}
