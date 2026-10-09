use crate::settings::{Connection, DatabaseKind};
use sqlx::{Connection as _, MySqlConnection, Row};
use std::collections::BTreeMap;

const CELL_PREVIEW_CHARS: usize = 200;

pub(crate) struct Session {
    connection: async_std::sync::Mutex<MySqlConnection>,
    pub(crate) databases: BTreeMap<String, Vec<String>>,
}

#[non_exhaustive]
#[derive(Clone, Default)]
pub(crate) struct TableSchema {
    pub(crate) columns: Vec<String>,
    pub(crate) column_types: Vec<String>,
    pub(crate) primary_key: Vec<usize>,
    pub(crate) generated_columns: Vec<usize>,
}

#[non_exhaustive]
pub(crate) struct TableRows {
    pub(crate) schema: TableSchema,
    pub(crate) rows: Vec<Vec<Option<String>>>,
    pub(crate) keys: Vec<Vec<Option<String>>>,
}

impl TableSchema {
    fn kind(&self, ix: usize) -> &str {
        self.column_types[ix].split(['(', ' ']).next().unwrap_or("")
    }

    pub(crate) fn is_binary(&self, ix: usize) -> bool {
        matches!(
            self.kind(ix),
            "binary"
                | "varbinary"
                | "tinyblob"
                | "blob"
                | "mediumblob"
                | "longblob"
                | "bit"
                | "geometry"
                | "point"
                | "linestring"
                | "polygon"
                | "multipoint"
                | "multilinestring"
                | "multipolygon"
                | "geometrycollection"
        )
    }

    pub(crate) fn is_editable(&self, ix: usize) -> bool {
        !self.generated_columns.contains(&ix)
            && !matches!(
                self.kind(ix),
                "geometry"
                    | "point"
                    | "linestring"
                    | "polygon"
                    | "multipoint"
                    | "multilinestring"
                    | "multipolygon"
                    | "geometrycollection"
                    | "bit"
            )
    }

    fn expression(&self, ix: usize, preview: bool) -> String {
        let column = quote_identifier(&self.columns[ix]);
        let column = if preview {
            let limit = if self.is_binary(ix) {
                CELL_PREVIEW_CHARS / 2 + 1
            } else {
                CELL_PREVIEW_CHARS + 1
            };
            format!("LEFT({column}, {limit})")
        } else {
            column
        };
        if self.is_binary(ix) {
            format!("HEX({column})")
        } else {
            format!("CAST({column} AS CHAR CHARACTER SET utf8mb4)")
        }
    }

    fn parameter(&self, ix: usize) -> &'static str {
        if self.is_binary(ix) { "UNHEX(?)" } else { "?" }
    }

    pub(crate) fn key(&self, row: &[Option<String>]) -> Vec<Option<String>> {
        self.primary_key.iter().map(|ix| row[*ix].clone()).collect()
    }

    fn row_query(&self, database: &str, table: &str) -> Result<String, String> {
        if self.primary_key.is_empty() {
            return Err("Editing requires a table with a primary key.".into());
        }
        let columns = (0..self.columns.len())
            .map(|ix| self.expression(ix, false))
            .collect::<Vec<_>>();
        let predicates = self
            .primary_key
            .iter()
            .map(|ix| {
                format!(
                    "{} <=> {}",
                    quote_identifier(&self.columns[*ix]),
                    self.parameter(*ix)
                )
            })
            .collect::<Vec<_>>();
        Ok(format!(
            "SELECT {} FROM {}.{} WHERE {} LIMIT 1",
            columns.join(", "),
            quote_identifier(database),
            quote_identifier(table),
            predicates.join(" AND ")
        ))
    }
}

