use serde::{Deserialize, Serialize};
mod password;
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

const MAX_SETTINGS_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum DatabaseKind {
    MySQL,
    MariaDB,
    MongoDB,
    SQLite,
    PostgreSQL,
}

impl DatabaseKind {
    pub(crate) const ALL: [Self; 5] = [
        Self::MySQL,
        Self::MariaDB,
        Self::MongoDB,
        Self::SQLite,
        Self::PostgreSQL,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::MySQL => "MySQL",
            Self::MariaDB => "MariaDB",
            Self::MongoDB => "MongoDB",
            Self::SQLite => "SQLite",
            Self::PostgreSQL => "PostgreSQL",
        }
    }

    pub(crate) fn port(self) -> Option<u16> {
        match self {
            Self::MySQL | Self::MariaDB => Some(3306),
            Self::MongoDB => Some(27017),
            Self::PostgreSQL => Some(5432),
            Self::SQLite => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Connection {
    pub(crate) name: String,
    pub(crate) database_type: DatabaseKind,
    pub(crate) host: String,
    pub(crate) port: Option<u16>,
    pub(crate) username: String,
    #[serde(with = "password")]
    pub(crate) password: String,
    pub(crate) database: String,
    pub(crate) file_path: String,
}

impl Connection {
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        if self.name.trim().is_empty() {
            return Err("Enter a connection name.");
        }
        if self.database_type == DatabaseKind::SQLite {
            if self.file_path.trim().is_empty() {
                return Err("Enter a SQLite file path.");
            }
        } else {
            if self.host.trim().is_empty() {
                return Err("Enter a host.");
            }
            if self.port.is_none_or(|port| port == 0) {
                return Err("Enter a port from 1 to 65535.");
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Settings {
    // Replace these examples with your app's settings.
    pub(crate) display_name: String,
    pub(crate) notifications_enabled: bool,
    pub(crate) recent_items_limit: u32,
    pub(crate) connections: Vec<Connection>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            display_name: "Guest".into(),
            notifications_enabled: true,
            recent_items_limit: 10,
            connections: Vec::new(),
        }
    }
}

impl Settings {
    pub(crate) fn path() -> io::Result<PathBuf> {
        let directory = dirs::data_local_dir()
            .ok_or_else(|| io::Error::other("Could not find the application data directory"))?;
        Ok(directory.join(env!("CARGO_PKG_NAME")).join("settings.json"))
    }

    pub(crate) fn load(path: &Path) -> io::Result<Self> {
        match Self::read(path) {
            Ok(settings) => Ok(settings),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let settings = Self::default();
                settings.save(path)?;
                Ok(settings)
            }
            Err(error) => Err(error),
        }
    }

    pub(crate) fn read(path: &Path) -> io::Result<Self> {
        let settings: Self = read_json(path)?;
        settings.validate()?;
        Ok(settings)
    }

    pub(crate) fn save(&self, path: &Path) -> io::Result<()> {
        self.validate()?;
        write_json(path, self)
    }

    fn validate(&self) -> io::Result<()> {
        for connection in &self.connections {
            connection
                .validate()
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        }
        Ok(())
    }
}

pub(crate) fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> io::Result<T> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(MAX_SETTINGS_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_SETTINGS_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Settings JSON exceeds 1 MiB",
        ));
    }
    Ok(serde_json::from_slice(&bytes)?)
}

pub(crate) fn write_json(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    if bytes.len() > MAX_SETTINGS_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Settings JSON exceeds 1 MiB",
        ));
    }
    let directory = path
        .parent()
        .ok_or_else(|| io::Error::other("Invalid settings path"))?;
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(directory)?;
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    let result = (|| {
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_persist_defaults_changes_and_preserve_invalid_files() -> io::Result<()> {
        let directory = std::env::temp_dir().join(format!(
            "{}-settings-test-{}",
            env!("CARGO_PKG_NAME"),
            std::process::id()
        ));
        fs::create_dir(&directory)?;
        let path = directory.join("app/settings.json");
        let result = (|| -> io::Result<()> {
            let mut settings = Settings::load(&path)?;
            assert_eq!(settings, Settings::default());
            assert!(path.exists());
            let missing = directory.join("missing.json");
            assert!(Settings::read(&missing).is_err());
            assert!(!missing.exists());
            settings.display_name = "Ada".into();
            settings.notifications_enabled = false;
            settings.recent_items_limit = 3;
            settings.save(&path)?;
            assert_eq!(Settings::load(&path)?, settings);
            fs::write(&path, r#"{"display_name":"Lin"}"#)?;
            assert_eq!(
                Settings::load(&path)?,
                Settings {
                    display_name: "Lin".into(),
                    ..Settings::default()
                }
            );
            for invalid in [
                "invalid JSON",
                r#"{"connections":[{"name":"Bad","database_type":"MySQL","host":"localhost","port":0,"username":"","password":"","database":"","file_path":""}]}"#,
                r#"{"recent_items_limit":-1}"#,
            ] {
                fs::write(&path, invalid)?;
                assert!(Settings::load(&path).is_err());
                assert_eq!(fs::read_to_string(&path)?, invalid);
            }
            let oversized = directory.join("oversized.json");
            fs::write(&oversized, vec![b' '; MAX_SETTINGS_BYTES + 1])?;
            assert_eq!(
                Settings::read(&oversized).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
            let huge = Settings {
                display_name: "x".repeat(MAX_SETTINGS_BYTES),
                ..Settings::default()
            };
            assert!(huge.save(&path).is_err());
            assert_eq!(fs::read_to_string(&path)?, r#"{"recent_items_limit":-1}"#);
            let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
            fs::write(&temporary, "existing temporary file")?;
            assert!(settings.save(&path).is_err());
            assert_eq!(fs::read_to_string(&path)?, r#"{"recent_items_limit":-1}"#);
            assert_eq!(fs::read_to_string(&temporary)?, "existing temporary file");
            assert!(
                Settings::path()?
                    .ends_with(Path::new(env!("CARGO_PKG_NAME")).join("settings.json"))
            );
            Ok(())
        })();
        fs::remove_dir_all(directory)?;
        result
    }
}
