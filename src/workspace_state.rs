use crate::{
    connections::OpenTable,
    database::{PAGE_SIZE, TableQuery},
    settings::{read_json, write_json},
};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, io, path::PathBuf};

#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SqlDraft {
    pub(crate) number: usize,
    pub(crate) connection_key: String,
    pub(crate) connection_name: String,
    pub(crate) database: String,
    pub(crate) table: String,
    pub(crate) sql: String,
}

impl SqlDraft {
    pub(crate) fn new(request: &OpenTable) -> Self {
        Self {
            number: 0,
            connection_key: request.session.connection_key().to_owned(),
            connection_name: request.connection_name.clone(),
            database: request.database.clone(),
            table: request.table.clone(),
            sql: String::new(),
        }
    }
}

#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TableDraft {
    pub(crate) connection_key: String,
    pub(crate) connection_name: String,
    pub(crate) database: String,
    pub(crate) table: String,
    pub(crate) query: TableQuery,
}

impl TableDraft {
    pub(crate) fn new(request: &OpenTable) -> Self {
        Self {
            connection_key: request.session.connection_key().to_owned(),
            connection_name: request.connection_name.clone(),
            database: request.database.clone(),
            table: request.table.clone(),
            query: TableQuery::default(),
        }
    }
}

// Keep the original SQL-only workspace format readable.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum SavedTab {
    Sql(SqlDraft),
    Table(TableDraft),
}

impl SavedTab {
    #[cfg(test)]
    pub(crate) fn sql(&self) -> &SqlDraft {
        let Self::Sql(draft) = self else {
            panic!("Expected SQL draft")
        };
        draft
    }
}

#[non_exhaustive]
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct WorkspaceState {
    version: u32,
    pub(crate) tabs: Vec<SavedTab>,
    pub(crate) active: usize,
}

impl Default for WorkspaceState {
    fn default() -> Self {
        Self {
            version: 1,
            tabs: Vec::new(),
            active: 0,
        }
    }
}

impl WorkspaceState {
    fn validate(&self) -> io::Result<()> {
        let mut numbers = HashSet::new();
        if self.version != 1
            || self.active >= self.tabs.len().max(1)
            || self.tabs.iter().any(|tab| match tab {
                SavedTab::Sql(tab) => {
                    tab.number == 0
                        || tab.number == usize::MAX
                        || !numbers.insert(tab.number)
                        || tab.connection_key.is_empty()
                        || tab.connection_name.trim().is_empty()
                        || tab.database.is_empty()
                }
                SavedTab::Table(tab) => {
                    let mut columns = HashSet::new();
                    tab.connection_key.is_empty()
                        || tab.connection_name.trim().is_empty()
                        || tab.database.is_empty()
                        || tab.table.is_empty()
                        || tab.query.page.checked_mul(PAGE_SIZE).is_none()
                        || tab
                            .query
                            .sort
                            .as_ref()
                            .is_some_and(|(name, _)| name.is_empty())
                        || tab.query.filters.iter().any(|filter| {
                            !columns.insert(&filter.column)
                                || filter.column.as_ref().is_some_and(String::is_empty)
                                || filter.value.is_empty()
                        })
                }
            })
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid workspace state",
            ));
        }
        Ok(())
    }
}

pub(crate) struct WorkspaceFile {
    path: Option<PathBuf>,
    error: Option<String>,
    load_failed: bool,
}

impl WorkspaceFile {
    pub(crate) fn load(path: Option<PathBuf>) -> (Self, WorkspaceState) {
        let loaded = path.as_ref().map_or(Ok(WorkspaceState::default()), |path| {
            let state: WorkspaceState = match read_json(path) {
                Ok(state) => state,
                Err(error) if error.kind() == io::ErrorKind::NotFound => WorkspaceState::default(),
                Err(error) => return Err(error),
            };
            state.validate()?;
            Ok(state)
        });
        match loaded {
            Ok(state) => (
                Self {
                    path,
                    error: None,
                    load_failed: false,
                },
                state,
            ),
            Err(error) => (
                Self {
                    path: None,
                    error: Some(format!(
                        "Couldn’t load workspace: {error}. The existing file is preserved and autosave is disabled. Fix workspace.json and restart."
                    )),
                    load_failed: true,
                },
                WorkspaceState::default(),
            ),
        }
    }

