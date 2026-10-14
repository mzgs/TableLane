use super::*;
use std::{
    future::Future,
    sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst},
    task::Poll,
    time::Duration,
};

/// One SQL execution, including time waiting for the shared connection.
pub(crate) struct QueryCancellation {
    marker: String,
    requested: AtomicBool,
    finished: AtomicBool,
    interrupted: AtomicBool,
    discarded: AtomicBool,
    started: AtomicBool,
    gate: async_std::sync::Mutex<()>,
    signal: (
        async_std::channel::Sender<()>,
        async_std::channel::Receiver<()>,
    ),
}

impl QueryCancellation {
    pub(crate) fn new() -> Self {
        static NEXT_RUN: AtomicU64 = AtomicU64::new(1);
        Self {
            marker: format!("/* tablelane-run-{} */", NEXT_RUN.fetch_add(1, SeqCst)),
            requested: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            interrupted: AtomicBool::new(false),
            discarded: AtomicBool::new(false),
            started: AtomicBool::new(false),
            gate: async_std::sync::Mutex::new(()),
            signal: async_std::channel::bounded(1),
        }
    }

    pub(crate) fn is_result_discarded(&self) -> bool {
        self.discarded.load(SeqCst)
    }

    pub(super) fn tagged_sql(&self, sql: &str) -> String {
        format!("{}\n{sql}", self.marker)
    }

    pub(super) fn discard_completed(&self) {
        self.discarded.store(true, SeqCst);
    }

