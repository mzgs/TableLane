use crate::settings::{Connection, DatabaseKind};
use sqlx::{Connection as _, MySqlConnection, Row};
use std::collections::BTreeMap;

const CELL_PREVIEW_CHARS: usize = 200;

pub(crate) struct Session {
    connection: async_std::sync::Mutex<MySqlConnection>,
    pub(crate) databases: BTreeMap<String, Vec<String>>,
}

#[non_exhaustive]
pub(crate) struct TableRows {
    pub(crate) columns: Vec<String>,
    pub(crate) rows: Vec<Vec<Option<String>>>,
}

fn quote_identifier(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

fn truncate_preview(mut value: String) -> String {
    if let Some((end, _)) = value.char_indices().nth(CELL_PREVIEW_CHARS) {
        value.truncate(end);
        value.push('…');
    }
    value
}

impl Session {
    pub(crate) async fn read_table(
        &self,
        database: &str,
        table: &str,
    ) -> Result<TableRows, String> {
        async_std::future::timeout(std::time::Duration::from_secs(15), async {
            let mut connection = self.connection.lock().await;
            let metadata = sqlx::query(
                "SELECT COLUMN_NAME, DATA_TYPE FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? ORDER BY ORDINAL_POSITION",
            ).bind(database).bind(table).fetch_all(&mut *connection).await?;
            let mut columns = Vec::new();
            let mut expressions = Vec::new();
            let text_limit = CELL_PREVIEW_CHARS + 1;
            let binary_limit = CELL_PREVIEW_CHARS / 2 + 1;
            for column in metadata {
                let name: String = column.try_get(0)?;
                let kind: String = column.try_get(1)?;
                let quoted = quote_identifier(&name);
                let expression = match kind.as_str() {
                    "binary" | "varbinary" | "tinyblob" | "blob" | "mediumblob" | "longblob" | "bit" | "geometry" | "point" | "linestring" | "polygon" | "multipoint" | "multilinestring" | "multipolygon" | "geometrycollection" => format!("HEX(LEFT({quoted}, {binary_limit}))"),
                    _ => format!("CAST(LEFT({quoted}, {text_limit}) AS CHAR CHARACTER SET utf8mb4)"),
                };
                columns.push(name);
                expressions.push(expression);
            }
            if columns.is_empty() {
                return Err(sqlx::Error::Protocol("Table is unavailable or has no accessible columns.".into()));
            }
            // shortcut: preview is capped at 1,000 rows, add pagination when browsing beyond the preview is needed.
            let query = format!("SELECT {} FROM {}.{} LIMIT 1000", expressions.join(", "), quote_identifier(database), quote_identifier(table));
            let rows = sqlx::query(&query).fetch_all(&mut *connection).await?
                .iter().map(|row| (0..columns.len()).map(|ix| row.try_get::<Option<String>, _>(ix).map(|value| value.map(truncate_preview))).collect())
                .collect::<Result<Vec<Vec<Option<String>>>, sqlx::Error>>()?;
            Ok::<_, sqlx::Error>(TableRows { columns, rows })
        }).await.map_err(|_| "Loading table timed out. Double-click the table to try again.".to_owned())?
            .map_err(|error| format!("Couldn’t load table: {error}. Double-click the table to try again."))
    }
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
    async_std::future::timeout(std::time::Duration::from_secs(15), async {
        let mut session = MySqlConnection::connect(url.as_str()).await?;
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
        Ok::<_, sqlx::Error>(Session { connection: async_std::sync::Mutex::new(session), databases })
    }).await.map_err(|_| "Connection timed out. Check the host and port, then try again.".to_owned())?
        .map_err(|error| format!("Couldn’t connect or load databases: {error}"))
}

#[cfg(test)]
mod tests {
    use super::connection_url;
    use crate::settings::{Connection, DatabaseKind};

    #[test]
    fn cell_previews_preserve_short_values_and_truncate_on_unicode_boundaries() {
        for value in [String::new(), "Short text".into(), "ع".repeat(200)] {
            assert_eq!(super::truncate_preview(value.clone()), value);
        }
        assert_eq!(
            super::truncate_preview("ع".repeat(201)),
            format!("{}…", "ع".repeat(200))
        );
        assert_eq!(
            super::truncate_preview("FF".repeat(101)),
            format!("{}…", "FF".repeat(100))
        );
    }

    #[test]
    fn identifiers_escape_backticks_and_keep_qualified_names_separate() {
        assert_eq!(super::quote_identifier("a`b/table"), "`a``b/table`");
        assert_eq!(super::quote_identifier("schema.table"), "`schema.table`");
    }

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
        let rows =
            async_std::task::block_on(session.read_table("tablelane_test", "widgets")).unwrap();
        assert!(!rows.columns.is_empty());
        assert!(
            async_std::task::block_on(session.read_table("tablelane_test", "missing_table"))
                .is_err()
        );
        async_std::task::block_on(async {
            let mut connection = session.connection.lock().await;
            for query in [
                "CREATE DATABASE `preview``test`",
                "CREATE TABLE `preview``test`.`types``/table` (`number` BIGINT UNSIGNED, `amount` DECIMAL(20,4), `date` DATETIME, `text``value` LONGTEXT, `bytes` LONGBLOB, `nullable` TEXT)",
                "INSERT INTO `preview``test`.`types``/table` VALUES (18446744073709551615, 123456789.1234, '2026-10-09 12:34:56', 'hello 世界', X'00FF', NULL)",
            ] {
                sqlx::query(query).execute(&mut *connection).await.unwrap();
            }
            drop(connection);
            let rows = session
                .read_table("preview`test", "types`/table")
                .await
                .unwrap();
            assert_eq!(
                rows.columns,
                [
                    "number",
                    "amount",
                    "date",
                    "text`value",
                    "bytes",
                    "nullable"
                ]
            );
            assert_eq!(
                rows.rows[0],
                vec![
                    Some("18446744073709551615".into()),
                    Some("123456789.1234".into()),
                    Some("2026-10-09 12:34:56".into()),
                    Some("hello 世界".into()),
                    Some("00FF".into()),
                    None
                ]
            );
            let mut connection = session.connection.lock().await;
            sqlx::query("UPDATE `preview``test`.`types``/table` SET `text``value` = REPEAT('ع', 100000), `bytes` = REPEAT(X'FF', 100000)")
                .execute(&mut *connection).await.unwrap();
            drop(connection);
            let preview = session
                .read_table("preview`test", "types`/table")
                .await
                .unwrap();
            assert_eq!(preview.rows[0][3], Some(format!("{}…", "ع".repeat(200))));
            assert_eq!(preview.rows[0][4], Some(format!("{}…", "FF".repeat(100))));
            assert_eq!(preview.rows[0][5], None);
            let mut connection = session.connection.lock().await;
            sqlx::query("UPDATE `preview``test`.`types``/table` SET `text``value` = 'hello 世界', `bytes` = X'00FF'")
                .execute(&mut *connection).await.unwrap();
            for _ in 0..10 {
                sqlx::query("INSERT INTO `preview``test`.`types``/table` SELECT * FROM `preview``test`.`types``/table`").execute(&mut *connection).await.unwrap();
            }
            drop(connection);
            assert_eq!(
                session
                    .read_table("preview`test", "types`/table")
                    .await
                    .unwrap()
                    .rows
                    .len(),
                1000
            );
            let mut connection = session.connection.lock().await;
            sqlx::query("TRUNCATE TABLE `preview``test`.`types``/table`")
                .execute(&mut *connection)
                .await
                .unwrap();
            drop(connection);
            let empty = session
                .read_table("preview`test", "types`/table")
                .await
                .unwrap();
            assert_eq!(empty.columns.len(), 6);
            assert!(empty.rows.is_empty());
            let mut connection = session.connection.lock().await;
            sqlx::query("DROP DATABASE `preview``test`")
                .execute(&mut *connection)
                .await
                .unwrap();
        });
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
