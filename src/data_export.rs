use anyhow::{Context, Result, ensure};
use ring::rand::{SecureRandom, SystemRandom};
use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
};

#[derive(Clone, Copy)]
pub(crate) enum ExportFormat {
    Csv,
    Json,
}

impl ExportFormat {
    pub(crate) fn extension(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Json => "json",
        }
    }
}

pub(crate) struct TemporaryFile(PathBuf);

impl TemporaryFile {
    pub(crate) fn new(directory: &Path) -> Result<(Self, File)> {
        let mut nonce = [0; 16];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| anyhow::anyhow!("Couldn’t generate a temporary file name."))?;
        let name = nonce
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let path = directory.join(format!(".tablelane-{name}.tmp"));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&path)?;
        Ok((Self(path), file))
    }

    #[cfg(test)]
    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TemporaryFile {
    // shortcut: cleanup runs on drop; add startup cleanup for temporary files left by a crash.
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

pub(crate) struct ExportWriter {
    writer: BufWriter<File>,
    temporary: TemporaryFile,
    format: ExportFormat,
    columns: usize,
    rows: usize,
}

impl ExportWriter {
    pub(crate) fn new(path: &Path, format: ExportFormat, columns: &[String]) -> Result<Self> {
        let directory = path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let (temporary, file) = TemporaryFile::new(directory)?;
        let mut writer = BufWriter::new(file);
        match format {
            ExportFormat::Csv => write_csv(
                &mut writer,
                &columns.iter().cloned().map(Some).collect::<Vec<_>>(),
            )?,
            ExportFormat::Json => {
                writer.write_all(b"{\"columns\":")?;
                serde_json::to_writer(&mut writer, columns)?;
                writer.write_all(b",\"rows\":[")?;
            }
        }
        Ok(Self {
            temporary,
            writer,
            format,
            columns: columns.len(),
            rows: 0,
        })
    }

    pub(crate) fn push(&mut self, row: &[Option<String>]) -> Result<()> {
        ensure!(
            row.len() == self.columns,
            "The row no longer matches the export columns."
        );
        match self.format {
            ExportFormat::Csv => write_csv(&mut self.writer, row)?,
            ExportFormat::Json => {
                if self.rows > 0 {
                    self.writer.write_all(b",")?;
                }
                serde_json::to_writer(&mut self.writer, row)?;
            }
        }
        self.rows += 1;
        Ok(())
    }

    pub(crate) fn finish(mut self, path: &Path) -> Result<usize> {
        if matches!(self.format, ExportFormat::Json) {
            self.writer.write_all(b"]}\n")?;
        }
        self.writer.flush()?;
        self.writer.get_ref().sync_all()?;
        drop(self.writer);
        fs::rename(&self.temporary.0, path).context("Couldn’t publish the export file")?;
        Ok(self.rows)
    }
}

fn write_csv(writer: &mut impl Write, row: &[Option<String>]) -> Result<()> {
    for (ix, value) in row.iter().enumerate() {
        if ix > 0 {
            writer.write_all(b",")?;
        }
        match value {
            Some(value) => {
                writer.write_all(b"\"")?;
                writer.write_all(value.replace('"', "\"\"").as_bytes())?;
                writer.write_all(b"\"")?;
            }
            None => writer.write_all(b"\\N")?,
        }
    }
    writer.write_all(b"\r\n")?;
    Ok(())
}

// The completed result is captured once, so exporting never repeats SQL side effects.
pub(crate) struct QueryExport {
    writer: BufWriter<File>,
    temporary: TemporaryFile,
    columns: Vec<String>,
    rows: usize,
}

impl QueryExport {
    pub(crate) fn new() -> Result<Self> {
        let (temporary, file) = TemporaryFile::new(&std::env::temp_dir())?;
        Ok(Self {
            temporary,
            writer: BufWriter::new(file),
            columns: Vec::new(),
            rows: 0,
        })
    }

    pub(crate) fn set_columns(&mut self, columns: &[String]) {
        self.columns = columns.to_vec();
    }

    pub(crate) fn push(&mut self, row: &[Option<String>]) -> Result<()> {
        ensure!(
            row.len() == self.columns.len(),
            "The row no longer matches the result columns."
        );
        serde_json::to_writer(&mut self.writer, row)?;
        self.writer.write_all(b"\n")?;
        self.rows += 1;
        Ok(())
    }

    pub(crate) fn finish(&mut self) -> Result<()> {
        self.writer.flush()?;
        Ok(())
    }

    pub(crate) fn row_count(&self) -> usize {
        self.rows
    }

    pub(crate) fn export(&self, path: &Path, format: ExportFormat) -> Result<usize> {
        let mut writer = ExportWriter::new(path, format, &self.columns)?;
        for line in BufReader::new(File::open(&self.temporary.0)?).lines() {
            let row: Vec<Option<String>> = serde_json::from_str(&line?)?;
            writer.push(&row)?;
        }
        ensure!(
            writer.rows == self.rows,
            "The saved result is incomplete. Export could not be completed."
        );
        writer.finish(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exports_full_values_nulls_duplicate_columns_and_publishes_only_on_success() {
        let (directory, file) = TemporaryFile::new(&std::env::temp_dir()).unwrap();
        drop(file);
        fs::remove_file(&directory.0).unwrap();
        fs::create_dir(&directory.0).unwrap();
        let path = directory.0.join("result.json");
        fs::write(&path, "original").unwrap();
        let columns = vec![
            "a,\"b".into(),
            "a,\"b".into(),
            "empty".into(),
            "literal".into(),
        ];
        let row = vec![
            Some(format!("{}\n\"tail\"", "世界".repeat(500))),
            None,
            Some(String::new()),
            Some("\\N".into()),
        ];
        {
            let mut writer = ExportWriter::new(&path, ExportFormat::Json, &columns).unwrap();
            writer.push(&row).unwrap();
            assert!(writer.push(&[]).is_err());
        }
        assert_eq!(fs::read_to_string(&path).unwrap(), "original");
        let mut snapshot = QueryExport::new().unwrap();
        let temporary = snapshot.temporary.0.clone();
        snapshot.set_columns(&columns);
        for _ in 0..1001 {
            snapshot.push(&row).unwrap();
        }
        snapshot.finish().unwrap();
        assert_eq!(snapshot.export(&path, ExportFormat::Json).unwrap(), 1001);
        let json: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(json["columns"], serde_json::json!(columns));
        assert_eq!(json["rows"][1000], serde_json::json!(row));
        let csv = directory.0.join("result.csv");
        snapshot.export(&csv, ExportFormat::Csv).unwrap();
        let csv_text = fs::read_to_string(&csv).unwrap();
        assert!(csv_text.starts_with("\"a,\"\"b\",\"a,\"\"b\",\"empty\",\"literal\"\r\n"));
        assert!(csv_text.contains("\n\"\"tail\"\"\",\\N,\"\",\"\\N\"\r\n"));
        drop(snapshot);
        assert!(!temporary.exists());
        let empty = directory.0.join("empty.json");
        ExportWriter::new(&empty, ExportFormat::Json, &columns)
            .unwrap()
            .finish(&empty)
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&fs::read(&empty).unwrap()).unwrap();
        assert_eq!(json["rows"], serde_json::json!([]));
        fs::remove_dir_all(&directory.0).unwrap();
    }
}
