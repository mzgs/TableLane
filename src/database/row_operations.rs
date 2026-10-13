use super::*;

#[derive(Clone, Copy)]
pub(crate) enum RowChange<'a> {
    Insert(&'a [Option<Option<String>>]),
    Delete(&'a [Option<String>]),
}

impl Session {
    pub(super) async fn validate_row_write(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::MySql>,
        database: &str,
        table: &str,
        schema: &TableSchema,
    ) -> Result<(), String> {
        let target = format!("{}.{}", quote_identifier(database), quote_identifier(table));
        // Hold a metadata lock until commit so concurrent DDL cannot change validation.
        sqlx::query(&format!("SELECT * FROM {target} LIMIT 0"))
            .fetch_all(&mut **transaction)
            .await
            .map_err(|error| self.database_error(error))?;
        let engine: Option<String> = sqlx::query_scalar("SELECT ENGINE FROM information_schema.TABLES WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?")
                .bind(database).bind(table).fetch_optional(&mut **transaction).await.map_err(|error| self.database_error(error))?.flatten();
        if engine.as_deref() != Some("InnoDB") {
            return Err(
                "Row operations require an InnoDB table so failures can be rolled back.".into(),
            );
        }
        let account: String = sqlx::query_scalar("SELECT CURRENT_USER()")
            .fetch_one(&mut **transaction)
            .await
            .map_err(|error| self.database_error(error))?;
        let (user, host) = account
            .rsplit_once('@')
            .ok_or("Couldn’t verify trigger visibility.")?;
        let grantee = format!("'{user}'@'{host}'");
        // shortcut: direct grants only; resolve effective roles when role-based accounts need row writes.
        let visible: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM (SELECT GRANTEE, PRIVILEGE_TYPE FROM information_schema.USER_PRIVILEGES UNION ALL SELECT GRANTEE, PRIVILEGE_TYPE FROM information_schema.SCHEMA_PRIVILEGES WHERE TABLE_SCHEMA = ? UNION ALL SELECT GRANTEE, PRIVILEGE_TYPE FROM information_schema.TABLE_PRIVILEGES WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?) p WHERE GRANTEE = ? AND PRIVILEGE_TYPE = 'TRIGGER'")
                .bind(database).bind(database).bind(table).bind(grantee).fetch_one(&mut **transaction)
                .await.map_err(|error| self.database_error(error))?;
        // A zero trigger count is trustworthy only when the account can see them.
        if visible == 0 {
            return Err("Row operations require a direct TRIGGER privilege on this table to verify that all effects can roll back.".into());
        }
        let triggers: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM information_schema.TRIGGERS WHERE EVENT_OBJECT_SCHEMA = ? AND EVENT_OBJECT_TABLE = ?")
                .bind(database).bind(table).fetch_one(&mut **transaction).await.map_err(|error| self.database_error(error))?;
        // Trigger effects can reach nontransactional tables or external services.
        if triggers != 0 {
            return Err("Row operations are unavailable for tables with triggers because their effects may not roll back.".into());
        }
        if read_schema(transaction, database, table)
            .await
            .map_err(|error| self.database_error(error))?
            != *schema
        {
            return Err("Table columns changed. Refresh the table and start again.".into());
        }
        Ok(())
    }

    pub(crate) async fn change_row(
        &self,
        database: &str,
        table: &str,
        schema: &TableSchema,
        change: RowChange<'_>,
    ) -> Result<(), String> {
        self.change_rows(database, table, schema, &[change]).await
    }

    pub(crate) async fn delete_rows(
        &self,
        database: &str,
        table: &str,
        schema: &TableSchema,
        originals: &[Vec<Option<String>>],
    ) -> Result<(), String> {
        if originals.is_empty() {
            return Err("Select rows before deleting.".into());
        }
        let mut keys = std::collections::BTreeSet::new();
        for original in originals {
            if original.len() != schema.columns.len() {
                return Err("The row no longer matches the table columns.".into());
            }
            if !keys.insert(schema.key(original)) {
                return Err("A row was selected more than once.".into());
            }
        }
        let changes = originals
            .iter()
            .map(|row| RowChange::Delete(row))
            .collect::<Vec<_>>();
        self.change_rows(database, table, schema, &changes).await
    }

    async fn change_rows(
        &self,
        database: &str,
        table: &str,
        schema: &TableSchema,
        changes: &[RowChange<'_>],
    ) -> Result<(), String> {
        let mut guard = self
            .connection()
            .await
            .map_err(|error| self.database_error(error))?;
        let connection = guard.as_mut().unwrap();
        let active: i64 = sqlx::query_scalar("SELECT @@in_transaction OR NOT @@autocommit")
            .fetch_one(&mut *connection)
            .await
            .map_err(|error| self.database_error(error))?;
        if active != 0 {
            return Err("Commit or roll back the open SQL transaction and enable autocommit before changing rows.".into());
        }
        let mut transaction = connection
            .begin()
            .await
            .map_err(|error| self.database_error(error))?;
        let result = async {
            self.validate_row_write(&mut transaction, database, table, schema)
                .await?;
            // Check and lock every original before any delete, including rows a
            // self-referencing foreign key might delete or change through a cascade.
            for change in changes {
                let RowChange::Delete(original) = change else {
                    continue;
                };
                if original.len() != schema.columns.len() {
                    return Err("The row no longer matches the table columns.".into());
                }
                let sql = format!("{} FOR UPDATE", schema.row_query(database, table)?);
                let mut statement = sqlx::query(&sql);
                for key in schema.key(original) {
                    statement = statement.bind(key);
                }
                let current = statement
                    .fetch_optional(&mut *transaction)
                    .await
                    .map_err(|error| self.database_error(error))?;
                let current = current
                    .map(|row| {
                        (0..schema.columns.len())
                            .map(|ix| row.try_get::<Option<String>, _>(ix))
                            .collect::<Result<Vec<_>, _>>()
                    })
                    .transpose()
                    .map_err(|error| self.database_error(error))?;
                if current.as_deref() != Some(*original) {
                    return Err("The row changed or was deleted. Clear the selection and refresh the table before retrying.".into());
                }
            }
            for change in changes {
                let (sql, bindings) = row_statement(database, table, schema, *change)?;
                let mut statement = sqlx::query(&sql).persistent(false);
                for value in bindings {
                    statement = statement.bind(value);
                }
                let result = statement
                    .execute(&mut *transaction)
                    .await
                    .map_err(|error| self.database_error(error))?;
                if result.rows_affected() != 1 && !matches!(change, RowChange::Delete(_)) {
                    return Err(
                        "The row changed or was deleted. Cancel and refresh the table before retrying."
                            .into(),
                    );
                }
                let warnings: u64 = sqlx::query_scalar("SHOW COUNT(*) WARNINGS")
                    .fetch_one(&mut *transaction)
                    .await
                    .map_err(|error| self.database_error(error))?;
                if warnings != 0 {
                    return Err(
                        "The database reported conversion warnings; the row operation was rolled back."
                            .into(),
                    );
                }
            }
            Ok(())
        }
        .await;
        match result {
            Ok(()) => transaction.commit().await.map_err(|error| {
                format!(
                    "Couldn’t confirm the row operation: {}. Check the table before retrying.",
                    self.database_error(error)
                )
            }),
            Err(error) => {
                transaction.rollback().await.map_err(|rollback| {
                    format!(
                        "{error} Rollback failed: {}. Check the table before retrying.",
                        self.database_error(rollback)
                    )
                })?;
                Err(error)
            }
        }
    }
}

fn row_statement(
    database: &str,
    table: &str,
    schema: &TableSchema,
    change: RowChange<'_>,
) -> Result<(String, Vec<Option<String>>), String> {
    let target = format!("{}.{}", quote_identifier(database), quote_identifier(table));
    let statement = match change {
        RowChange::Insert(values) => {
            if values.len() != schema.columns.len() {
                return Err("The row no longer matches the table columns.".into());
            }
            let mut columns = Vec::new();
            let mut parameters = Vec::new();
            let mut bindings = Vec::new();
            for (ix, value) in values.iter().enumerate() {
                let Some(value) = value else { continue };
                if !schema.is_editable(ix) {
                    return Err(format!(
                        "{} cannot be inserted. Use its database default.",
                        schema.columns[ix]
                    ));
                }
                if schema.is_binary(ix)
                    && value.as_ref().is_some_and(|value| {
                        value.len() % 2 != 0 || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
                    })
                {
                    return Err(format!(
                        "{} must contain pairs of hexadecimal digits.",
                        schema.columns[ix]
                    ));
                }
                columns.push(quote_identifier(&schema.columns[ix]));
                parameters.push(schema.parameter(ix));
                bindings.push(value.clone());
            }
            (
                format!(
                    "INSERT INTO {target} ({}) VALUES ({})",
                    columns.join(", "),
                    parameters.join(", ")
                ),
                bindings,
            )
        }
        RowChange::Delete(original) => {
            schema.row_query(database, table)?;
            if original.len() != schema.columns.len() {
                return Err("The row no longer matches the table columns.".into());
            }
            let predicates = schema
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
            let bindings = schema.key(original);
            (
                format!(
                    "DELETE FROM {target} WHERE {} LIMIT 1",
                    predicates.join(" AND ")
                ),
                bindings,
            )
        }
    };
    Ok(statement)
}

#[cfg(test)]
mod tests {
    use super::{RowChange, TableQuery};
    use crate::settings::{Connection, DatabaseKind};

    #[test]
    #[ignore = "requires an isolated MariaDB server and TABLELANE_TEST_MARIADB_PORT"]
    fn mariadb_insert_duplicate_delete_validate_defaults_conflicts_and_rollback() {
        async_std::task::block_on(async {
            let mut config = Connection {
                name: "Row operations".into(),
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
            let session = super::super::connect(&config).await.unwrap();
            let database = "row_change_test";
            session
                .run_sql("mysql", "CREATE DATABASE row_change_test")
                .await
                .unwrap();
            let table = "rows`data";
            session.run_sql(database, "CREATE TABLE `rows``data` (id BIGINT PRIMARY KEY AUTO_INCREMENT, body LONGTEXT NOT NULL DEFAULT 'fallback', bytes BLOB, nullable TEXT, empty TEXT, short VARCHAR(4) DEFAULT 'auto', computed BIGINT AS (id + 1) VIRTUAL) ENGINE=InnoDB").await.unwrap();
            let schema = session
                .read_table(database, table, &TableQuery::default())
                .await
                .unwrap()
                .schema;
            assert_eq!(schema.auto_increment, vec![0]);
            assert_eq!(schema.generated_columns, vec![6]);
            session
                .change_row(database, table, &schema, RowChange::Insert(&vec![None; 7]))
                .await
                .unwrap();
            let original = session
                .read_row(database, table, &schema, &[Some("1".into())])
                .await
                .unwrap();
            assert_eq!(
                original,
                vec![
                    Some("1".into()),
                    Some("fallback".into()),
                    None,
                    None,
                    None,
                    Some("auto".into()),
                    Some("2".into())
                ]
            );
            let values = vec![
                None,
                Some(Some("世界".repeat(500))),
                Some(Some("00FF".into())),
                Some(None),
                Some(Some(String::new())),
                None,
                None,
            ];
            session
                .change_row(database, table, &schema, RowChange::Insert(&values))
                .await
                .unwrap();
            let full = session
                .read_row(database, table, &schema, &[Some("2".into())])
                .await
                .unwrap();
            assert_eq!(full[1], Some("世界".repeat(500)));
            assert_eq!(full[2].as_deref(), Some("00FF"));
            assert_eq!(full[3], None);
            assert_eq!(full[4].as_deref(), Some(""));
            let mut duplicate = full.iter().cloned().map(Some).collect::<Vec<_>>();
            duplicate[0] = None;
            duplicate[6] = None;
            session
                .change_row(database, table, &schema, RowChange::Insert(&duplicate))
                .await
                .unwrap();
            let copy = session
                .read_row(database, table, &schema, &[Some("3".into())])
                .await
                .unwrap();
            assert_eq!(full[1..6], copy[1..6]);
            for (ix, value) in [(2, "invalid"), (5, "too long")] {
                let mut invalid = values.clone();
                invalid[ix] = Some(Some(value.into()));
                assert!(
                    session
                        .change_row(database, table, &schema, RowChange::Insert(&invalid))
                        .await
                        .is_err()
                );
            }
            let mut invalid = values.clone();
            invalid[0] = Some(Some("2".into()));
            assert!(
                session
                    .change_row(database, table, &schema, RowChange::Insert(&invalid))
                    .await
                    .is_err()
            );
            invalid = values.clone();
            invalid[1] = Some(None);
            assert!(
                session
                    .change_row(database, table, &schema, RowChange::Insert(&invalid))
                    .await
                    .is_err()
            );
            invalid = values.clone();
            invalid[6] = Some(Some("8".into()));
            assert!(
                session
                    .change_row(database, table, &schema, RowChange::Insert(&invalid))
                    .await
                    .is_err()
            );
            session
                .run_sql(database, "SET sql_mode = '', max_error_count = 0")
                .await
                .unwrap();
            invalid = values.clone();
            invalid[5] = Some(Some("truncated".into()));
            assert!(
                session
                    .change_row(database, table, &schema, RowChange::Insert(&invalid))
                    .await
                    .unwrap_err()
                    .contains("warnings")
            );
            assert_eq!(
                session
                    .read_table(database, table, &TableQuery::default())
                    .await
                    .unwrap()
                    .rows
                    .len(),
                3
            );
            session
                .run_sql(database, "START TRANSACTION")
                .await
                .unwrap();
            session
                .run_sql(
                    database,
                    "INSERT INTO `rows``data` (body) VALUES ('uncommitted')",
                )
                .await
                .unwrap();
            for change in [RowChange::Insert(&values), RowChange::Delete(&original)] {
                assert!(
                    session
                        .change_row(database, table, &schema, change)
                        .await
                        .unwrap_err()
                        .contains("open SQL transaction")
                );
            }
            let (result, _) = session
                .run_sql(database, "SELECT @@in_transaction")
                .await
                .unwrap();
            assert_eq!(result.rows[0][0].as_deref(), Some("1"));
            session.run_sql(database, "ROLLBACK").await.unwrap();
            session
                .run_sql(database, "SET autocommit = 0")
                .await
                .unwrap();
            assert!(
                session
                    .change_row(database, table, &schema, RowChange::Insert(&values))
                    .await
                    .unwrap_err()
                    .contains("autocommit")
            );
            session
                .run_sql(database, "SET autocommit = 1")
                .await
                .unwrap();
            session
                .run_sql(
                    database,
                    "UPDATE `rows``data` SET body = 'FALLBACK' WHERE id = 1",
                )
                .await
                .unwrap();
            assert!(
                session
                    .change_row(database, table, &schema, RowChange::Delete(&original))
                    .await
                    .unwrap_err()
                    .contains("row changed")
            );
            let current = session
                .read_row(database, table, &schema, &[Some("1".into())])
                .await
                .unwrap();
            session
                .change_row(database, table, &schema, RowChange::Delete(&current))
                .await
                .unwrap();
            assert!(
                session
                    .change_row(database, table, &schema, RowChange::Delete(&current))
                    .await
                    .is_err()
            );
            session.run_sql(database, "CREATE TABLE related (id BIGINT PRIMARY KEY, parent BIGINT, FOREIGN KEY (parent) REFERENCES `rows``data`(id) ON DELETE CASCADE) ENGINE=InnoDB").await.unwrap();
            session
                .run_sql(database, "INSERT INTO related VALUES (1, 2)")
                .await
                .unwrap();
            session
                .change_row(database, table, &schema, RowChange::Delete(&full))
                .await
                .unwrap();
            assert!(
                session
                    .read_table(database, "related", &TableQuery::default())
                    .await
                    .unwrap()
                    .rows
                    .is_empty()
            );
            session
                .run_sql(
                    database,
                    "CREATE TABLE no_key (body TEXT DEFAULT 'default') ENGINE=InnoDB",
                )
                .await
                .unwrap();
            let no_key = session
                .read_table(database, "no_key", &TableQuery::default())
                .await
                .unwrap()
                .schema;
            session
                .change_row(database, "no_key", &no_key, RowChange::Insert(&[None]))
                .await
                .unwrap();
            assert!(
                session
                    .change_row(
                        database,
                        "no_key",
                        &no_key,
                        RowChange::Delete(&[Some("default".into())])
                    )
                    .await
                    .unwrap_err()
                    .contains("primary key")
            );
            session.run_sql(database, "CREATE TABLE composite (id INT, token VARBINARY(1000), body LONGTEXT, PRIMARY KEY (id, token)) ENGINE=InnoDB").await.unwrap();
            let composite = session
                .read_table(database, "composite", &TableQuery::default())
                .await
                .unwrap()
                .schema;
            session
                .change_row(
                    database,
                    "composite",
                    &composite,
                    RowChange::Insert(&[
                        Some(Some("7".into())),
                        Some(Some("A0FF".repeat(260))),
                        Some(Some("long".repeat(300))),
                    ]),
                )
                .await
                .unwrap();
            let row = session
                .read_row(
                    database,
                    "composite",
                    &composite,
                    &[Some("7".into()), Some("A0FF".repeat(260))],
                )
                .await
                .unwrap();
            session
                .change_row(database, "composite", &composite, RowChange::Delete(&row))
                .await
                .unwrap();
            session
                .run_sql(
                    database,
                    "CREATE TABLE nontransactional (id INT) ENGINE=MyISAM",
                )
                .await
                .unwrap();
            let nontransactional = session
                .read_table(database, "nontransactional", &TableQuery::default())
                .await
                .unwrap()
                .schema;
            assert!(
                session
                    .change_row(
                        database,
                        "nontransactional",
                        &nontransactional,
                        RowChange::Insert(&[Some(Some("1".into()))])
                    )
                    .await
                    .unwrap_err()
                    .contains("InnoDB")
            );
            session.run_sql(database, "CREATE TRIGGER row_side_effect AFTER INSERT ON `rows``data` FOR EACH ROW INSERT INTO nontransactional VALUES (NEW.id)").await.unwrap();
            assert!(
                session
                    .change_row(database, table, &schema, RowChange::Insert(&values))
                    .await
                    .unwrap_err()
                    .contains("triggers")
            );
            assert!(
                session
                    .change_row(database, table, &schema, RowChange::Delete(&copy))
                    .await
                    .unwrap_err()
                    .contains("triggers")
            );
            session
                .run_sql(database, "DROP TRIGGER row_side_effect")
                .await
                .unwrap();
            session
                .run_sql(
                    database,
                    "CREATE USER 'row_change_reader'@'%' IDENTIFIED BY ''",
                )
                .await
                .unwrap();
            session
                .run_sql(
                    database,
                    "GRANT SELECT, INSERT, DELETE ON row_change_test.* TO 'row_change_reader'@'%'",
                )
                .await
                .unwrap();
            config.username = "row_change_reader".into();
            let restricted = super::super::connect(&config).await.unwrap();
            assert!(
                restricted
                    .change_row(database, table, &schema, RowChange::Insert(&values))
                    .await
                    .unwrap_err()
                    .contains("TRIGGER privilege")
            );
            assert!(
                restricted
                    .change_row(database, table, &schema, RowChange::Delete(&copy))
                    .await
                    .unwrap_err()
                    .contains("TRIGGER privilege")
            );
            restricted.disconnect();
            session
                .run_sql(database, "DROP USER 'row_change_reader'@'%'")
                .await
                .unwrap();
            session
                .run_sql(database, "ALTER TABLE `rows``data` ADD COLUMN newer INT")
                .await
                .unwrap();
            assert!(
                session
                    .change_row(database, table, &schema, RowChange::Insert(&values))
                    .await
                    .unwrap_err()
                    .contains("columns changed")
            );
            assert!(
                session
                    .change_row(database, table, &schema, RowChange::Delete(&copy))
                    .await
                    .unwrap_err()
                    .contains("columns changed")
            );
            session
                .run_sql("mysql", "DROP DATABASE row_change_test")
                .await
                .unwrap();
        });
    }
    #[test]
    #[ignore = "requires an isolated MariaDB server and TABLELANE_TEST_MARIADB_PORT"]
    fn mariadb_bulk_delete_uses_full_composite_keys_and_rolls_back_all_rows() {
        async_std::task::block_on(async {
            let config = Connection {
                name: "Bulk operations".into(),
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
            let session = super::super::connect(&config).await.unwrap();
            let other = super::super::connect(&config).await.unwrap();
            let db = "bulk_delete_test";
            session
                .run_sql("mysql", "CREATE DATABASE bulk_delete_test")
                .await
                .unwrap();
            session.run_sql(db, "CREATE TABLE widgets (id INT, token VARBINARY(1000), body LONGTEXT, empty TEXT, PRIMARY KEY (id, token)) ENGINE=InnoDB").await.unwrap();
            session.run_sql(db, "INSERT INTO widgets VALUES (1, REPEAT(X'A0FF', 260), CONCAT(REPEAT('x', 300), 'one'), ''), (1, X'00FF', NULL, ''), (2, X'00FF', CONCAT(REPEAT('x', 300), 'two'), NULL)").await.unwrap();
            let data = session
                .read_table(db, "widgets", &TableQuery::default())
                .await
                .unwrap();
            let mut originals = Vec::new();
            for key in &data.keys {
                originals.push(
                    session
                        .read_row(db, "widgets", &data.schema, key)
                        .await
                        .unwrap(),
                );
            }
            assert!(
                session
                    .delete_rows(db, "widgets", &data.schema, &[])
                    .await
                    .is_err()
            );
            assert!(
                session
                    .delete_rows(
                        db,
                        "widgets",
                        &data.schema,
                        &[originals[0].clone(), originals[0].clone()]
                    )
                    .await
                    .unwrap_err()
                    .contains("more than once")
            );
            // A suffix hidden by the preview changes after selection. Earlier deletes must roll back.
            other
                .run_sql(
                    db,
                    "UPDATE widgets SET body = CONCAT(body, 'changed') WHERE id = 2",
                )
                .await
                .unwrap();
            assert!(
                session
                    .delete_rows(db, "widgets", &data.schema, &originals)
                    .await
                    .unwrap_err()
                    .contains("row changed")
            );
            assert_eq!(
                session
                    .read_table(db, "widgets", &TableQuery::default())
                    .await
                    .unwrap()
                    .rows
                    .len(),
                3
            );
            originals[2] = session
                .read_row(db, "widgets", &data.schema, &data.keys[2])
                .await
                .unwrap();
            session.run_sql(db, "CREATE TABLE related (parent INT, token VARBINARY(1000), FOREIGN KEY (parent, token) REFERENCES widgets(id, token)) ENGINE=InnoDB").await.unwrap();
            session
                .run_sql(db, "INSERT INTO related VALUES (2, X'00FF')")
                .await
                .unwrap();
            assert!(
                session
                    .delete_rows(db, "widgets", &data.schema, &originals)
                    .await
                    .is_err()
            );
            assert_eq!(
                session
                    .read_table(db, "widgets", &TableQuery::default())
                    .await
                    .unwrap()
                    .rows
                    .len(),
                3
            );
            session.run_sql(db, "DROP TABLE related").await.unwrap();
            session.run_sql(db, "START TRANSACTION").await.unwrap();
            assert!(
                session
                    .delete_rows(db, "widgets", &data.schema, &originals)
                    .await
                    .unwrap_err()
                    .contains("open SQL transaction")
            );
            assert_eq!(
                session
                    .run_sql(db, "SELECT @@in_transaction")
                    .await
                    .unwrap()
                    .0
                    .rows[0][0]
                    .as_deref(),
                Some("1")
            );
            session.run_sql(db, "ROLLBACK").await.unwrap();
            other
                .run_sql(db, "DELETE FROM widgets WHERE id = 2")
                .await
                .unwrap();
            assert!(
                session
                    .delete_rows(db, "widgets", &data.schema, &originals)
                    .await
                    .unwrap_err()
                    .contains("row changed")
            );
            assert_eq!(
                session
                    .read_table(db, "widgets", &TableQuery::default())
                    .await
                    .unwrap()
                    .rows
                    .len(),
                2
            );
            session
                .delete_rows(db, "widgets", &data.schema, &originals[..2])
                .await
                .unwrap();
            assert!(
                session
                    .read_table(db, "widgets", &TableQuery::default())
                    .await
                    .unwrap()
                    .rows
                    .is_empty()
            );
            for rule in ["CASCADE", "SET NULL"] {
                session.run_sql(db, &format!("CREATE TABLE tree (id INT PRIMARY KEY, parent INT, body TEXT, FOREIGN KEY (parent) REFERENCES tree(id) ON DELETE {rule}) ENGINE=InnoDB")).await.unwrap();
                session
                    .run_sql(
                        db,
                        "INSERT INTO tree VALUES (1, NULL, 'parent'), (2, 1, 'child')",
                    )
                    .await
                    .unwrap();
                let data = session
                    .read_table(db, "tree", &TableQuery::default())
                    .await
                    .unwrap();
                session
                    .delete_rows(db, "tree", &data.schema, &data.rows)
                    .await
                    .unwrap();
                assert!(
                    session
                        .read_table(db, "tree", &TableQuery::default())
                        .await
                        .unwrap()
                        .rows
                        .is_empty()
                );
                session.run_sql(db, "DROP TABLE tree").await.unwrap();
            }
            session
                .run_sql("mysql", "DROP DATABASE bulk_delete_test")
                .await
                .unwrap();
        });
    }
}