    pub(crate) fn error(&self) -> Option<&String> {
        self.error.as_ref()
    }

    pub(crate) fn is_readonly(&self) -> bool {
        self.load_failed
    }

    pub(crate) fn save(&mut self, state: &WorkspaceState) -> bool {
        if self.load_failed {
            return false;
        }
        let Some(path) = &self.path else {
            return true;
        };
        self.error = state.validate().and_then(|_| write_json(path, state)).err().map(|error| {
            format!("Couldn’t save workspace: {error}. Recent changes are only available in this session. Copy your SQL and note your table filters before quitting, or retry autosave.")
        });
        self.error.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_accepts_legacy_sql_and_validates_table_state() {
        let legacy = r#"{"version":1,"tabs":[{"number":1,"connection_key":"key","connection_name":"Local","database":"db","table":"widgets","sql":"SELECT 1"}],"active":0}"#;
        let mut state: WorkspaceState = serde_json::from_str(legacy).unwrap();
        assert!(state.validate().is_ok());
        assert_eq!(state.tabs[0].sql().sql, "SELECT 1");
        let mut draft = TableDraft {
            connection_key: "key".into(),
            connection_name: "Local".into(),
            database: "db".into(),
            table: "widgets".into(),
            query: TableQuery::default(),
        };
        draft.query.page = usize::MAX;
        state.tabs.push(SavedTab::Table(draft.clone()));
        assert!(state.validate().is_err());
        draft.query.page = 1;
        draft.query.filters = vec![crate::database::TableFilter {
            column: Some("id".into()),
            operator: crate::database::FilterOperator::GreaterThan,
            value: "10".into(),
        }];
        state.tabs[1] = SavedTab::Table(draft.clone());
        state.active = 1;
        assert!(state.validate().is_ok());
        let json = serde_json::to_string(&state).unwrap();
        assert_eq!(
            serde_json::from_str::<WorkspaceState>(&json).unwrap(),
            state
        );
        draft.query.filters.push(draft.query.filters[0].clone());
        state.tabs[1] = SavedTab::Table(draft);
        assert!(state.validate().is_err());
        assert!(
            serde_json::from_str::<WorkspaceState>(&json.replace("GreaterThan", "UnknownOperator"))
                .is_err()
        );
    }

    #[test]
    fn workspace_restores_exact_drafts_and_preserves_invalid_or_failed_saves() -> io::Result<()> {
        let directory =
            std::env::temp_dir().join(format!("tablelane-workspace-{}", std::process::id()));
        std::fs::create_dir(&directory)?;
        let path = directory.join("workspace.json");
        let (mut file, mut state) = WorkspaceFile::load(Some(path.clone()));
        assert!(state.tabs.is_empty());
        state.tabs = vec![SavedTab::Sql(SqlDraft {
            number: 3,
            connection_key: "key".into(),
            connection_name: "Local".into(),
            database: "db".into(),
            table: "table".into(),
            sql: "SELECT '世界';\n-- unfinished\n".into(),
        })];
        assert!(file.save(&state));
        let original = std::fs::read(&path)?;
        assert_eq!(WorkspaceFile::load(Some(path.clone())).1, state);
        let SavedTab::Sql(draft) = &mut state.tabs[0] else {
            unreachable!()
        };
        draft.sql = "x".repeat(1024 * 1024);
        assert!(!file.save(&state));
        assert_eq!(std::fs::read(&path)?, original);
        let SavedTab::Sql(draft) = &mut state.tabs[0] else {
            unreachable!()
        };
        draft.sql = "Recovered".into();
        assert!(file.save(&state));
        for invalid in [
            "invalid JSON",
            "{\"version\":2,\"tabs\":[],\"active\":0}",
            "{\"version\":1,\"tabs\":[],\"active\":1}",
        ] {
            std::fs::write(&path, invalid)?;
            let (mut file, _) = WorkspaceFile::load(Some(path.clone()));
            assert!(file.error().is_some());
            assert!(!file.save(&state));
            assert_eq!(std::fs::read_to_string(&path)?, invalid);
        }
        state.tabs.push(state.tabs[0].clone());
        assert!(state.validate().is_err());
        state.tabs.pop();
        state.tabs.clear();
        assert!(state.validate().is_ok());
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }
}
