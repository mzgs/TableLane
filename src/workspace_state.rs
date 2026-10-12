use crate::{
    connections::OpenTable,
    settings::{read_json, write_json},
};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, io, path::PathBuf};

#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct WorkspaceState {
    version: u32,
    pub(crate) tabs: Vec<SqlDraft>,
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
            || self.tabs.iter().any(|tab| {
                tab.number == 0
                    || tab.number == usize::MAX
                    || !numbers.insert(tab.number)
                    || tab.connection_key.is_empty()
                    || tab.connection_name.trim().is_empty()
                    || tab.database.is_empty()
            })
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid SQL workspace state",
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
                        "Couldn’t load SQL drafts: {error}. The existing file is preserved and autosave is disabled. Fix workspace.json and restart."
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
            format!("Couldn’t save SQL drafts: {error}. Recent changes are only available in this session. Copy your SQL before quitting, or retry autosave.")
        });
        self.error.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_restores_exact_drafts_and_preserves_invalid_or_failed_saves() -> io::Result<()> {
        let directory =
            std::env::temp_dir().join(format!("tablelane-workspace-{}", std::process::id()));
        std::fs::create_dir(&directory)?;
        let path = directory.join("workspace.json");
        let (mut file, mut state) = WorkspaceFile::load(Some(path.clone()));
        assert!(state.tabs.is_empty());
        state.tabs = vec![SqlDraft {
            number: 3,
            connection_key: "key".into(),
            connection_name: "Local".into(),
            database: "db".into(),
            table: "table".into(),
            sql: "SELECT '世界';\n-- unfinished\n".into(),
        }];
        assert!(file.save(&state));
        let original = std::fs::read(&path)?;
        assert_eq!(WorkspaceFile::load(Some(path.clone())).1, state);
        state.tabs[0].sql = "x".repeat(1024 * 1024);
        assert!(!file.save(&state));
        assert_eq!(std::fs::read(&path)?, original);
        state.tabs[0].sql = "Recovered".into();
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
