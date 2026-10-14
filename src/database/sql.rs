use super::*;
use async_std::stream::StreamExt;
use std::sync::Arc;

#[non_exhaustive]
pub(crate) struct SqlResult {
    pub(crate) data: TableRows,
    pub(crate) affected: u64,
    pub(crate) export: Option<Arc<QueryExport>>,
}

#[non_exhaustive]
pub(crate) struct SqlRun {
    pub(crate) results: Vec<SqlResult>,
    pub(crate) error: Option<String>,
}

impl SqlResult {
    fn new(schema: TableSchema, capture: bool) -> Result<Self, sqlx::Error> {
        let export = if capture {
            let mut export = QueryExport::new().map_err(|error| {
                sqlx::Error::Protocol(format!("Couldn’t prepare result export: {error}."))
            })?;
            export.set_columns(&schema.columns);
            Some(Arc::new(export))
        } else {
            None
        };
        Ok(Self {
            data: TableRows {
                schema,
                rows: Vec::new(),
                keys: Vec::new(),
                has_more: false,
            },
            affected: 0,
            export,
        })
    }
}

impl Session {
    #[cfg(test)]
    pub(crate) async fn run_sql_export(
        &self,
        database: &str,
        sql: &str,
        cancellation: &QueryCancellation,
    ) -> Result<(TableRows, u64, QueryExport), String> {
        let mut run = self.run_sql_script(database, sql, cancellation).await?;
        if let Some(error) = run.error {
            return Err(error);
        }
        let affected = run.results.iter().map(|result| result.affected).sum();
        let first = run.results.remove(0);
        let export = Arc::try_unwrap(first.export.unwrap()).ok().unwrap();
        Ok((first.data, affected, export))
    }
    // Schema forms retain single-statement validation before any side effects.
    pub(crate) async fn run_sql(
        &self,
        database: &str,
        sql: &str,
    ) -> Result<(TableRows, u64), String> {
        if sql.trim().is_empty() {
            return Err("Enter SQL to run.".into());
        }
        let mut guard = self
            .connection()
            .await
            .map_err(|error| self.database_error(error))?;
        let connection = guard.as_mut().unwrap();
        sqlx::query(&format!("USE {}", quote_identifier(database)))
            .execute(&mut *connection)
            .await
            .map_err(|error| self.database_error(error))?;
        let mut run = self.run_statement(connection, sql, false, None).await;
        if let Some(error) = run.error {
            return Err(error);
        }
        let affected = run.results.iter().map(|result| result.affected).sum();
        let data = run.results.remove(0).data;
        Ok((data, affected))
    }

    pub(crate) async fn run_sql_script(
        &self,
        database: &str,
        sql: &str,
        cancellation: &QueryCancellation,
    ) -> Result<SqlRun, String> {
        let result = async {
            let mut guard = self
                .query_connection(Some(cancellation))
                .await
                .map_err(|error| self.database_error(error))?;
            let connection = guard.as_mut().unwrap();
            cancellation
                .check_before_execution()
                .map_err(|error| error.to_string())?;
            let use_database =
                cancellation.tagged_sql(&format!("USE {}", quote_identifier(database)));
            sqlx::query(&use_database)
                .execute(&mut *connection)
                .await
                .map_err(|error| self.database_error(error))?;
            let mut script = SqlScript::new(sql);
            let mut run = SqlRun {
                results: Vec::new(),
                error: None,
            };
            loop {
                if let Err(error) = cancellation.check_before_execution() {
                    run.error = Some(error.to_string());
                    break;
                }
                // A preceding SET can change how the next statement's quotes are parsed.
                let mode_sql = cancellation.tagged_sql("SELECT @@SESSION.sql_mode");
                let mode: String = match sqlx::query_scalar(&mode_sql)
                    .fetch_one(&mut *connection)
                    .await
                {
                    Ok(mode) => mode,
                    Err(error) => {
                        cancellation.mark_interrupted(&error);
                        run.error = Some(self.database_error(error));
                        break;
                    }
                };
                let statement = match script.next(&mode) {
                    Ok(Some(statement)) => statement,
                    Ok(None) => break,
                    Err(error) => {
                        run.error = Some(error);
                        break;
                    }
                };
                let statement_run = self
                    .run_statement(connection, statement, true, Some(cancellation))
                    .await;
                run.results.extend(statement_run.results);
                if let Some(error) = statement_run.error {
                    run.error = Some(error);
                    break;
                }
            }
            if run.results.is_empty() && run.error.is_none() {
                run.error = Some("Enter SQL to run.".into());
            }
            Ok(run)
        }
        .await;
        if let Some(status) = cancellation.finish(self).await {
            return Err(status);
        }
        if result.is_ok() && cancellation.is_requested() {
            cancellation.discard_completed();
            return Err("SQL finished after cancellation was requested. Results were discarded; check writes before retrying.".into());
        }
        result
    }