    pub(crate) async fn cancel(&self, session: &Session) -> Result<(), String> {
        let _gate = self.gate.lock().await;
        if self.finished.load(SeqCst) {
            return Ok(());
        }
        self.requested.store(true, SeqCst);
        let _ = self.signal.0.try_send(());
        async_std::future::timeout(Duration::from_secs(5), async {
            let mut control = MySqlConnection::connect_with(&session.options).await?;
            loop {
                if self.finished.load(SeqCst) {
                    return Ok(());
                }
                let query_id: Option<u64> = sqlx::query_scalar(
                    "SELECT CAST(QUERY_ID AS UNSIGNED) FROM information_schema.PROCESSLIST WHERE ID = ? AND LEFT(INFO, ?) = ?",
                )
                .bind(session.thread_id)
                .bind(self.marker.len() as u64)
                .bind(&self.marker)
                .fetch_optional(&mut control)
                .await?;
                if let Some(query_id) = query_id {
                    // An exact query ID cannot cancel a later command on this session.
                    match sqlx::query(&format!("KILL SOFT QUERY ID {query_id}"))
                        .execute(&mut control)
                        .await
                    {
                        Ok(_) => {
                            self.interrupted.store(true, SeqCst);
                            return Ok(());
                        }
                        Err(sqlx::Error::Database(error))
                            if error.try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>()
                                .is_some_and(|error| error.number() == 1957) =>
                        {
                            // The matching query finished between lookup and KILL.
                            return Ok(());
                        }
                        Err(error) => return Err(error),
                    }
                }
                async_std::task::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .map_err(|_| "Cancellation timed out. SQL may still be running; try Cancel again.".to_owned())?
        .map_err(|error: sqlx::Error| format!("Couldn’t cancel SQL: {error}. SQL may still be running; try again."))
    }

    pub(super) fn is_requested(&self) -> bool {
        self.requested.load(SeqCst)
    }

    pub(super) fn check_before_execution(&self) -> Result<(), sqlx::Error> {
        if self.is_requested() {
            if !self.started.load(SeqCst) {
                self.discarded.store(true, SeqCst);
            }
            Err(sqlx::Error::Protocol(
                "SQL cancelled before execution.".into(),
            ))
        } else {
            Ok(())
        }
    }

    pub(super) fn mark_started(&self) {
        self.started.store(true, SeqCst);
    }

    pub(super) fn mark_interrupted(&self, error: &sqlx::Error) {
        if self.is_requested()
            && error.as_database_error().is_some_and(|error| {
                error
                    .try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>()
                    .is_some_and(|error| error.number() == 1317)
            })
        {
            self.interrupted.store(true, SeqCst);
        }
    }

    pub(super) async fn finish(&self, session: &Session) -> Option<String> {
        self.finished.store(true, SeqCst);
        // Wait for the control request before publishing the execution outcome.
        let _gate = self.gate.lock().await;
        if !session.is_connected() || !self.is_requested() {
            return None;
        }
        if self.discarded.load(SeqCst) {
            Some("SQL cancelled before execution.".into())
        } else if self.interrupted.load(SeqCst) {
            self.discarded.store(true, SeqCst);
            Some(
                "SQL cancelled. Check writes before retrying; existing transactions remain open."
                    .into(),
            )
        } else {
            None
        }
    }
}

impl Session {
    pub(super) async fn query_connection(
        &self,
        cancellation: Option<&QueryCancellation>,
    ) -> Result<async_std::sync::MutexGuard<'_, Option<MySqlConnection>>, sqlx::Error> {
        let Some(cancellation) = cancellation else {
            return self.connection().await;
        };
        let mut connection = std::pin::pin!(self.connection());
        let mut signal = std::pin::pin!(cancellation.signal.1.recv());
        std::future::poll_fn(|cx| {
            if let Err(error) = cancellation.check_before_execution() {
                return Poll::Ready(Err(error));
            }
            if let Poll::Ready(result) = connection.as_mut().poll(cx) {
                return Poll::Ready(result);
            }
            if signal.as_mut().poll(cx).is_ready() {
                cancellation.discarded.store(true, SeqCst);
                return Poll::Ready(Err(sqlx::Error::Protocol(
                    "SQL cancelled before execution.".into(),
                )));
            }
            Poll::Pending
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    async fn wait_for_query(session: &Session, cancellation: &QueryCancellation) {
        async_std::future::timeout(Duration::from_secs(5), async {
            let mut observer = MySqlConnection::connect_with(&session.options).await.unwrap();
            loop {
                let running: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM information_schema.PROCESSLIST WHERE ID = ? AND LEFT(INFO, ?) = ? AND COMMAND = 'Query'")
                    .bind(session.thread_id).bind(cancellation.marker.len() as u64).bind(&cancellation.marker)
                    .fetch_one(&mut observer).await.unwrap();
                if running != 0 { break; }
                async_std::task::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("SQL never reached the server");
    }

    fn config() -> Connection {
        Connection {
            name: "Cancellation".into(),
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
        }
    }

    #[test]
    #[ignore = "requires an isolated MariaDB server and TABLELANE_TEST_MARIADB_PORT"]
    fn mariadb_sql_cancellation_stops_only_its_run_and_preserves_transactions() {
        async_std::task::block_on(async {
            let session = Arc::new(connect(&config()).await.unwrap());
            let db = "cancel_sql_test";
            session
                .run_sql("mysql", "CREATE DATABASE cancel_sql_test")
                .await
                .unwrap();
            session
                .run_sql(
                    db,
                    "CREATE TABLE widgets (id INT PRIMARY KEY, body TEXT) ENGINE=InnoDB",
                )
                .await
                .unwrap();
            session
                .run_sql(db, "INSERT INTO widgets VALUES (1, 'original')")
                .await
                .unwrap();
            session
                .run_sql(db, "SET @preserved = 42, @queued_runs = 0")
                .await
                .unwrap();
            session
                .run_sql(db, "CREATE TEMPORARY TABLE temporary_rows (id INT)")
                .await
                .unwrap();
            session
                .run_sql(db, "INSERT INTO temporary_rows VALUES (7)")
                .await
                .unwrap();
            // Cancellation wakes a queued run without interrupting the lock owner.
            let guard = session.connection.lock().await;
            let queued = Arc::new(QueryCancellation::new());
            let task_session = session.clone();
            let task_control = queued.clone();
            let task = async_std::task::spawn(async move {
                task_session
                    .run_sql_export(db, "SET @queued_runs = @queued_runs + 1", &task_control)
                    .await
            });
            queued.cancel(&session).await.unwrap();
            let result = async_std::future::timeout(Duration::from_secs(2), task)
                .await
                .unwrap();
            assert!(result.err().unwrap().contains("cancelled before execution"));
            assert!(queued.is_result_discarded());
            drop(guard);
            assert_eq!(
                session
                    .run_sql(db, "SELECT @queued_runs")
                    .await
                    .unwrap()
                    .0
                    .rows[0][0]
                    .as_deref(),
                Some("0")
            );

            session.run_sql(db, "START TRANSACTION").await.unwrap();
            session
                .run_sql(db, "INSERT INTO widgets VALUES (2, 'uncommitted')")
                .await
                .unwrap();
            for sql in [
                "SELECT SLEEP(30)",
                "UPDATE widgets SET body = CONCAT('changed', SLEEP(30)) WHERE id = 1",
                "SELECT seq, SLEEP(0.005), REPEAT('x', 1000) FROM seq_1_to_10000",
            ] {
                let cancellation = Arc::new(QueryCancellation::new());
                let task_session = session.clone();
                let task_control = cancellation.clone();
                let task = async_std::task::spawn(async move {
                    task_session.run_sql_export(db, sql, &task_control).await
                });
                wait_for_query(&session, &cancellation).await;
                async_std::task::sleep(Duration::from_millis(50)).await;
                cancellation.cancel(&session).await.unwrap();
                let result = async_std::future::timeout(Duration::from_secs(5), task)
                    .await
                    .unwrap();
                assert!(result.err().unwrap().starts_with("SQL cancelled"));
                assert!(cancellation.is_result_discarded());
                session.check_health().await;
                assert!(session.is_connected());
                let values = session.run_sql(db, "SELECT CONNECTION_ID(), @preserved, @@in_transaction, (SELECT id FROM temporary_rows), (SELECT body FROM widgets WHERE id = 1), (SELECT body FROM widgets WHERE id = 2)").await.unwrap().0;
                assert_eq!(
                    values.rows[0],
                    [
                        Some(session.thread_id.to_string()),
                        Some("42".into()),
                        Some("1".into()),
                        Some("7".into()),
                        Some("original".into()),
                        Some("uncommitted".into())
                    ]
                );
                // A completed handle cannot cancel a subsequent statement.
                cancellation.cancel(&session).await.unwrap();
                let new_run = QueryCancellation::new();
                let result = session
                    .run_sql_export(db, "SELECT SLEEP(0.05), 123", &new_run)
                    .await
                    .unwrap();
                assert_eq!(result.0.rows[0][1].as_deref(), Some("123"));
            }
            session.run_sql(db, "ROLLBACK").await.unwrap();
            // Cancel a later statement, discard earlier snapshots, and never run the remainder.
            let cancellation = Arc::new(QueryCancellation::new());
            let task_session = session.clone();
            let task_control = cancellation.clone();
            let task = async_std::task::spawn(async move {
                task_session.run_sql_script(db, "SET @script_before = 1, @script_after = 0; SELECT 7; SELECT SLEEP(30); SET @script_after = 1;", &task_control).await
            });
            let mut observer = MySqlConnection::connect_with(&session.options)
                .await
                .unwrap();
            async_std::future::timeout(Duration::from_secs(5), async {
                loop {
                    let running: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM information_schema.PROCESSLIST WHERE ID = ? AND INFO LIKE '%SLEEP(30)%' AND COMMAND = 'Query'")
                        .bind(session.thread_id).fetch_one(&mut observer).await.unwrap();
                    if running != 0 { break; }
                    async_std::task::sleep(Duration::from_millis(10)).await;
                }
            }).await.unwrap();
            cancellation.cancel(&session).await.unwrap();
            assert!(
                async_std::future::timeout(Duration::from_secs(5), task)
                    .await
                    .unwrap()
                    .err()
                    .unwrap()
                    .starts_with("SQL cancelled")
            );
            assert!(cancellation.is_result_discarded());
            assert_eq!(
                session
                    .run_sql(db, "SELECT @script_before, @script_after")
                    .await
                    .unwrap()
                    .0
                    .rows[0],
                [Some("1".into()), Some("0".into())]
            );
            assert_eq!(
                session
                    .run_sql(db, "SELECT COUNT(*) FROM widgets")
                    .await
                    .unwrap()
                    .0
                    .rows[0][0]
                    .as_deref(),
                Some("1")
            );

            // Own-query cancellation needs no PROCESS or CONNECTION ADMIN grant.
            session
                .run_sql(db, "CREATE USER 'cancel_reader'@'%' IDENTIFIED BY ''")
                .await
                .unwrap();
            session
                .run_sql(
                    db,
                    "GRANT SELECT ON cancel_sql_test.* TO 'cancel_reader'@'%'",
                )
                .await
                .unwrap();
            let mut restricted_config = config();
            restricted_config.username = "cancel_reader".into();
            let restricted = Arc::new(connect(&restricted_config).await.unwrap());
            let cancellation = Arc::new(QueryCancellation::new());
            let task_session = restricted.clone();
            let task_control = cancellation.clone();
            let task = async_std::task::spawn(async move {
                task_session
                    .run_sql_export(db, "SELECT SLEEP(30)", &task_control)
                    .await
            });
            let mut observer = MySqlConnection::connect_with(&session.options)
                .await
                .unwrap();
            async_std::future::timeout(Duration::from_secs(5), async {
                loop {
                    let running: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM information_schema.PROCESSLIST WHERE ID = ? AND INFO LIKE '%SLEEP(30)%' AND COMMAND = 'Query'")
                        .bind(restricted.thread_id).fetch_one(&mut observer).await.unwrap();
                    if running != 0 { break; }
                    async_std::task::sleep(Duration::from_millis(10)).await;
                }
            }).await.unwrap();
            cancellation.cancel(&restricted).await.unwrap();
            assert!(
                async_std::future::timeout(Duration::from_secs(5), task)
                    .await
                    .unwrap()
                    .is_err()
            );
            assert!(restricted.run_sql(db, "SELECT 1").await.is_ok());
            restricted.close().await;
            session
                .run_sql(db, "DROP USER 'cancel_reader'@'%'")
                .await
                .unwrap();
            session
                .run_sql("mysql", "DROP DATABASE cancel_sql_test")
                .await
                .unwrap();
        });
    }

    #[test]
    #[ignore = "requires an isolated MariaDB server and TABLELANE_TEST_MARIADB_PORT"]
    fn mariadb_sql_cancel_failure_and_late_request_leave_session_usable() {
        async_std::task::block_on(async {
            let mut session = connect(&config()).await.unwrap();
            // Only the separate control connection fails authentication.
            session.options = session.options.clone().password("wrong-password");
            let session = Arc::new(session);
            let cancellation = Arc::new(QueryCancellation::new());
            let task_session = session.clone();
            let task_control = cancellation.clone();
            let task = async_std::task::spawn(async move {
                task_session
                    .run_sql_export("mysql", "SELECT SLEEP(0.5)", &task_control)
                    .await
            });
            let mut observer = MySqlConnection::connect_with(
                &connection_url(&config()).unwrap().as_str().parse().unwrap(),
            )
            .await
            .unwrap();
            async_std::future::timeout(Duration::from_secs(5), async {
                loop {
                    let running: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM information_schema.PROCESSLIST WHERE ID = ? AND INFO LIKE '%SLEEP(0.5)%' AND COMMAND = 'Query'")
                        .bind(session.thread_id).fetch_one(&mut observer).await.unwrap();
                    if running != 0 { break; }
                    async_std::task::sleep(Duration::from_millis(10)).await;
                }
            }).await.unwrap();
            assert!(
                cancellation
                    .cancel(&session)
                    .await
                    .unwrap_err()
                    .contains("Couldn’t cancel SQL")
            );
            let result = async_std::future::timeout(Duration::from_secs(5), task)
                .await
                .unwrap();
            assert!(
                result
                    .err()
                    .unwrap()
                    .starts_with("SQL finished after cancellation")
            );
            assert!(session.is_connected());
            assert!(session.run_sql("mysql", "SELECT 99").await.is_ok());

            let completed = QueryCancellation::new();
            let (_, _, export) = session
                .run_sql_export("mysql", "SELECT 'complete'", &completed)
                .await
                .unwrap();
            completed.cancel(&session).await.unwrap();
            assert!(!completed.is_requested());
            assert_eq!(export.row_count(), 1);
            assert!(session.run_sql("mysql", "SELECT SLEEP(0.01)").await.is_ok());
        });
    }
}