fn quote_identifier(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

pub(crate) fn truncate_preview(mut value: String) -> String {
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
                "SELECT COLUMN_NAME, COLUMN_TYPE, COLUMN_KEY, EXTRA FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? ORDER BY ORDINAL_POSITION",
            ).bind(database).bind(table).fetch_all(&mut *connection).await?;
            let mut schema = TableSchema::default();
            for column in metadata {
                let ix = schema.columns.len();
                schema.columns.push(column.try_get(0)?);
                schema.column_types.push(column.try_get(1)?);
                if column.try_get::<String, _>(2)? == "PRI" { schema.primary_key.push(ix); }
                if column.try_get::<String, _>(3)?.contains("GENERATED") { schema.generated_columns.push(ix); }
            }
            if schema.columns.is_empty() {
                return Err(sqlx::Error::Protocol("Table is unavailable or has no accessible columns.".into()));
            }
            let mut expressions = (0..schema.columns.len()).map(|ix| schema.expression(ix, true)).collect::<Vec<_>>();
            expressions.extend(schema.primary_key.iter().map(|ix| schema.expression(*ix, false)));
            // shortcut: preview is capped at 1,000 rows, add pagination when browsing beyond the preview is needed.
            let query = format!("SELECT {} FROM {}.{} LIMIT 1000", expressions.join(", "), quote_identifier(database), quote_identifier(table));
            let records = sqlx::query(&query).fetch_all(&mut *connection).await?;
            let rows = records.iter().map(|row| (0..schema.columns.len()).map(|ix| row.try_get::<Option<String>, _>(ix).map(|value| value.map(truncate_preview))).collect())
                .collect::<Result<Vec<Vec<Option<String>>>, sqlx::Error>>()?;
            let keys = records.iter().map(|row| (schema.columns.len()..expressions.len()).map(|ix| row.try_get::<Option<String>, _>(ix)).collect()).collect::<Result<Vec<Vec<Option<String>>>, sqlx::Error>>()?;
            Ok::<_, sqlx::Error>(TableRows { schema, rows, keys })
        }).await.map_err(|_| "Loading table timed out. Double-click the table to try again.".to_owned())?
            .map_err(|error| format!("Couldn’t load table: {error}. Double-click the table to try again."))
    }

    pub(crate) async fn read_row(
        &self,
        database: &str,
        table: &str,
        schema: &TableSchema,
        key: &[Option<String>],
    ) -> Result<Vec<Option<String>>, String> {
        let query = schema.row_query(database, table)?;
        if key.len() != schema.primary_key.len() {
            return Err("The row key is unavailable.".into());
        }
        let mut statement = sqlx::query(&query);
        for value in key {
            statement = statement.bind(value);
        }
        let mut connection = self.connection.lock().await;
        let row = statement
            .fetch_optional(&mut *connection)
            .await
            .map_err(|error| format!("Couldn’t load row: {error}"))?
            .ok_or("This row no longer exists. Reopen the table to refresh it.")?;
        (0..schema.columns.len())
            .map(|ix| row.try_get(ix).map_err(|error| error.to_string()))
            .collect()
    }

    pub(crate) async fn save_row(
        &self,
        database: &str,
        table: &str,
        schema: &TableSchema,
        original: &[Option<String>],
        values: &[Option<String>],
    ) -> Result<Vec<Option<String>>, String> {
        let row_query = schema.row_query(database, table)?;
        if original.len() != schema.columns.len() || values.len() != original.len() {
            return Err("The row no longer matches the table columns.".into());
        }
        let changed = (0..values.len())
            .filter(|ix| values[*ix] != original[*ix])
            .collect::<Vec<_>>();
        if changed.is_empty() {
            return Ok(original.to_vec());
        }
        for ix in &changed {
            if !schema.is_editable(*ix) {
                return Err(format!("{} cannot be edited.", schema.columns[*ix]));
            }
            if schema.is_binary(*ix)
                && values[*ix].as_ref().is_some_and(|value| {
                    value.len() % 2 != 0 || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
            {
                return Err(format!(
                    "{} must contain pairs of hexadecimal digits.",
                    schema.columns[*ix]
                ));
            }
        }
        let assignments = changed
            .iter()
            .map(|ix| {
                format!(
                    "{} = {}",
                    quote_identifier(&schema.columns[*ix]),
                    schema.parameter(*ix)
                )
            })
            .collect::<Vec<_>>();
        let mut predicates = schema
            .primary_key
            .iter()
            .map(|ix| {
                format!(
                    "{} <=> {}",
                    quote_identifier(&schema.columns[*ix]),
                    schema.parameter(*ix)
                )
            })
            .collect::<Vec<_>>();
        // Compare the full original values so saving cannot overwrite somebody else's changes.
        predicates.extend(
            (0..original.len())
                .map(|ix| format!("BINARY ({}) <=> BINARY (?)", schema.expression(ix, false))),
        );
        let query = format!(
            "UPDATE {}.{} SET {} WHERE {} LIMIT 1",
            quote_identifier(database),
            quote_identifier(table),
            assignments.join(", "),
            predicates.join(" AND ")
        );
        let mut connection = self.connection.lock().await;
        let engine: Option<String> = sqlx::query_scalar("SELECT ENGINE FROM information_schema.TABLES WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?").bind(database).bind(table).fetch_optional(&mut *connection).await.map_err(|error| error.to_string())?.flatten();
        if engine.as_deref() != Some("InnoDB") {
            return Err(
                "Saving requires an InnoDB table so failed saves can be rolled back.".into(),
            );
        }
        let mut transaction = connection
            .begin()
            .await
            .map_err(|error| error.to_string())?;
        let result = async {
            let mut statement = sqlx::query(&query);
            for ix in &changed {
                statement = statement.bind(&values[*ix]);
            }
            for ix in &schema.primary_key {
                statement = statement.bind(&original[*ix]);
            }
            for value in original {
                statement = statement.bind(value);
            }
            let result = statement
                .execute(&mut *transaction)
                .await
                .map_err(|error| format!("Couldn’t save row: {error}"))?;
            if result.rows_affected() != 1 {
                return Err(
                    "The row changed or was deleted. Cancel and reopen the table before saving."
                        .to_owned(),
                );
            }
            let warnings = sqlx::query("SHOW WARNINGS")
                .fetch_all(&mut *transaction)
                .await
                .map_err(|error| error.to_string())?;
            if let Some(warning) = warnings.first() {
                return Err(format!(
                    "Save cancelled: {}",
                    warning
                        .try_get::<String, _>(2)
                        .map_err(|error| error.to_string())?
                ));
            }
            let mut statement = sqlx::query(&row_query);
            for ix in &schema.primary_key {
                statement = statement.bind(&values[*ix]);
            }
            let row = statement
                .fetch_optional(&mut *transaction)
                .await
                .map_err(|error| error.to_string())?
                .ok_or("Couldn’t verify the saved row; changes were rolled back.")?;
            (0..schema.columns.len())
                .map(|ix| row.try_get(ix).map_err(|error| error.to_string()))
                .collect::<Result<Vec<Option<String>>, String>>()
        }
        .await;
        match result {
            Ok(row) => {
                transaction.commit().await.map_err(|error| format!("Couldn’t confirm the save: {error}. Reselect the row to check its current values."))?;
                Ok(row)
            }
            Err(error) => {
                transaction
                    .rollback()
                    .await
                    .map_err(|rollback| format!("{error} Rollback failed: {rollback}"))?;
                Err(error)
            }
        }
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
    #[ignore = "requires a temporary MariaDB server and TABLELANE_TEST_MARIADB_PORT"]
    fn mariadb_row_edits_save_full_values_and_reject_conflicts() {
        let config = Connection {
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
            database: String::new(),
            file_path: String::new(),
        };
        async_std::task::block_on(async {
            let session = super::connect(&config).await.unwrap();
            let mut connection = session.connection.lock().await;
            sqlx::query("CREATE DATABASE row_editor_test")
                .execute(&mut *connection)
                .await
                .unwrap();
            sqlx::query("CREATE TABLE row_editor_test.`edit``rows` (id INT, token VARCHAR(300), body LONGTEXT, nullable TEXT, bytes BLOB, short VARCHAR(8), computed INT AS (id + 1) STORED, PRIMARY KEY (id, token)) ENGINE=InnoDB").execute(&mut *connection).await.unwrap();
            sqlx::query("INSERT INTO row_editor_test.`edit``rows` (id, token, body, nullable, bytes, short) VALUES (1, ?, ?, NULL, X'00FF', 'old')").bind("ع".repeat(260)).bind("世界".repeat(500)).execute(&mut *connection).await.unwrap();
            drop(connection);
            let data = session
                .read_table("row_editor_test", "edit`rows")
                .await
                .unwrap();
            assert_eq!(data.keys[0][1], Some("ع".repeat(260)));
            assert!(data.rows[0][2].as_ref().unwrap().ends_with('…'));
            let original = session
                .read_row("row_editor_test", "edit`rows", &data.schema, &data.keys[0])
                .await
                .unwrap();
            assert_eq!(original[2], Some("世界".repeat(500)));
            let mut values = original.clone();
            values[3] = Some(String::new());
            values[4] = Some("A0FF".into());
            values[5] = Some("new".into());
            let saved = session
                .save_row(
                    "row_editor_test",
                    "edit`rows",
                    &data.schema,
                    &original,
                    &values,
                )
                .await
                .unwrap();
            assert_eq!(saved[2], original[2]);
            assert_eq!(saved[3], Some(String::new()));
            assert_eq!(saved[4], Some("A0FF".into()));
            let mut values = saved.clone();
            values[0] = Some("2".into());
            values[1] = Some("quote' ; --".into());
            values[2] = Some("'; DROP TABLE widgets; --".into());
            let saved = session
                .save_row(
                    "row_editor_test",
                    "edit`rows",
                    &data.schema,
                    &saved,
                    &values,
                )
                .await
                .unwrap();
            assert_eq!(saved[2], values[2]);
            assert_eq!(saved[6], Some("3".into()));
            let mut invalid = saved.clone();
            invalid[4] = Some("XYZ".into());
            assert!(
                session
                    .save_row(
                        "row_editor_test",
                        "edit`rows",
                        &data.schema,
                        &saved,
                        &invalid
                    )
                    .await
                    .unwrap_err()
                    .contains("hexadecimal")
            );
            invalid = saved.clone();
            invalid[6] = Some("4".into());
            assert!(
                session
                    .save_row(
                        "row_editor_test",
                        "edit`rows",
                        &data.schema,
                        &saved,
                        &invalid
                    )
                    .await
                    .unwrap_err()
                    .contains("cannot be edited")
            );
            let mut connection = session.connection.lock().await;
            sqlx::query("SET SESSION sql_mode = ''")
                .execute(&mut *connection)
                .await
                .unwrap();
            drop(connection);
            invalid = saved.clone();
            invalid[5] = Some("too long to fit".into());
            assert!(
                session
                    .save_row(
                        "row_editor_test",
                        "edit`rows",
                        &data.schema,
                        &saved,
                        &invalid
                    )
                    .await
                    .unwrap_err()
                    .contains("Save cancelled")
            );
            let reread = session
                .read_row(
                    "row_editor_test",
                    "edit`rows",
                    &data.schema,
                    &data.schema.key(&saved),
                )
                .await
                .unwrap();
            assert_eq!(reread, saved);
            let mut connection = session.connection.lock().await;
            sqlx::query("UPDATE row_editor_test.`edit``rows` SET short = 'external'")
                .execute(&mut *connection)
                .await
                .unwrap();
            drop(connection);
            values = saved.clone();
            values[2] = Some("overwrite".into());
            assert!(
                session
                    .save_row(
                        "row_editor_test",
                        "edit`rows",
                        &data.schema,
                        &saved,
                        &values
                    )
                    .await
                    .unwrap_err()
                    .contains("row changed")
            );
            let reread = session
                .read_row(
                    "row_editor_test",
                    "edit`rows",
                    &data.schema,
                    &data.schema.key(&saved),
                )
                .await
                .unwrap();
            assert_eq!(reread[2], saved[2]);
            assert_eq!(reread[5], Some("external".into()));
            let mut no_key = data.schema.clone();
            no_key.primary_key.clear();
            assert!(
                session
                    .save_row("row_editor_test", "edit`rows", &no_key, &saved, &values)
                    .await
                    .unwrap_err()
                    .contains("primary key")
            );
            sqlx::query("DROP DATABASE row_editor_test")
                .execute(&mut *session.connection.lock().await)
                .await
                .unwrap();
        });
    }

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
        assert!(!rows.schema.columns.is_empty());
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
                rows.schema.columns,
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
            assert!(rows.schema.column_types[0].starts_with("bigint"));
            assert!(rows.schema.column_types[0].ends_with("unsigned"));
            assert_eq!(rows.schema.column_types[1], "decimal(20,4)");
            assert_eq!(
                &rows.schema.column_types[2..],
                ["datetime", "longtext", "longblob", "text"]
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
            assert_eq!(empty.schema.columns.len(), 6);
            assert_eq!(empty.schema.column_types, rows.schema.column_types);
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