    async fn run_statement(
        &self,
        connection: &mut MySqlConnection,
        sql: &str,
        capture: bool,
        cancellation: Option<&QueryCancellation>,
    ) -> SqlRun {
        let mut results = Vec::new();
        let result = async {
            if sql.trim().is_empty() {
                return Err(sqlx::Error::Protocol("Enter SQL to run.".into()));
            }
            if let Some(cancellation) = cancellation {
                cancellation.check_before_execution()?;
            }
            let tagged = cancellation.map(|cancellation| cancellation.tagged_sql(sql));
            let sql = tagged.as_deref().unwrap_or(sql);
            let description = connection.describe(sql).await?;
            let schema = TableSchema {
                columns: description
                    .columns()
                    .iter()
                    .map(|column| column.name().to_owned())
                    .collect(),
                column_types: description
                    .columns()
                    .iter()
                    .map(|column| column.type_info().name().to_owned())
                    .collect(),
                ..TableSchema::default()
            };
            let mut current = Some(SqlResult::new(schema, capture)?);
            if let Some(cancellation) = cancellation {
                cancellation.check_before_execution()?;
                cancellation.mark_started();
            }
            let mut capture_error = None;
            let mut stream = sqlx::raw_sql(sql).fetch_many(&mut *connection);
            let mut stream_error = None;
            while let Some(item) = stream.next().await {
                let item = match item {
                    Ok(item) => item,
                    Err(error) => {
                        stream_error = Some(error);
                        break;
                    }
                };
                if cancellation.is_some_and(QueryCancellation::is_requested)
                    || capture_error.is_some()
                {
                    continue;
                }
                let captured = (|| -> Result<(), sqlx::Error> {
                    if current.is_none() {
                        current = Some(SqlResult::new(TableSchema::default(), capture)?);
                    }
                    let result = current.as_mut().unwrap();
                    match item {
                        sqlx::Either::Left(done) => {
                            result.affected = done.rows_affected();
                            if let Some(export) = &mut result.export {
                                Arc::get_mut(export).unwrap().finish().map_err(|error| {
                                    sqlx::Error::Protocol(format!(
                                        "Saving full results failed: {error}."
                                    ))
                                })?;
                            }
                            results.push(current.take().unwrap());
                        }
                        sqlx::Either::Right(row) => {
                            if result.data.rows.is_empty() {
                                result.data.schema.columns = row
                                    .columns()
                                    .iter()
                                    .map(|column| column.name().to_owned())
                                    .collect();
                                result.data.schema.column_types = row
                                    .columns()
                                    .iter()
                                    .map(|column| column.type_info().name().to_owned())
                                    .collect();
                                if let Some(export) = &mut result.export {
                                    Arc::get_mut(export)
                                        .unwrap()
                                        .set_columns(&result.data.schema.columns);
                                }
                            }
                            if let Some(export) = &mut result.export {
                                Arc::get_mut(export)
                                    .unwrap()
                                    .push(&sql_values(&row, false)?)
                                    .map_err(|error| {
                                        sqlx::Error::Protocol(format!(
                                            "Saving full results failed: {error}."
                                        ))
                                    })?;
                            }
                            if result.data.rows.len() < PAGE_SIZE {
                                result.data.rows.push(sql_values(&row, true)?);
                            } else {
                                result.data.has_more = true;
                            }
                        }
                    }
                    Ok(())
                })();
                if let Err(error) = captured {
                    capture_error = Some(error);
                }
            }
            drop(stream);
            // Keep the session locked until SQLx consumes the terminal response, including errors.
            let flushed = connection.flush().await;
            flushed?;
            if let Some(error) = stream_error {
                return Err(error);
            }
            if let Some(error) = capture_error {
                return Err(error);
            }
            Ok::<_, sqlx::Error>(())
        }
        .await;
        let error = result.err().map(|error| {
            if let Some(cancellation) = cancellation {
                cancellation.mark_interrupted(&error);
            }
            format!("Couldn’t run SQL: {}", self.database_error(error))
        });
        SqlRun { results, error }
    }
}

