use crate::{
    data_export::{ExportFormat, ExportWriter, QueryExport},
    settings::{Connection, DatabaseKind},
};
use sqlx::{Column as _, Connection as _, Executor as _, MySqlConnection, Row, TypeInfo as _};
use std::collections::BTreeMap;

const CELL_PREVIEW_CHARS: usize = 200;
pub(crate) const PAGE_SIZE: usize = 1000;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum FilterOperator {
    Contains,
    NotContains,
    Equals,
    NotEquals,
    StartsWith,
    EndsWith,
    GreaterThan,
    GreaterOrEqual,
    LessThan,
    LessOrEqual,
}

impl FilterOperator {
    pub(crate) const ALL: [Self; 10] = [
        Self::Contains,
        Self::NotContains,
        Self::Equals,
        Self::NotEquals,
        Self::StartsWith,
        Self::EndsWith,
        Self::GreaterThan,
        Self::GreaterOrEqual,
        Self::LessThan,
        Self::LessOrEqual,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Contains => "Contains",
            Self::NotContains => "Does not contain",
            Self::Equals => "Equals",
            Self::NotEquals => "Does not equal",
            Self::StartsWith => "Starts with",
            Self::EndsWith => "Ends with",
            Self::GreaterThan => "Greater than",
            Self::GreaterOrEqual => "Greater than or equal",
            Self::LessThan => "Less than",
            Self::LessOrEqual => "Less than or equal",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TableFilter {
    pub(crate) column: Option<String>,
    pub(crate) operator: FilterOperator,
    pub(crate) value: String,
}

#[derive(Clone, Default)]
pub(crate) struct TableQuery {
    pub(crate) page: usize,
    pub(crate) filters: Vec<TableFilter>,
    pub(crate) sort: Option<(String, bool)>,
}

impl TableQuery {
    fn clauses(&self, schema: &TableSchema) -> Result<(String, Vec<String>, u64), String> {
        let offset = self
            .page
            .checked_mul(PAGE_SIZE)
            .and_then(|offset| u64::try_from(offset).ok())
            .ok_or("Page is too large.")?;
        let mut predicates = Vec::new();
        let mut values = Vec::new();
        for filter in &self.filters {
            let columns = match &filter.column {
                Some(name) => vec![
                    schema
                        .columns
                        .iter()
                        .position(|column| column == name)
                        .ok_or("Filter column is unavailable.")?,
                ],
                None => (0..schema.columns.len()).collect(),
            };
            if columns.is_empty() {
                return Err("Filter column is unavailable.".into());
            }
            use FilterOperator::*;
            let text_match = matches!(
                filter.operator,
                Contains | NotContains | StartsWith | EndsWith
            );
            let negative = matches!(filter.operator, NotContains | NotEquals);
            let operator = match filter.operator {
                Contains | StartsWith | EndsWith => "LIKE ? ESCAPE '='",
                NotContains => "NOT LIKE ? ESCAPE '='",
                Equals => "= ?",
                NotEquals => "<> ?",
                GreaterThan => "> ?",
                GreaterOrEqual => ">= ?",
                LessThan => "< ?",
                LessOrEqual => "<= ?",
            };
            let value = if text_match {
                let escaped = filter
                    .value
                    .replace('=', "==")
                    .replace('%', "=%")
                    .replace('_', "=_");
                match filter.operator {
                    StartsWith => format!("{escaped}%"),
                    EndsWith => format!("%{escaped}"),
                    _ => format!("%{escaped}%"),
                }
            } else {
                filter.value.clone()
            };
            let mut matches = Vec::new();
            for ix in columns {
                // Compare original typed values so numeric comparisons stay numeric.
                let column = if text_match || schema.is_binary(ix) {
                    schema.expression(ix, false)
                } else {
                    quote_identifier(&schema.columns[ix])
                };
                let predicate = format!("{column} {operator}");
                matches.push(if filter.column.is_none() && negative {
                    format!("COALESCE({predicate}, TRUE)")
                } else {
                    predicate
                });
                values.push(value.clone());
            }
            predicates.push(if filter.column.is_some() {
                matches.join(" AND ")
            } else {
                format!(
                    "({})",
                    matches.join(if negative { " AND " } else { " OR " })
                )
            });
        }
        let mut order = Vec::new();
        if let Some((name, descending)) = &self.sort {
            if !schema.columns.contains(name) {
                return Err("Sort column is unavailable.".into());
            }
            order.push(format!(
                "{} {}",
                quote_identifier(name),
                if *descending { "DESC" } else { "ASC" }
            ));
        }
        // shortcut: tables without a primary key sort by all columns; use a unique index when supporting expensive large-column tables.
        let tie_breakers = if schema.primary_key.is_empty() {
            (0..schema.columns.len()).collect::<Vec<_>>()
        } else {
            schema.primary_key.clone()
        };
        for ix in tie_breakers {
            let name = &schema.columns[ix];
            if self.sort.as_ref().is_none_or(|(sorted, _)| sorted != name) {
                order.push(format!("{} ASC", quote_identifier(name)));
            }
        }
        let filter = if predicates.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", predicates.join(" AND "))
        };
        Ok((
            format!("{filter} ORDER BY {}", order.join(", ")),
            values,
            offset,
        ))
    }
}

pub(crate) struct Session {
    connection_key: String,
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
    pub(crate) has_more: bool,
}

#[non_exhaustive]
pub(crate) struct TableStructure {
    pub(crate) columns: TableRows,
    pub(crate) indexes: TableRows,
    pub(crate) foreign_keys: TableRows,
    pub(crate) ddl: String,
    pub(crate) no_backslash_escapes: bool,
}

fn metadata_rows(
    records: Vec<sqlx::mysql::MySqlRow>,
    names: &[&str],
) -> Result<TableRows, sqlx::Error> {
    Ok(TableRows {
        schema: TableSchema {
            columns: names.iter().map(|name| (*name).to_owned()).collect(),
            ..TableSchema::default()
        },
        rows: records
            .iter()
            .map(|row| (0..names.len()).map(|ix| row.try_get(ix)).collect())
            .collect::<Result<_, _>>()?,
        keys: Vec::new(),
        has_more: false,
    })
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

pub(crate) fn quote_identifier(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

pub(crate) fn truncate_preview(mut value: String) -> String {
    if let Some((end, _)) = value.char_indices().nth(CELL_PREVIEW_CHARS) {
        value.truncate(end);
        value.push('…');
    }
    value
}

fn sql_values(
    row: &sqlx::mysql::MySqlRow,
    preview: bool,
) -> Result<Vec<Option<String>>, sqlx::Error> {
    (0..row.len())
        .map(|ix| {
            let bytes = row.try_get_unchecked::<Option<&[u8]>, _>(ix)?;
            bytes
                .map(|bytes| -> Result<String, sqlx::Error> {
                    let kind = row.columns()[ix].type_info().name();
                    if kind.contains("BLOB")
                        || kind.contains("BINARY")
                        || matches!(kind, "BIT" | "GEOMETRY")
                    {
                        let mut value = bytes
                            .iter()
                            .take(if preview { 100 } else { bytes.len() })
                            .map(|byte| format!("{byte:02X}"))
                            .collect::<String>();
                        if preview && bytes.len() > 100 {
                            value.push('…');
                        }
                        Ok(value)
                    } else if preview {
                        Ok(truncate_preview(
                            String::from_utf8_lossy(bytes).into_owned(),
                        ))
                    } else {
                        String::from_utf8(bytes.to_vec())
                            .map_err(|error| sqlx::Error::Decode(Box::new(error)))
                    }
                })
                .transpose()
        })
        .collect()
}

async fn read_schema(
    connection: &mut MySqlConnection,
    database: &str,
    table: &str,
) -> Result<TableSchema, sqlx::Error> {
    let metadata = sqlx::query(
                "SELECT COLUMN_NAME, COLUMN_TYPE, COLUMN_KEY, EXTRA FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? ORDER BY ORDINAL_POSITION",
            ).bind(database).bind(table).fetch_all(&mut *connection).await?;
    let mut schema = TableSchema::default();
    for column in metadata {
        let ix = schema.columns.len();
        schema.columns.push(column.try_get(0)?);
        schema.column_types.push(column.try_get(1)?);
        if column.try_get::<String, _>(2)? == "PRI" {
            schema.primary_key.push(ix);
        }
        if column.try_get::<String, _>(3)?.contains("GENERATED") {
            schema.generated_columns.push(ix);
        }
    }
    if schema.columns.is_empty() {
        return Err(sqlx::Error::Protocol(
            "Table is unavailable or has no accessible columns.".into(),
        ));
    }
    Ok(schema)
}

impl Session {
    pub(crate) fn connection_key(&self) -> &str {
        &self.connection_key
    }

    pub(crate) async fn read_structure(
        &self,
        database: &str,
        table: &str,
    ) -> Result<TableStructure, String> {
        async_std::future::timeout(std::time::Duration::from_secs(15), async {
            let mut connection = self.connection.lock().await;
            let sql_mode: String = sqlx::query_scalar("SELECT @@SESSION.sql_mode").fetch_one(&mut *connection).await?;
            let ddl = sqlx::query(&format!("SHOW CREATE TABLE {}.{}", quote_identifier(database), quote_identifier(table)))
                .fetch_one(&mut *connection).await?.try_get(1)?;
            let columns = sqlx::query("SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_DEFAULT, COLUMN_KEY, EXTRA, COLLATION_NAME, COLUMN_COMMENT, GENERATION_EXPRESSION FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? ORDER BY ORDINAL_POSITION")
                .bind(database).bind(table).fetch_all(&mut *connection).await?;
            let indexes = sqlx::query("SELECT INDEX_NAME, IF(NON_UNIQUE = 0, 'Yes', 'No'), CAST(SEQ_IN_INDEX AS CHAR), COLUMN_NAME, CAST(SUB_PART AS CHAR), INDEX_TYPE FROM information_schema.STATISTICS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? ORDER BY INDEX_NAME, SEQ_IN_INDEX")
                .bind(database).bind(table).fetch_all(&mut *connection).await?;
            let foreign_keys = sqlx::query("SELECT k.CONSTRAINT_NAME, k.COLUMN_NAME, k.REFERENCED_TABLE_SCHEMA, k.REFERENCED_TABLE_NAME, k.REFERENCED_COLUMN_NAME, r.UPDATE_RULE, r.DELETE_RULE FROM information_schema.KEY_COLUMN_USAGE k JOIN information_schema.REFERENTIAL_CONSTRAINTS r ON r.CONSTRAINT_SCHEMA = k.CONSTRAINT_SCHEMA AND r.TABLE_NAME = k.TABLE_NAME AND r.CONSTRAINT_NAME = k.CONSTRAINT_NAME WHERE k.TABLE_SCHEMA = ? AND k.TABLE_NAME = ? AND k.REFERENCED_TABLE_NAME IS NOT NULL ORDER BY k.CONSTRAINT_NAME, k.ORDINAL_POSITION")
                .bind(database).bind(table).fetch_all(&mut *connection).await?;
            Ok::<_, sqlx::Error>(TableStructure {
                columns: metadata_rows(columns, &["Column", "Type", "Nullable", "Default", "Key", "Extra", "Collation", "Comment", "Expression"] )?,
                indexes: metadata_rows(indexes, &["Index", "Unique", "Position", "Column", "Prefix length", "Type"] )?,
                foreign_keys: metadata_rows(foreign_keys, &["Constraint", "Column", "Database", "Table", "Referenced column", "On update", "On delete"] )?,
                ddl,
                no_backslash_escapes: sql_mode.split(',').any(|mode| mode == "NO_BACKSLASH_ESCAPES"),
            })
        }).await.map_err(|_| "Loading table structure timed out. Try Refresh.".to_owned())?
            .map_err(|error| format!("Couldn’t load table structure: {error}"))
    }

    pub(crate) async fn run_sql(
        &self,
        database: &str,
        sql: &str,
    ) -> Result<(TableRows, u64), String> {
        self.run_sql_capture(database, sql, None).await
    }

    pub(crate) async fn run_sql_export(
        &self,
        database: &str,
        sql: &str,
    ) -> Result<(TableRows, u64, QueryExport), String> {
        let mut export = QueryExport::new().map_err(|error| {
            format!("Couldn’t prepare result export: {error}. SQL was not executed.")
        })?;
        let (data, affected) = self
            .run_sql_capture(database, sql, Some(&mut export))
            .await?;
        export
            .finish()
            .map_err(|error| format!("SQL completed, but saving full results failed: {error}."))?;
        Ok((data, affected, export))
    }

    async fn run_sql_capture(
        &self,
        database: &str,
        sql: &str,
        mut capture: Option<&mut QueryExport>,
    ) -> Result<(TableRows, u64), String> {
        use async_std::stream::StreamExt;
        if sql.trim().is_empty() {
            return Err("Enter a SQL statement to run.".into());
        }
        let result = async {
            let mut connection = self.connection.lock().await;
            sqlx::query(&format!("USE {}", quote_identifier(database)))
                .execute(&mut *connection)
                .await?;
            // Preparing validates a single statement without executing it.
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
            let mut data = TableRows {
                schema,
                rows: Vec::new(),
                keys: Vec::new(),
                has_more: false,
            };
            if let Some(export) = capture.as_deref_mut() {
                export.set_columns(&data.schema.columns);
            }
            let mut capture_error = None;
            let mut affected = 0;
            // shortcut: only the first result set is displayed; add a result selector when needed for stored procedures.
            let mut first_result = true;
            let mut stream = sqlx::raw_sql(sql).fetch_many(&mut *connection);
            while let Some(item) = stream.next().await {
                match item? {
                    sqlx::Either::Left(result) => {
                        affected += result.rows_affected();
                        first_result = false;
                    }
                    sqlx::Either::Right(row) if first_result => {
                        if data.rows.is_empty() {
                            data.schema.columns = row
                                .columns()
                                .iter()
                                .map(|column| column.name().to_owned())
                                .collect();
                            data.schema.column_types = row
                                .columns()
                                .iter()
                                .map(|column| column.type_info().name().to_owned())
                                .collect();
                        }
                        if let Some(export) = capture.as_deref_mut() {
                            if data.rows.is_empty() {
                                export.set_columns(&data.schema.columns);
                            }
                            if capture_error.is_none() {
                                let result = sql_values(&row, false)
                                    .map_err(anyhow::Error::from)
                                    .and_then(|values| export.push(&values));
                                if let Err(error) = result {
                                    capture_error = Some(error);
                                }
                            }
                        }
                        if data.rows.len() == PAGE_SIZE {
                            data.has_more = true;
                            continue;
                        }
                        let values = sql_values(&row, true)?;
                        data.rows.push(values);
                    }
                    _ => {}
                }
            }
            // Drain the stream even if the disk fills, leaving the shared session usable.
            if let Some(error) = capture_error {
                return Err(sqlx::Error::Protocol(format!(
                    "SQL completed, but saving full results failed: {error}."
                )));
            }
            Ok::<_, sqlx::Error>((data, affected))
        }
        .await;
        result.map_err(|error| format!("Couldn’t run SQL: {error}"))
    }

    pub(crate) async fn read_table(
        &self,
        database: &str,
        table: &str,
        options: &TableQuery,
    ) -> Result<TableRows, String> {
        async_std::future::timeout(std::time::Duration::from_secs(15), async {
            let mut connection = self.connection.lock().await;
            let schema = read_schema(&mut connection, database, table).await?;
            let mut expressions = (0..schema.columns.len())
                .map(|ix| schema.expression(ix, true))
                .collect::<Vec<_>>();
            expressions.extend(
                schema
                    .primary_key
                    .iter()
                    .map(|ix| schema.expression(*ix, false)),
            );
            let (clauses, values, offset) =
                options.clauses(&schema).map_err(sqlx::Error::Protocol)?;
            let query = format!(
                "SELECT {} FROM {}.{}{clauses} LIMIT ? OFFSET ?",
                expressions.join(", "),
                quote_identifier(database),
                quote_identifier(table)
            );
            let mut statement = sqlx::query(&query);
            for value in values {
                statement = statement.bind(value);
            }
            let mut records = statement
                .bind((PAGE_SIZE + 1) as u64)
                .bind(offset)
                .fetch_all(&mut *connection)
                .await?;
            let has_more = records.len() > PAGE_SIZE;
            records.truncate(PAGE_SIZE);
            let rows = records
                .iter()
                .map(|row| {
                    (0..schema.columns.len())
                        .map(|ix| {
                            row.try_get::<Option<String>, _>(ix)
                                .map(|value| value.map(truncate_preview))
                        })
                        .collect()
                })
                .collect::<Result<Vec<Vec<Option<String>>>, sqlx::Error>>()?;
            let keys = records
                .iter()
                .map(|row| {
                    (schema.columns.len()..expressions.len())
                        .map(|ix| row.try_get::<Option<String>, _>(ix))
                        .collect()
                })
                .collect::<Result<Vec<Vec<Option<String>>>, sqlx::Error>>()?;
            Ok::<_, sqlx::Error>(TableRows {
                schema,
                rows,
                keys,
                has_more,
            })
        })
        .await
        .map_err(|_| "Loading table timed out. Double-click the table to try again.".to_owned())?
        .map_err(|error| {
            format!("Couldn’t load table: {error}. Double-click the table to try again.")
        })
    }

    pub(crate) async fn export_table(
        &self,
        database: &str,
        table: &str,
        options: &TableQuery,
        path: &std::path::Path,
        format: ExportFormat,
    ) -> anyhow::Result<usize> {
        use async_std::stream::StreamExt;
        let mut connection = self.connection.lock().await;
        let schema = read_schema(&mut connection, database, table).await?;
        let (clauses, values, _) = options.clauses(&schema).map_err(anyhow::Error::msg)?;
        let expressions = (0..schema.columns.len())
            .map(|ix| schema.expression(ix, false))
            .collect::<Vec<_>>();
        let sql = format!(
            "SELECT {} FROM {}.{}{clauses}",
            expressions.join(", "),
            quote_identifier(database),
            quote_identifier(table)
        );
        let mut statement = sqlx::query(&sql);
        for value in values {
            statement = statement.bind(value);
        }
        let mut writer = ExportWriter::new(path, format, &schema.columns)?;
        let mut write_error = None;
        let mut stream = statement.fetch(&mut *connection);
        while let Some(row) = stream.next().await {
            let row = row?;
            if write_error.is_none() {
                let result = (0..schema.columns.len())
                    .map(|ix| row.try_get(ix))
                    .collect::<Result<Vec<Option<String>>, sqlx::Error>>()
                    .map_err(anyhow::Error::from)
                    .and_then(|values| writer.push(&values));
                if let Err(error) = result {
                    write_error = Some(error);
                }
            }
        }
        if let Some(error) = write_error {
            return Err(error);
        }
        writer.finish(path)
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
        Ok::<_, sqlx::Error>(Session { connection_key: connection.key(), connection: async_std::sync::Mutex::new(session), databases })
    }).await.map_err(|_| "Connection timed out. Check the host and port, then try again.".to_owned())?
        .map_err(|error| format!("Couldn’t connect or load databases: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{FilterOperator, TableFilter};

    fn contains(column: &str, value: &str) -> TableFilter {
        TableFilter {
            column: Some(column.into()),
            operator: FilterOperator::Contains,
            value: value.into(),
        }
    }

    #[test]
    fn filter_operators_bind_values_and_group_all_columns() {
        use super::{TableQuery, TableSchema};
        use FilterOperator::*;
        let schema = TableSchema {
            columns: vec!["number".into(), "text".into()],
            column_types: vec!["int".into(), "text".into()],
            primary_key: vec![0],
            ..TableSchema::default()
        };
        for (operator, predicate, value) in [
            (
                Contains,
                "CAST(`number` AS CHAR CHARACTER SET utf8mb4) LIKE ? ESCAPE '='",
                "%2=%=_==%",
            ),
            (
                NotContains,
                "CAST(`number` AS CHAR CHARACTER SET utf8mb4) NOT LIKE ? ESCAPE '='",
                "%2=%=_==%",
            ),
            (
                StartsWith,
                "CAST(`number` AS CHAR CHARACTER SET utf8mb4) LIKE ? ESCAPE '='",
                "2=%=_==%",
            ),
            (
                EndsWith,
                "CAST(`number` AS CHAR CHARACTER SET utf8mb4) LIKE ? ESCAPE '='",
                "%2=%=_==",
            ),
            (Equals, "`number` = ?", "2%_="),
            (NotEquals, "`number` <> ?", "2%_="),
            (GreaterThan, "`number` > ?", "2%_="),
            (GreaterOrEqual, "`number` >= ?", "2%_="),
            (LessThan, "`number` < ?", "2%_="),
            (LessOrEqual, "`number` <= ?", "2%_="),
        ] {
            let query = TableQuery {
                filters: vec![TableFilter {
                    column: Some("number".into()),
                    operator,
                    value: "2%_=".into(),
                }],
                ..TableQuery::default()
            };
            let (sql, values, _) = query.clauses(&schema).unwrap();
            assert_eq!(sql, format!(" WHERE {predicate} ORDER BY `number` ASC"));
            assert_eq!(values, [value]);
        }
        let mut query = TableQuery {
            filters: vec![
                TableFilter {
                    column: None,
                    operator: Contains,
                    value: "2".into(),
                },
                contains("text", "x"),
            ],
            ..TableQuery::default()
        };
        let (sql, values, _) = query.clauses(&schema).unwrap();
        assert_eq!(
            sql,
            " WHERE (CAST(`number` AS CHAR CHARACTER SET utf8mb4) LIKE ? ESCAPE '=' OR CAST(`text` AS CHAR CHARACTER SET utf8mb4) LIKE ? ESCAPE '=') AND CAST(`text` AS CHAR CHARACTER SET utf8mb4) LIKE ? ESCAPE '=' ORDER BY `number` ASC"
        );
        assert_eq!(values, ["%2%", "%2%", "%x%"]);
        query.filters.truncate(1);
        query.filters[0].operator = NotEquals;
        let (sql, values, _) = query.clauses(&schema).unwrap();
        assert_eq!(
            sql,
            " WHERE (COALESCE(`number` <> ?, TRUE) AND COALESCE(`text` <> ?, TRUE)) ORDER BY `number` ASC"
        );
        assert_eq!(values, ["2", "2"]);
    }

    #[test]
    fn table_queries_bind_literal_filters_validate_columns_and_order_pages() {
        use super::{TableQuery, TableSchema};
        let schema = TableSchema {
            columns: vec!["id".into(), "text`value".into(), "bytes".into()],
            column_types: vec!["int".into(), "text".into(), "blob".into()],
            primary_key: vec![0],
            ..TableSchema::default()
        };
        let query = TableQuery {
            page: 2,
            filters: vec![
                contains("text`value", "a%_=' OR 1=1 --"),
                contains("bytes", "00FF"),
            ],
            sort: Some(("text`value".into(), true)),
        };
        let (sql, values, offset) = query.clauses(&schema).unwrap();
        assert_eq!(
            sql,
            " WHERE CAST(`text``value` AS CHAR CHARACTER SET utf8mb4) LIKE ? ESCAPE '=' AND HEX(`bytes`) LIKE ? ESCAPE '=' ORDER BY `text``value` DESC, `id` ASC"
        );
        assert_eq!(values, ["%a=%=_==' OR 1==1 --%", "%00FF%"]);
        assert_eq!(offset, 2000);
        assert_eq!(
            TableQuery::default().clauses(&schema).unwrap().0,
            " ORDER BY `id` ASC"
        );
        let no_key = TableSchema {
            primary_key: vec![],
            ..schema.clone()
        };
        assert_eq!(
            TableQuery::default().clauses(&no_key).unwrap().0,
            " ORDER BY `id` ASC, `text``value` ASC, `bytes` ASC"
        );
        assert!(
            TableQuery {
                sort: Some(("missing".into(), false)),
                ..TableQuery::default()
            }
            .clauses(&schema)
            .is_err()
        );
        assert!(
            TableQuery {
                filters: vec![contains("missing", "x")],
                ..TableQuery::default()
            }
            .clauses(&schema)
            .is_err()
        );
        assert!(
            TableQuery {
                page: usize::MAX,
                ..TableQuery::default()
            }
            .clauses(&schema)
            .is_err()
        );
    }

    #[test]
    #[ignore = "requires a temporary MariaDB server and TABLELANE_TEST_MARIADB_PORT"]
    fn mariadb_filters_sorts_and_pages_full_values_with_row_keys() {
        use super::TableQuery;
        let port = std::env::var("TABLELANE_TEST_MARIADB_PORT")
            .unwrap()
            .parse()
            .unwrap();
        async_std::task::block_on(async {
            let session = super::connect(&crate::settings::Connection {
                name: "Test".into(),
                database_type: crate::settings::DatabaseKind::MariaDB,
                host: "127.0.0.1".into(),
                port: Some(port),
                username: "root".into(),
                password: String::new(),
                database: String::new(),
                file_path: String::new(),
            })
            .await
            .unwrap();
            let mut connection = session.connection.lock().await;
            sqlx::query("CREATE DATABASE browse_test")
                .execute(&mut *connection)
                .await
                .unwrap();
            sqlx::query("CREATE TABLE browse_test.widgets (id INT PRIMARY KEY, category INT, body LONGTEXT, bytes BLOB) ENGINE=InnoDB").execute(&mut *connection).await.unwrap();
            sqlx::query("INSERT INTO browse_test.widgets SELECT seq, seq % 2, CONCAT(REPEAT('x', 250), 'tail', seq), X'00FF' FROM browse_test.seq_1_to_2005").execute(&mut *connection).await.unwrap();
            sqlx::query("UPDATE browse_test.widgets SET body = ? WHERE id = 2004")
                .bind("literal%_=' OR 1=1 --")
                .execute(&mut *connection)
                .await
                .unwrap();
            drop(connection);
            let mut query = TableQuery::default();
            let first = session
                .read_table("browse_test", "widgets", &query)
                .await
                .unwrap();
            assert_eq!(first.rows.len(), 1000);
            assert!(first.has_more);
            assert_eq!(first.rows[0][0].as_deref(), Some("1"));
            assert_eq!(first.rows[999][0].as_deref(), Some("1000"));
            assert_eq!(first.keys[999], [Some("1000".into())]);
            let export_path = std::env::temp_dir().join(format!(
                "tablelane-filter-export-{}.json",
                std::process::id()
            ));
            let export_query = TableQuery {
                page: 2,
                filters: vec![contains("category", "1")],
                sort: Some(("id".into(), true)),
            };
            assert_eq!(
                session
                    .export_table(
                        "browse_test",
                        "widgets",
                        &export_query,
                        &export_path,
                        ExportFormat::Json
                    )
                    .await
                    .unwrap(),
                1003
            );
            let json: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&export_path).unwrap()).unwrap();
            assert_eq!(json["rows"][0][0], "2005");
            assert_eq!(json["rows"][1002][0], "1");
            assert_eq!(json["rows"][0][2], format!("{}tail2005", "x".repeat(250)));
            assert_eq!(json["rows"][0][3], "00FF");
            let invalid = TableQuery {
                sort: Some(("missing".into(), false)),
                ..TableQuery::default()
            };
            assert!(
                session
                    .export_table(
                        "browse_test",
                        "widgets",
                        &invalid,
                        &export_path,
                        ExportFormat::Json
                    )
                    .await
                    .is_err()
            );
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&std::fs::read(&export_path).unwrap())
                    .unwrap(),
                json
            );
            std::fs::remove_file(export_path).unwrap();
            query.page = 1;
            let second = session
                .read_table("browse_test", "widgets", &query)
                .await
                .unwrap();
            assert_eq!(second.rows[0][0].as_deref(), Some("1001"));
            assert!(second.has_more);
            query.page = 2;
            let last = session
                .read_table("browse_test", "widgets", &query)
                .await
                .unwrap();
            assert_eq!(last.rows.len(), 5);
            assert!(!last.has_more);
            assert_eq!(last.rows[4][0].as_deref(), Some("2005"));
            query.page = 0;
            query.sort = Some(("id".into(), true));
            let sorted = session
                .read_table("browse_test", "widgets", &query)
                .await
                .unwrap();
            assert_eq!(sorted.rows[0][0].as_deref(), Some("2005"));
            assert_eq!(sorted.rows[999][0].as_deref(), Some("1006"));
            query.sort = Some(("category".into(), false));
            let ties = session
                .read_table("browse_test", "widgets", &query)
                .await
                .unwrap();
            assert_eq!(ties.rows[0][0].as_deref(), Some("2"));
            assert_eq!(ties.rows[999][0].as_deref(), Some("2000"));
            query.page = 1;
            let ties_next = session
                .read_table("browse_test", "widgets", &query)
                .await
                .unwrap();
            assert_eq!(ties_next.rows[0][0].as_deref(), Some("2002"));
            query.page = 0;
            query.filters = vec![contains("body", "tail2005"), contains("category", "1")];
            let filtered = session
                .read_table("browse_test", "widgets", &query)
                .await
                .unwrap();
            assert_eq!(filtered.rows.len(), 1);
            assert!(!filtered.has_more);
            assert_eq!(filtered.keys[0], [Some("2005".into())]);
            assert!(filtered.rows[0][2].as_ref().unwrap().ends_with('…'));
            assert!(
                session
                    .read_row(
                        "browse_test",
                        "widgets",
                        &filtered.schema,
                        &filtered.keys[0]
                    )
                    .await
                    .unwrap()[2]
                    .as_ref()
                    .unwrap()
                    .ends_with("tail2005")
            );
            query.filters = vec![contains("body", "%_=' OR 1=1 --")];
            let literal = session
                .read_table("browse_test", "widgets", &query)
                .await
                .unwrap();
            assert_eq!(literal.rows.len(), 1);
            assert_eq!(literal.rows[0][0].as_deref(), Some("2004"));
            query.filters = vec![contains("bytes", "00FF")];
            assert!(
                session
                    .read_table("browse_test", "widgets", &query)
                    .await
                    .unwrap()
                    .has_more
            );
            query.filters = vec![contains("body", "unmatched")];
            let empty = session
                .read_table("browse_test", "widgets", &query)
                .await
                .unwrap();
            assert!(empty.rows.is_empty());
            assert!(!empty.has_more);
            use FilterOperator::*;
            query.sort = None;
            for (column, operator, value, count, first_id) in [
                (Some("id"), GreaterThan, "2000", 5, "2001"),
                (Some("id"), GreaterOrEqual, "2000", 6, "2000"),
                (Some("id"), LessThan, "10", 9, "1"),
                (Some("id"), LessOrEqual, "10", 10, "1"),
                (Some("id"), Equals, "10", 1, "10"),
                (Some("body"), NotContains, "tail", 1, "2004"),
                (Some("body"), StartsWith, "literal%_=", 1, "2004"),
                (Some("body"), EndsWith, "tail2005", 1, "2005"),
                (None, Contains, "tail2005", 1, "2005"),
                (None, NotContains, "tail", 1, "2004"),
            ] {
                query.filters = vec![TableFilter {
                    column: column.map(str::to_owned),
                    operator,
                    value: value.into(),
                }];
                let result = session
                    .read_table("browse_test", "widgets", &query)
                    .await
                    .unwrap();
                assert_eq!(result.rows.len(), count, "{column:?} {operator:?}");
                assert_eq!(result.rows[0][0].as_deref(), Some(first_id));
                assert!(!result.has_more);
            }
            sqlx::query("UPDATE browse_test.widgets SET body = NULL WHERE id = 2005")
                .execute(&mut *session.connection.lock().await)
                .await
                .unwrap();
            let result = session
                .read_table("browse_test", "widgets", &query)
                .await
                .unwrap();
            assert_eq!(result.rows.len(), 2);
            assert_eq!(result.rows[1][0].as_deref(), Some("2005"));
            sqlx::query("DROP DATABASE browse_test")
                .execute(&mut *session.connection.lock().await)
                .await
                .unwrap();
        });
    }
    use super::connection_url;
    use crate::{
        data_export::ExportFormat,
        settings::{Connection, DatabaseKind},
    };

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
                .read_table(
                    "row_editor_test",
                    "edit`rows",
                    &super::TableQuery::default(),
                )
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
        let rows = async_std::task::block_on(session.read_table(
            "tablelane_test",
            "widgets",
            &super::TableQuery::default(),
        ))
        .unwrap();
        assert!(!rows.schema.columns.is_empty());
        assert!(
            async_std::task::block_on(session.read_table(
                "tablelane_test",
                "missing_table",
                &super::TableQuery::default()
            ))
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
                .read_table(
                    "preview`test",
                    "types`/table",
                    &super::TableQuery::default(),
                )
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
                .read_table(
                    "preview`test",
                    "types`/table",
                    &super::TableQuery::default(),
                )
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
                    .read_table(
                        "preview`test",
                        "types`/table",
                        &super::TableQuery::default()
                    )
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
                .read_table(
                    "preview`test",
                    "types`/table",
                    &super::TableQuery::default(),
                )
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
