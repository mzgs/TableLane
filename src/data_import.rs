use crate::database::TableSchema;
use anyhow::{Context, Result, ensure};
use std::{
    fs::{self, File, Metadata},
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

pub(crate) struct CsvImport {
    path: PathBuf,
    metadata: Metadata,
    headers: Vec<String>,
    first_row: Vec<Option<String>>,
    record_limit: usize,
    rows: AtomicU64,
    bytes: AtomicU64,
}

impl CsvImport {
    pub(crate) fn read(path: &Path, record_limit: usize) -> Result<Self> {
        let file = File::open(path)?;
        let metadata = file.metadata()?;
        ensure!(metadata.is_file(), "Choose a regular CSV file.");
        metadata
            .modified()
            .context("Couldn’t inspect the CSV modification time")?;
        let mut reader = CsvReader::new(BufReader::with_capacity(64 * 1024, file), record_limit);
        let headers = read_headers(&mut reader)?;
        let first_row = reader
            .read_record()?
            .context("CSV contains no data rows.")?;
        validate_width(&first_row, headers.len(), 2)?;
        let first_row = first_row
            .into_iter()
            .map(|value| {
                value.map(|value| {
                    crate::database::truncate_preview(value.chars().take(201).collect())
                })
            })
            .collect();
        let csv = Self {
            path: path.to_owned(),
            metadata,
            headers,
            first_row,
            record_limit,
            rows: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
        };
        csv.verify_unchanged(&reader)?;
        Ok(csv)
    }

    pub(crate) fn headers(&self) -> &[String] {
        &self.headers
    }
    pub(crate) fn first_row(&self) -> &[Option<String>] {
        &self.first_row
    }
    pub(crate) fn byte_len(&self) -> u64 {
        self.metadata.len()
    }
    pub(crate) fn progress(&self) -> (u64, u64) {
        (
            self.rows.load(Ordering::Relaxed),
            self.bytes.load(Ordering::Relaxed),
        )
    }
    pub(crate) fn set_progress(&self, rows: u64, bytes: u64) {
        self.rows.store(rows, Ordering::Relaxed);
        self.bytes.store(bytes, Ordering::Relaxed);
    }

    fn matches_source(&self, metadata: &Metadata) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.dev() != self.metadata.dev() || metadata.ino() != self.metadata.ino() {
                return false;
            }
        }
        metadata.len() == self.metadata.len()
            && metadata.modified().ok() == self.metadata.modified().ok()
    }

    pub(crate) fn verify_unchanged(&self, reader: &CsvReader<BufReader<File>>) -> Result<()> {
        ensure!(
            self.matches_source(&reader.reader.get_ref().metadata()?)
                && self.matches_source(&fs::metadata(&self.path)?),
            "CSV changed after its preview was opened. Cancel and choose the file again."
        );
        Ok(())
    }

    pub(crate) fn reader(&self) -> Result<CsvReader<BufReader<File>>> {
        let file = File::open(&self.path)?;
        ensure!(
            self.matches_source(&file.metadata()?),
            "CSV changed after its preview was opened. Cancel and choose the file again."
        );
        let mut reader =
            CsvReader::new(BufReader::with_capacity(64 * 1024, file), self.record_limit);
        ensure!(
            read_headers(&mut reader)? == self.headers,
            "CSV headers changed. Cancel and choose the file again."
        );
        self.set_progress(0, reader.bytes());
        Ok(reader)
    }

    pub(crate) fn validate_mapping(
        &self,
        schema: &TableSchema,
        mapping: &[Option<String>],
    ) -> Result<Vec<(usize, usize)>, String> {
        if mapping.len() != self.headers.len() {
            return Err("The mapping no longer matches the CSV headers.".into());
        }
        let mut columns = Vec::new();
        for (source, target) in mapping.iter().enumerate() {
            let Some(target) = target else {
                continue;
            };
            let dest = schema
                .columns
                .iter()
                .position(|name| name == target)
                .ok_or_else(|| {
                    format!(
                        "Column {target} is unavailable. Refresh the table and reopen Import CSV."
                    )
                })?;
            if !schema.is_editable(dest) {
                return Err(format!("Column {target} cannot be imported."));
            }
            if columns.iter().any(|(_, ix)| *ix == dest) {
                return Err(format!("Column {target} is mapped more than once."));
            }
            columns.push((source, dest));
        }
        if columns.is_empty() {
            return Err("Map at least one CSV column.".into());
        }
        Ok(columns)
    }

    pub(crate) fn validate_row(
        &self,
        schema: &TableSchema,
        columns: &[(usize, usize)],
        row: &[Option<String>],
        record: u64,
    ) -> Result<()> {
        validate_width(row, self.headers.len(), record)?;
        for (source, dest) in columns {
            if schema.is_binary(*dest)
                && row[*source].as_ref().is_some_and(|value| {
                    value.len() % 2 != 0 || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
            {
                anyhow::bail!(
                    "CSV record {record}, column {} must contain pairs of hexadecimal digits.",
                    schema.columns[*dest]
                );
            }
        }
        Ok(())
    }
}

fn validate_width(row: &[Option<String>], columns: usize, record: u64) -> Result<()> {
    ensure!(
        row.len() == columns,
        "CSV record {record} has {} fields; expected {columns}.",
        row.len()
    );
    Ok(())
}

fn read_headers<R: BufRead>(reader: &mut CsvReader<R>) -> Result<Vec<String>> {
    let mut row = reader
        .read_record()?
        .context("CSV is empty; a header row is required.")?;
    if let Some(Some(first)) = row.first_mut()
        && first.starts_with('\u{feff}')
    {
        first.drain(..3);
    }
    let headers = row
        .into_iter()
        .map(|name| {
            name.filter(|name| !name.is_empty())
                .context("CSV headers cannot be empty or NULL.")
        })
        .collect::<Result<Vec<_>>>()?;
    let mut unique = std::collections::HashSet::new();
    ensure!(
        headers.iter().all(|name| unique.insert(name)),
        "CSV headers must be unique."
    );
    Ok(headers)
}

pub(crate) struct CsvReader<R> {
    reader: R,
    bytes: u64,
    record: u64,
    limit: usize,
}

impl<R: BufRead> CsvReader<R> {
    pub(crate) fn new(reader: R, record_limit: usize) -> Self {
        Self {
            reader,
            bytes: 0,
            record: 0,
            limit: record_limit,
        }
    }
    pub(crate) fn bytes(&self) -> u64 {
        self.bytes
    }
    fn peek(&mut self) -> Result<Option<u8>> {
        Ok(self.reader.fill_buf()?.first().copied())
    }
    fn consume(&mut self, count: usize, start: u64) -> Result<()> {
        ensure!(
            self.bytes - start + count as u64 <= self.limit as u64,
            "CSV record {} exceeds this connection’s max_allowed_packet size ({} bytes). Increase the server setting and reconnect for larger records.",
            self.record,
            self.limit
        );
        self.reader.consume(count);
        self.bytes += count as u64;
        Ok(())
    }
    fn segment(&mut self, value: &mut Vec<u8>, quoted: bool, start: u64) -> Result<()> {
        let buffer = self.reader.fill_buf()?;
        let count = if quoted {
            memchr::memchr(b'"', buffer).unwrap_or(buffer.len())
        } else {
            let end = memchr::memchr3(b'"', b',', b'\n', buffer).unwrap_or(buffer.len());
            memchr::memchr(b'\r', &buffer[..end]).unwrap_or(end)
        };
        ensure!(
            self.bytes - start + count as u64 <= self.limit as u64,
            "CSV record {} exceeds this connection’s max_allowed_packet size ({} bytes). Increase the server setting and reconnect for larger records.",
            self.record,
            self.limit
        );
        value.extend_from_slice(&buffer[..count]);
        self.consume(count, start)
    }
    pub(crate) fn read_record(&mut self) -> Result<Option<Vec<Option<String>>>> {
        if self.peek()?.is_none() {
            return Ok(None);
        }
        self.record += 1;
        let start = self.bytes;
        let mut row = Vec::new();
        loop {
            let quoted = self.peek()? == Some(b'"');
            let mut value = Vec::new();
            if quoted {
                self.consume(1, start)?;
                loop {
                    self.segment(&mut value, true, start)?;
                    match self.peek()? {
                        Some(b'"') => {
                            self.consume(1, start)?;
                            if self.peek()? == Some(b'"') {
                                self.consume(1, start)?;
                                value.push(b'"');
                            } else {
                                break;
                            }
                        }
                        Some(_) => continue,
                        None => anyhow::bail!("Unclosed quote in CSV record {}.", self.record),
                    }
                }
                ensure!(
                    matches!(self.peek()?, None | Some(b',' | b'\r' | b'\n')),
                    "Unexpected text after a quote in CSV record {}.",
                    self.record
                );
            } else {
                loop {
                    self.segment(&mut value, false, start)?;
                    match self.peek()? {
                        Some(b'"') => {
                            anyhow::bail!("Unexpected quote in CSV record {}.", self.record)
                        }
                        None | Some(b',' | b'\r' | b'\n') => break,
                        _ => {}
                    }
                }
            }
            let value = String::from_utf8(value)
                .with_context(|| format!("CSV record {} must contain UTF-8 text.", self.record))?;
            row.push(if !quoted && value == "\\N" {
                None
            } else {
                Some(value)
            });
            match self.peek()? {
                Some(b',') => {
                    self.consume(1, start)?;
                    continue;
                }
                Some(b'\r') => {
                    self.consume(1, start)?;
                    ensure!(
                        self.peek()? == Some(b'\n'),
                        "CSV line endings must be LF or CRLF."
                    );
                    self.consume(1, start)?;
                }
                Some(b'\n') => self.consume(1, start)?,
                None => {}
                _ => unreachable!(),
            }
            return Ok(Some(row));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_export::TemporaryFile;
    use std::{
        io::{Cursor, Write},
        ops::Deref,
    };

    struct TestCsv {
        csv: CsvImport,
        _temporary: TemporaryFile,
    }
    impl Deref for TestCsv {
        type Target = CsvImport;
        fn deref(&self) -> &Self::Target {
            &self.csv
        }
    }
    fn csv_fixture(text: &str) -> Result<TestCsv> {
        let (temporary, mut file) = TemporaryFile::new(&std::env::temp_dir())?;
        file.write_all(text.as_bytes())?;
        drop(file);
        let csv = CsvImport::read(temporary.path(), 16 * 1024 * 1024)?;
        Ok(TestCsv {
            csv,
            _temporary: temporary,
        })
    }

    #[test]
    fn csv_streaming_handles_chunk_boundaries_late_errors_and_changed_sources() {
        let text = "\u{feff}body,bytes,nullable,empty,literal\r\n\"世界,\"\"quoted\"\"\nline\",00FF,\\N,,\"\\N\"\r\n";
        let csv = csv_fixture(text).unwrap();
        for capacity in 1..=16 {
            let mut reader =
                CsvReader::new(BufReader::with_capacity(capacity, Cursor::new(text)), 1024);
            assert_eq!(read_headers(&mut reader).unwrap(), csv.headers());
            assert_eq!(reader.read_record().unwrap().unwrap(), csv.first_row());
            assert!(reader.read_record().unwrap().is_none());
            assert_eq!(reader.bytes(), text.len() as u64);
        }
        for invalid in [
            b"body\nvalid\n\"unclosed".as_slice(),
            b"body\nvalid\n\xff",
            b"body\nvalid\nx\"y",
        ] {
            let mut reader =
                CsvReader::new(BufReader::with_capacity(1, Cursor::new(invalid)), 1024);
            assert!(reader.read_record().unwrap().is_some());
            assert!(reader.read_record().unwrap().is_some());
            assert!(reader.read_record().is_err());
        }
        let mut reader = CsvReader::new(Cursor::new(b"body\n123456789"), 8);
        assert!(reader.read_record().unwrap().is_some());
        assert!(
            reader
                .read_record()
                .unwrap_err()
                .to_string()
                .contains("max_allowed_packet")
        );
        std::fs::write(&csv.path, "body\nchanged").unwrap();
        assert!(
            csv.reader()
                .err()
                .unwrap()
                .to_string()
                .contains("CSV changed")
        );
    }

    #[test]
    #[ignore = "streams 16 GiB of generated CSV to check large-file handling and memory use"]
    fn csv_streaming_16_gib_uses_one_record_at_a_time() {
        use std::io::Read;
        struct RepeatedRows {
            row: Vec<u8>,
            offset: usize,
            remaining: u64,
        }
        impl Read for RepeatedRows {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                let count = buffer
                    .len()
                    .min(self.row.len() - self.offset)
                    .min(self.remaining as usize);
                buffer[..count].copy_from_slice(&self.row[self.offset..self.offset + count]);
                self.offset = (self.offset + count) % self.row.len();
                self.remaining -= count as u64;
                Ok(count)
            }
        }
        let bytes = 16_u64 * 1024 * 1024 * 1024;
        let mut row = vec![b'x'; 64 * 1024];
        *row.last_mut().unwrap() = b'\n';
        let input = RepeatedRows {
            row,
            offset: 0,
            remaining: bytes,
        };
        let mut reader = CsvReader::new(BufReader::with_capacity(64 * 1024, input), 1024 * 1024);
        let start = std::time::Instant::now();
        let mut rows = 0_u64;
        while let Some(row) = reader.read_record().unwrap() {
            assert_eq!(row.len(), 1);
            assert_eq!(row[0].as_ref().unwrap().len(), 64 * 1024 - 1);
            rows += 1;
        }
        assert_eq!(rows, 262_144);
        assert_eq!(reader.bytes(), bytes);
        eprintln!(
            "Streamed {bytes} bytes in {:?}; retained one 64 KiB record and one 64 KiB read buffer.",
            start.elapsed()
        );
    }

    #[test]
    #[ignore = "requires an isolated MariaDB server and TABLELANE_TEST_MARIADB_PORT"]
    fn mariadb_csv_streaming_batches_large_files_and_rolls_back_late_failures() {
        use crate::{
            database::{TableQuery, connect},
            settings::{Connection, DatabaseKind},
        };
        use std::{
            io::BufWriter,
            sync::Arc,
            time::{Duration, Instant},
        };
        async_std::task::block_on(async {
            let session = connect(&Connection {
                name: "Streaming import".into(),
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
            session
                .run_sql("mysql", "CREATE DATABASE csv_streaming_test")
                .await
                .unwrap();
            session.run_sql("csv_streaming_test", "CREATE TABLE bulk (id BIGINT PRIMARY KEY, body LONGTEXT NOT NULL, bytes BLOB, short VARCHAR(4)) ENGINE=InnoDB").await.unwrap();
            session
                .run_sql("csv_streaming_test", "CREATE TABLE failures LIKE bulk")
                .await
                .unwrap();
            let schema = session
                .read_table("csv_streaming_test", "bulk", &TableQuery::default())
                .await
                .unwrap()
                .schema;
            let (temporary, file) = TemporaryFile::new(&std::env::temp_dir()).unwrap();
            let mut writer = BufWriter::new(file);
            writeln!(
                writer,
                "id,body,bytes,short,skip0,skip1,skip2,skip3,skip4,skip5,skip6,skip7"
            )
            .unwrap();
            let body = "世界".repeat(60);
            for ix in 1..=100_003 {
                writeln!(writer, "{ix},{body},00FF,ok,x,x,x,x,x,x,x,x").unwrap();
            }
            writer.flush().unwrap();
            drop(writer);
            let limit = session.import_record_limit().await.unwrap();
            let csv = Arc::new(CsvImport::read(temporary.path(), limit).unwrap());
            assert!(csv.byte_len() > 16 * 1024 * 1024);
            let mut mapping = vec![
                Some("id".into()),
                Some("body".into()),
                Some("bytes".into()),
                Some("short".into()),
            ];
            mapping.extend(vec![None; 8]);
            let (before, _) = session.run_sql("csv_streaming_test", "SELECT VARIABLE_VALUE FROM information_schema.SESSION_STATUS WHERE VARIABLE_NAME = 'COM_INSERT'").await.unwrap();
            let before: u64 = before.rows[0][0].as_ref().unwrap().parse().unwrap();
            assert_eq!(
                session
                    .import_csv("csv_streaming_test", "bulk", &schema, &csv, &mapping)
                    .await
                    .unwrap(),
                100_003
            );
            assert_eq!(csv.progress(), (100_003, csv.byte_len()));
            let (after, _) = session.run_sql("csv_streaming_test", "SELECT VARIABLE_VALUE FROM information_schema.SESSION_STATUS WHERE VARIABLE_NAME = 'COM_INSERT'").await.unwrap();
            let after: u64 = after.rows[0][0].as_ref().unwrap().parse().unwrap();
            assert_eq!(
                after - before,
                101,
                "100,003 rows should use 101 batched inserts"
            );
            let (data, _) = session
                .run_sql(
                    "csv_streaming_test",
                    "SELECT COUNT(*), MAX(CHAR_LENGTH(body)), HEX(MAX(bytes)) FROM bulk",
                )
                .await
                .unwrap();
            assert_eq!(
                data.rows[0],
                [
                    Some("100003".into()),
                    Some("120".into()),
                    Some("00FF".into())
                ]
            );

            session
                .run_sql("csv_streaming_test", "SET SESSION sql_mode = ''")
                .await
                .unwrap();
            session
                .run_sql("csv_streaming_test", "SET SESSION max_error_count = 0")
                .await
                .unwrap();
            for tail in [
                "1006,\"unclosed",
                "1006,valid,xyz,ok",
                "1,duplicate,00FF,ok",
                "1006,valid,00FF,toolong",
                "1006,missing",
                "1006,\\N,00FF,ok",
            ] {
                let mut text = "id,body,bytes,short\n".to_owned();
                for ix in 1..=1005 {
                    text.push_str(&format!("{ix},valid,00FF,ok\n"));
                }
                text.push_str(tail);
                let csv = csv_fixture(&text).unwrap();
                let error = session
                    .import_csv(
                        "csv_streaming_test",
                        "failures",
                        &schema,
                        &csv,
                        &mapping[..4],
                    )
                    .await
                    .unwrap_err();
                assert!(error.contains("No rows were imported"), "{tail}: {error}");
                assert_eq!(
                    csv.progress().0,
                    1000,
                    "a successful first batch must be rolled back: {tail}"
                );
                let (data, _) = session
                    .run_sql("csv_streaming_test", "SELECT COUNT(*) FROM failures")
                    .await
                    .unwrap();
                assert_eq!(data.rows[0][0].as_deref(), Some("0"));
            }

            // A source changed while batches are running must roll back the previously inserted batches.
            csv.set_progress(0, 0);
            let changed_csv = csv.clone();
            let path = temporary.path().to_owned();
            let change = std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(30);
                while changed_csv.progress().0 < 1000 {
                    assert!(Instant::now() < deadline, "no import progress");
                    std::thread::sleep(Duration::from_millis(1));
                }
                let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
                writeln!(file, "100004,changed,00FF,ok,x,x,x,x,x,x,x,x").unwrap();
            });
            let error = session
                .import_csv("csv_streaming_test", "failures", &schema, &csv, &mapping)
                .await
                .unwrap_err();
            change.join().unwrap();
            assert!(error.contains("CSV changed"), "{error}");
            let (data, _) = session
                .run_sql("csv_streaming_test", "SELECT COUNT(*) FROM failures")
                .await
                .unwrap();
            assert_eq!(data.rows[0][0].as_deref(), Some("0"));
            session
                .run_sql("mysql", "DROP DATABASE csv_streaming_test")
                .await
                .unwrap();
            session.close().await;
        });
    }

    #[test]
    #[ignore = "requires an isolated MariaDB server and TABLELANE_TEST_MARIADB_PORT"]
    fn mariadb_csv_import_maps_full_values_and_rolls_back_every_failure() {
        use crate::{
            database::{TableQuery, connect},
            settings::{Connection, DatabaseKind},
        };
        async_std::task::block_on(async {
            let mut config = Connection {
                name: "Import test".into(),
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
            let session = connect(&config).await.unwrap();
            session
                .run_sql("mysql", "CREATE DATABASE csv_import_test")
                .await
                .unwrap();
            session.run_sql("csv_import_test", "CREATE TABLE `import``rows` (id INT PRIMARY KEY AUTO_INCREMENT, body LONGTEXT NOT NULL, bytes BLOB, nullable TEXT, empty TEXT, short VARCHAR(4) DEFAULT 'auto', number INT DEFAULT 42, computed INT AS (id + 1) VIRTUAL) ENGINE=InnoDB").await.unwrap();
            let schema = session
                .read_table("csv_import_test", "import`rows", &TableQuery::default())
                .await
                .unwrap()
                .schema;
            let csv = csv_fixture(&format!(
                "content,bytes,nullable,empty,ignored\n\"{}\n\"\"tail\"\"\",00FF,\\N,,discard\n",
                "世界".repeat(300)
            ))
            .unwrap();
            let mapping = vec![
                Some("body".into()),
                Some("bytes".into()),
                Some("nullable".into()),
                Some("empty".into()),
                None,
            ];
            assert_eq!(
                session
                    .import_csv("csv_import_test", "import`rows", &schema, &csv, &mapping)
                    .await
                    .unwrap(),
                1
            );
            let data = session
                .read_table("csv_import_test", "import`rows", &TableQuery::default())
                .await
                .unwrap();
            let full = session
                .read_row("csv_import_test", "import`rows", &schema, &data.keys[0])
                .await
                .unwrap();
            assert_eq!(
                full[1].as_ref().unwrap(),
                &format!("{}\n\"tail\"", "世界".repeat(300))
            );
            assert_eq!(&full[2..5], &csv.first_row()[1..4]);
            assert_eq!(full[5].as_deref(), Some("auto"));
            assert_eq!(full[6].as_deref(), Some("42"));
            let id = full[0].as_ref().unwrap();
            session
                .run_sql("csv_import_test", "SET SESSION sql_mode = ''")
                .await
                .unwrap();
            for (text, mapping) in [
                (
                    format!("id,body\n100,valid\n{id},duplicate"),
                    vec![Some("id".into()), Some("body".into())],
                ),
                (
                    "body,short\nvalid,ok\ninvalid,toolong".into(),
                    vec![Some("body".into()), Some("short".into())],
                ),
                (
                    "body,number\nvalid,10\ninvalid,not-a-number".into(),
                    vec![Some("body".into()), Some("number".into())],
                ),
                ("body\nvalid\n\\N".into(), vec![Some("body".into())]),
                (
                    "body,bytes\nvalid,00FF\ninvalid,xyz".into(),
                    vec![Some("body".into()), Some("bytes".into())],
                ),
                ("number\n10\n20".into(), vec![Some("number".into())]),
            ] {
                let csv = csv_fixture(&text).unwrap();
                assert!(
                    session
                        .import_csv("csv_import_test", "import`rows", &schema, &csv, &mapping)
                        .await
                        .is_err(),
                    "{text}"
                );
                let (rows, _) = session
                    .run_sql("csv_import_test", "SELECT COUNT(*) FROM `import``rows`")
                    .await
                    .unwrap();
                assert_eq!(rows.rows[0][0].as_deref(), Some("1"), "{text}");
            }
            let csv = csv_fixture("body\nvalid").unwrap();
            let body = [Some("body".into())];
            session
                .run_sql("csv_import_test", "START TRANSACTION")
                .await
                .unwrap();
            session
                .run_sql(
                    "csv_import_test",
                    "INSERT INTO `import``rows` (body) VALUES ('pending')",
                )
                .await
                .unwrap();
            assert!(
                session
                    .import_csv("csv_import_test", "import`rows", &schema, &csv, &body)
                    .await
                    .unwrap_err()
                    .contains("open SQL transaction")
            );
            let (rows, _) = session
                .run_sql("csv_import_test", "SELECT @@in_transaction")
                .await
                .unwrap();
            assert_eq!(rows.rows[0][0].as_deref(), Some("1"));
            session
                .run_sql("csv_import_test", "ROLLBACK")
                .await
                .unwrap();
            session
                .run_sql("csv_import_test", "SET autocommit = 0")
                .await
                .unwrap();
            assert!(
                session
                    .import_csv("csv_import_test", "import`rows", &schema, &csv, &body)
                    .await
                    .is_err()
            );
            session
                .run_sql("csv_import_test", "SET autocommit = 1")
                .await
                .unwrap();
            session
                .run_sql(
                    "csv_import_test",
                    "CREATE TABLE nontransactional (body TEXT) ENGINE=MyISAM",
                )
                .await
                .unwrap();
            let other = session
                .read_table(
                    "csv_import_test",
                    "nontransactional",
                    &TableQuery::default(),
                )
                .await
                .unwrap()
                .schema;
            assert!(
                session
                    .import_csv("csv_import_test", "nontransactional", &other, &csv, &body)
                    .await
                    .unwrap_err()
                    .contains("InnoDB")
            );
            session.run_sql("csv_import_test", "CREATE TRIGGER import_side_effect AFTER INSERT ON `import``rows` FOR EACH ROW INSERT INTO nontransactional VALUES (NEW.body)").await.unwrap();
            assert!(
                session
                    .import_csv("csv_import_test", "import`rows", &schema, &csv, &body)
                    .await
                    .unwrap_err()
                    .contains("triggers")
            );
            session
                .run_sql("csv_import_test", "DROP TRIGGER import_side_effect")
                .await
                .unwrap();
            session
                .run_sql(
                    "csv_import_test",
                    "CREATE USER 'csv_import_reader'@'%' IDENTIFIED BY ''",
                )
                .await
                .unwrap();
            session
                .run_sql(
                    "csv_import_test",
                    "GRANT SELECT, INSERT ON csv_import_test.* TO 'csv_import_reader'@'%'",
                )
                .await
                .unwrap();
            config.username = "csv_import_reader".into();
            let limited = connect(&config).await.unwrap();
            assert!(
                limited
                    .import_csv("csv_import_test", "import`rows", &schema, &csv, &body)
                    .await
                    .unwrap_err()
                    .contains("TRIGGER privilege")
            );
            session
                .run_sql(
                    "csv_import_test",
                    "GRANT TRIGGER ON csv_import_test.`import``rows` TO 'csv_import_reader'@'%'",
                )
                .await
                .unwrap();
            assert_eq!(
                limited
                    .import_csv("csv_import_test", "import`rows", &schema, &csv, &body)
                    .await
                    .unwrap(),
                1
            );
            limited.close().await;
            session
                .run_sql("csv_import_test", "DROP USER 'csv_import_reader'@'%'")
                .await
                .unwrap();
            session
                .run_sql(
                    "csv_import_test",
                    "ALTER TABLE `import``rows` MODIFY body VARCHAR(2000) NOT NULL",
                )
                .await
                .unwrap();
            assert!(
                session
                    .import_csv("csv_import_test", "import`rows", &schema, &csv, &body)
                    .await
                    .unwrap_err()
                    .contains("columns changed")
            );
            let schema = session
                .read_table("csv_import_test", "import`rows", &TableQuery::default())
                .await
                .unwrap()
                .schema;
            let csv = csv_fixture(&format!("body\n{}", "value\n".repeat(1005))).unwrap();
            assert_eq!(
                session
                    .import_csv("csv_import_test", "import`rows", &schema, &csv, &body)
                    .await
                    .unwrap(),
                1005
            );
            let (rows, _) = session
                .run_sql("csv_import_test", "SELECT COUNT(*) FROM `import``rows`")
                .await
                .unwrap();
            assert_eq!(rows.rows[0][0].as_deref(), Some("1007"));
            session
                .run_sql("mysql", "DROP DATABASE csv_import_test")
                .await
                .unwrap();
            session.close().await;
        });
    }
    #[test]
    fn csv_preserves_values_and_rejects_malformed_input_and_mapping() {
        let csv = csv_fixture("\u{feff}body,bytes,nullable,empty,literal\r\n\"世界,\"\"quoted\"\"\nline\",00FF,\\N,,\"\\N\"\r\n").unwrap();
        assert_eq!(
            csv.first_row(),
            [
                Some("世界,\"quoted\"\nline".into()),
                Some("00FF".into()),
                None,
                Some("".into()),
                Some("\\N".into())
            ]
        );
        for text in [
            "",
            "a\n",
            "a,a\nx,y",
            "a,b\nx",
            "a\n\"unclosed",
            "a\n\"x\"oops",
            "a\nx\"y",
            "a\rx",
            ",b\nx,y",
        ] {
            assert!(csv_fixture(text).is_err(), "{text:?}");
        }
        let schema = TableSchema {
            columns: vec!["text".into(), "blob".into(), "computed".into()],
            column_types: vec!["text".into(), "blob".into(), "int".into()],
            generated_columns: vec![2],
            ..TableSchema::default()
        };
        let mut mapping = vec![Some("text".into()), Some("blob".into()), None, None, None];
        assert_eq!(
            csv.validate_mapping(&schema, &mapping).unwrap(),
            [(0, 0), (1, 1)]
        );
        mapping[1] = Some("text".into());
        assert!(
            csv.validate_mapping(&schema, &mapping)
                .unwrap_err()
                .contains("more than once")
        );
        mapping[1] = Some("computed".into());
        assert!(csv.validate_mapping(&schema, &mapping).is_err());
        mapping.fill(None);
        assert!(csv.validate_mapping(&schema, &mapping).is_err());
        let invalid = csv_fixture("bytes\nxyz").unwrap();
        let columns = invalid
            .validate_mapping(&schema, &[Some("blob".into())])
            .unwrap();
        assert!(
            invalid
                .validate_row(&schema, &columns, invalid.first_row(), 2)
                .is_err()
        );
        let path =
            std::env::temp_dir().join(format!("tablelane-import-limit-{}.csv", std::process::id()));
        let file = File::create(&path).unwrap();
        file.set_len(16 * 1024 * 1024 * 1024).unwrap();
        drop(file);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .write_all(b"body\nfirst\n")
            .unwrap();
        let sparse = CsvImport::read(&path, 16 * 1024 * 1024).unwrap();
        assert_eq!(sparse.byte_len(), 16 * 1024 * 1024 * 1024);
        assert_eq!(sparse.first_row()[0].as_deref(), Some("first"));
        std::fs::write(&path, [0xff]).unwrap();
        assert!(CsvImport::read(&path, 16 * 1024 * 1024).is_err());
        let mut export = crate::data_export::ExportWriter::new(
            &path,
            crate::data_export::ExportFormat::Csv,
            csv.headers(),
        )
        .unwrap();
        export.push(csv.first_row()).unwrap();
        export.finish(&path).unwrap();
        let imported = CsvImport::read(&path, 16 * 1024 * 1024).unwrap();
        assert_eq!(imported.headers(), csv.headers());
        assert_eq!(imported.first_row(), csv.first_row());
        std::fs::remove_file(path).unwrap();
    }
}