struct SqlScript<'a> {
    sql: &'a str,
    offset: usize,
    delimiter: String,
}

impl<'a> SqlScript<'a> {
    fn new(sql: &'a str) -> Self {
        Self {
            sql,
            offset: 0,
            delimiter: ";".into(),
        }
    }

    fn next(&mut self, mode: &str) -> Result<Option<&'a str>, String> {
        let bytes = self.sql.as_bytes();
        let no_backslash = mode.split(',').any(|mode| mode == "NO_BACKSLASH_ESCAPES");
        let ansi_quotes = mode.split(',').any(|mode| mode == "ANSI_QUOTES");
        let mut start = self.offset;
        let mut ix = start;
        let mut code = false;
        while ix < bytes.len() {
            if !code && bytes[ix].is_ascii_whitespace() {
                ix += 1;
                continue;
            }
            if !code
                && bytes[ix..]
                    .get(..9)
                    .is_some_and(|word| word.eq_ignore_ascii_case(b"DELIMITER"))
                && bytes.get(ix + 9).is_some_and(u8::is_ascii_whitespace)
                && bytes[bytes[..ix]
                    .iter()
                    .rposition(|ch| *ch == b'\n')
                    .map_or(0, |at| at + 1)..ix]
                    .iter()
                    .all(u8::is_ascii_whitespace)
            {
                let end = self.sql[ix..].find('\n').map_or(bytes.len(), |at| ix + at);
                let delimiter = self.sql[ix + 9..end].trim();
                if delimiter.is_empty()
                    || delimiter
                        .chars()
                        .any(|ch| ch.is_whitespace() || matches!(ch, '\'' | '"' | '`' | '\\'))
                {
                    return Err(
                        "DELIMITER requires one nonempty token without quotes or backslashes."
                            .into(),
                    );
                }
                self.delimiter = delimiter.into();
                ix = end;
                start = ix;
                continue;
            }
            if bytes[ix..].starts_with(self.delimiter.as_bytes()) {
                self.offset = ix + self.delimiter.len();
                if code {
                    return Ok(Some(self.sql[start..ix].trim()));
                }
                ix = self.offset;
                start = ix;
                continue;
            }
            if bytes[ix] == b'#'
                || (bytes[ix..].starts_with(b"--")
                    && bytes.get(ix + 2).is_none_or(|ch| *ch <= b' '))
            {
                ix = self.sql[ix..].find('\n').map_or(bytes.len(), |at| ix + at);
                continue;
            }
            if bytes[ix..].starts_with(b"/*") {
                code |= bytes[ix..].starts_with(b"/*!") || bytes[ix..].starts_with(b"/*M!");
                ix = self.sql[ix + 2..]
                    .find("*/")
                    .map(|at| ix + 2 + at + 2)
                    .ok_or("Unclosed SQL comment. This statement was not executed.")?;
                continue;
            }
            if matches!(bytes[ix], b'\'' | b'"' | b'`') {
                code = true;
                let quote = bytes[ix];
                let escape = !no_backslash && (quote == b'\'' || (quote == b'"' && !ansi_quotes));
                ix += 1;
                loop {
                    let Some(&ch) = bytes.get(ix) else {
                        return Err("Unclosed SQL quote. This statement was not executed.".into());
                    };
                    ix += 1;
                    if escape && ch == b'\\' {
                        ix = (ix + 1).min(bytes.len());
                    } else if ch == quote {
                        if bytes.get(ix) == Some(&quote) {
                            ix += 1;
                        } else {
                            break;
                        }
                    }
                }
                continue;
            }
            code |= !bytes[ix].is_ascii_whitespace();
            ix += 1;
        }
        self.offset = bytes.len();
        Ok(code.then(|| self.sql[start..].trim()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_respect_quotes_comments_delimiters_and_current_sql_mode() {
        let sql = "-- ; ignored\nSELECT '世界; it''s', \"a;\", `b;``c`; # ; ignored\n/* ; */ SELECT 2; ; -- end";
        let mut script = SqlScript::new(sql);
        assert_eq!(
            script.next("").unwrap(),
            Some("-- ; ignored\nSELECT '世界; it''s', \"a;\", `b;``c`")
        );
        assert_eq!(
            script.next("").unwrap(),
            Some("# ; ignored\n/* ; */ SELECT 2")
        );
        assert_eq!(script.next("").unwrap(), None);

        let mut script = SqlScript::new(
            "DELIMITER $$\r\nCREATE PROCEDURE p() BEGIN SELECT ';'; SELECT 2; END$$\nDELIMITER ;\nCALL p();",
        );
        assert_eq!(
            script.next("").unwrap(),
            Some("CREATE PROCEDURE p() BEGIN SELECT ';'; SELECT 2; END")
        );
        assert_eq!(script.next("").unwrap(), Some("CALL p()"));
        assert_eq!(script.next("").unwrap(), None);

        let mut script = SqlScript::new("SELECT 'a\\'; SELECT 2;");
        assert!(script.next("").is_err());
        assert_eq!(
            script.next("NO_BACKSLASH_ESCAPES").unwrap(),
            Some("SELECT 'a\\'")
        );
        assert_eq!(
            script.next("NO_BACKSLASH_ESCAPES").unwrap(),
            Some("SELECT 2")
        );
        let mut script = SqlScript::new("SELECT \"a\\\"; SELECT 2;");
        assert!(script.next("").is_err());
        assert_eq!(script.next("ANSI_QUOTES").unwrap(), Some("SELECT \"a\\\""));
        assert_eq!(
            SqlScript::new("/*! SELECT 1 */;").next("").unwrap(),
            Some("/*! SELECT 1 */")
        );
        assert_eq!(
            SqlScript::new("SELECT 2--1; SELECT 3").next("").unwrap(),
            Some("SELECT 2--1")
        );
        for sql in [
            "SELECT 'unfinished",
            "SELECT `unfinished",
            "SELECT 1 /*unfinished",
            "DELIMITER  \nSELECT 1",
            "DELIMITER a b\nSELECT 1",
        ] {
            assert!(SqlScript::new(sql).next("").is_err(), "{sql}");
        }
    }

    #[test]
    #[ignore = "requires an isolated MariaDB server and TABLELANE_TEST_MARIADB_PORT"]
    fn mariadb_scripts_capture_each_result_stop_on_errors_and_preserve_transactions() {
        async_std::task::block_on(async {
            let session = connect(&Connection {
                name: "Scripts".into(),
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
            })
            .await
            .unwrap();
            let db = "sql_script_test";
            session
                .run_sql("mysql", "CREATE DATABASE sql_script_test")
                .await
                .unwrap();
            let run = session.run_sql_script(db, "CREATE TABLE rows_test (id INT PRIMARY KEY) ENGINE=InnoDB; INSERT INTO rows_test VALUES (1); SELECT id FROM rows_test WHERE id = 0; SELECT seq, REPEAT('世界', 300), X'00FF', NULL, '' FROM seq_1_to_1005; SELECT @script_runs := COALESCE(@script_runs, 0) + 1;", &QueryCancellation::new()).await.unwrap();
            assert!(run.error.is_none(), "{:?}", run.error);
            assert_eq!(run.results.len(), 5);
            assert_eq!(run.results[1].affected, 1);
            assert_eq!(run.results[2].data.schema.columns, ["id"]);
            assert!(run.results[2].data.rows.is_empty());
            assert_eq!(run.results[3].data.rows.len(), 1000);
            assert!(run.results[3].data.has_more);
            let export = run.results[3].export.as_ref().unwrap();
            assert_eq!(export.row_count(), 1005);
            let (file, handle) =
                crate::data_export::TemporaryFile::new(&std::env::temp_dir()).unwrap();
            drop(handle);
            export.export(file.path(), ExportFormat::Json).unwrap();
            let json: serde_json::Value =
                serde_json::from_slice(&std::fs::read(file.path()).unwrap()).unwrap();
            assert_eq!(json["rows"][1004][1], "世界".repeat(300));
            assert_eq!(json["rows"][1004][2], "00FF");
            assert!(json["rows"][1004][3].is_null());
            assert_eq!(json["rows"][1004][4], "");
            assert_eq!(
                session
                    .run_sql(db, "SELECT @script_runs")
                    .await
                    .unwrap()
                    .0
                    .rows[0][0]
                    .as_deref(),
                Some("1")
            );

            let run = session.run_sql_script(db, "START TRANSACTION; INSERT INTO rows_test VALUES (2); SELECT 7; INSERT INTO rows_test VALUES (2); INSERT INTO rows_test VALUES (3)", &QueryCancellation::new()).await.unwrap();
            assert!(run.error.unwrap().contains("Duplicate"));
            assert_eq!(run.results.len(), 3);
            let rows = session
                .run_sql(db, "SELECT @@in_transaction, COUNT(*) FROM rows_test")
                .await
                .unwrap()
                .0;
            assert_eq!(rows.rows[0], [Some("1".into()), Some("2".into())]);
            session.run_sql(db, "ROLLBACK").await.unwrap();
            let run = session.run_sql_script(db, "INSERT INTO rows_test VALUES (4); SELECT 'unfinished; INSERT INTO rows_test VALUES (5)", &QueryCancellation::new()).await.unwrap();
            assert!(run.error.unwrap().contains("Unclosed"));
            assert_eq!(run.results.len(), 1);
            assert_eq!(
                session
                    .run_sql(db, "SELECT GROUP_CONCAT(id ORDER BY id) FROM rows_test")
                    .await
                    .unwrap()
                    .0
                    .rows[0][0]
                    .as_deref(),
                Some("1,4")
            );

            let run = session.run_sql_script(db, "DELIMITER $$\nCREATE PROCEDURE multi_results() BEGIN SELECT 10 AS first_value; SELECT 'two;世界' AS second_value; SELECT 3 WHERE FALSE; END$$\nDELIMITER ;\nCALL multi_results(); SELECT 99;", &QueryCancellation::new()).await.unwrap();
            assert!(run.error.is_none(), "{:?}", run.error);
            assert_eq!(run.results.len(), 6); // CREATE, three row sets, CALL completion, final SELECT.
            assert_eq!(run.results[1].data.rows[0][0].as_deref(), Some("10"));
            assert_eq!(run.results[2].data.rows[0][0].as_deref(), Some("two;世界"));
            assert!(run.results[3].data.rows.is_empty());
            assert_eq!(run.results[5].data.rows[0][0].as_deref(), Some("99"));
            let run = session.run_sql_script(db, "SET sql_mode = 'NO_BACKSLASH_ESCAPES'; SELECT 'a\\'; SELECT 2; SET sql_mode = DEFAULT;", &QueryCancellation::new()).await.unwrap();
            assert!(run.error.is_none(), "{:?}", run.error);
            assert_eq!(run.results[1].data.rows[0][0].as_deref(), Some("a\\"));
            session
                .run_sql("mysql", "DROP DATABASE sql_script_test")
                .await
                .unwrap();
        });
    }
}
