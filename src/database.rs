use crate::settings::{Connection, DatabaseKind};
use sqlx::{AnyConnection, Connection as _, Row};
use std::collections::BTreeMap;

pub(crate) struct Session {
    // shortcut: status reflects the last successful connection, add health checks when querying is introduced.
    _connection: AnyConnection,
    pub(crate) databases: BTreeMap<String, Vec<String>>,
}

fn connection_url(connection: &Connection) -> Result<url::Url, String> {
    connection.validate()?;
    if connection.database_type != DatabaseKind::MariaDB {
        return Err("Only MariaDB connections are supported yet.".into());
    }
    let mut url = url::Url::parse("mysql://localhost").unwrap();
    url.set_host(Some(&connection.host))
        .map_err(|_| "Invalid host.")?;
    url.set_port(connection.port).map_err(|_| "Invalid port.")?;
    // URL setters leave '%' untouched; SQLx percent-decodes credentials.
    url.set_username(&connection.username.replace('%', "%25"))
        .map_err(|_| "Invalid username.")?;
    url.set_password(Some(&connection.password.replace('%', "%25")))
        .map_err(|_| "Invalid password.")?;
    if !connection.database.is_empty() {
        url.path_segments_mut()
            .map_err(|_| "Invalid database.")?
            .push(&connection.database);
    }
    Ok(url)
}

pub(crate) async fn connect(connection: &Connection) -> Result<Session, String> {
    let url = connection_url(connection)?;
    sqlx::any::install_default_drivers();
    async_std::future::timeout(std::time::Duration::from_secs(15), async {
        let mut session = AnyConnection::connect(url.as_str()).await?;
        let mut databases = BTreeMap::new();
        for row in sqlx::query("SHOW DATABASES").fetch_all(&mut session).await? {
            databases.insert(row.try_get::<String, _>(0)?, Vec::new());
        }
        for row in sqlx::query(
            "SELECT TABLE_SCHEMA, TABLE_NAME FROM information_schema.TABLES ORDER BY TABLE_SCHEMA, TABLE_NAME",
        ).fetch_all(&mut session).await? {
            let schema: String = row.try_get(0)?;
            if let Some(tables) = databases.get_mut(&schema) {
                tables.push(row.try_get(1)?);
            }
        }
        Ok::<_, sqlx::Error>(Session { _connection: session, databases })
    }).await.map_err(|_| "Connection timed out. Check the host and port, then try again.".to_owned())?
        .map_err(|error| format!("Couldn’t connect or load databases: {error}"))
}

#[cfg(test)]
mod tests {
    use super::connection_url;
    use crate::settings::{Connection, DatabaseKind};

    #[test]
    #[ignore = "requires a temporary MariaDB server and TABLELANE_TEST_MARIADB_PORT"]
    fn mariadb_lists_databases_tables_and_rejects_bad_credentials() {
        let mut connection = Connection {
            name: "Test".into(),
            database_type: DatabaseKind::MariaDB,
            host: "127.0.0.1".into(),
            port: Some(
                std::env::var("TABLELANE_TEST_MARIADB_PORT")
                    .unwrap()
                    .parse()
                    .unwrap(),
            ),
            username: "root".into(),
            password: String::new(),
            database: "tablelane_test".into(),
            file_path: String::new(),
        };
        let session = async_std::task::block_on(super::connect(&connection)).unwrap();
        assert_eq!(session.databases["tablelane_test"], vec!["widgets"]);
        assert!(session.databases["empty_db"].is_empty());
        connection.password = "incorrect".into();
        assert!(async_std::task::block_on(super::connect(&connection)).is_err());
    }

    #[test]
    fn connection_urls_escape_credentials_and_reject_unsupported_drivers() {
        let mut config = Connection {
            name: "Local".into(),
            database_type: DatabaseKind::MariaDB,
            host: "localhost".into(),
            port: Some(3307),
            username: "user%2F@host".into(),
            password: "p@ss:/?#%20 word".into(),
            database: "my database%2F".into(),
            file_path: String::new(),
        };
        let url = connection_url(&config).unwrap();
        assert_eq!(url.host_str(), Some("localhost"));
        assert_eq!(url.port(), Some(3307));
        assert_eq!(url.username(), "user%252F%40host");
        assert_eq!(url.password(), Some("p%40ss%3A%2F%3F%23%2520%20word"));
        assert_eq!(url.path(), "/my%20database%252F");
        assert!(url.query().is_none());
        let options: sqlx::mysql::MySqlConnectOptions = url.as_str().parse().unwrap();
        assert_eq!(options.get_username(), config.username.as_str());
        assert_eq!(options.get_database(), Some(config.database.as_str()));
        config.database_type = DatabaseKind::MySQL;
        assert!(
            connection_url(&config)
                .unwrap_err()
                .contains("Only MariaDB")
        );
        config.database_type = DatabaseKind::MariaDB;
        config.port = Some(0);
        assert!(connection_url(&config).is_err());
    }
}
