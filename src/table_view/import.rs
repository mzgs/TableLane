use super::*;
use crate::data_import::CsvImport;
use gpui_kit::component::WindowExt;
use std::sync::Arc;

struct ImportForm {
    owner: WeakEntity<TableView>,
    csv: Arc<CsvImport>,
    schema: TableSchema,
    mapping: Vec<Entity<SelectState<Vec<String>>>>,
    busy: bool,
    error: Option<String>,
}

impl ImportForm {
    fn new(
        csv: CsvImport,
        schema: TableSchema,
        owner: WeakEntity<TableView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let options = std::iter::once("Skip this column".to_owned())
            .chain(
                schema
                    .columns
                    .iter()
                    .enumerate()
                    .filter(|(ix, _)| schema.is_editable(*ix))
                    .map(|(_, name)| name.clone()),
            )
            .collect::<Vec<_>>();
        let mapping = csv
            .headers()
            .iter()
            .map(|header| {
                cx.new(|cx| {
                    SelectState::new(
                        options.clone(),
                        Some(component::IndexPath::new(
                            options
                                .iter()
                                .skip(1)
                                .position(|name| name == header)
                                .map_or(0, |ix| ix + 1),
                        )),
                        window,
                        cx,
                    )
                })
            })
            .collect::<Vec<_>>();
        for select in &mapping {
            cx.subscribe(select, |view, _, _: &SelectEvent<Vec<String>>, cx| {
                view.error = None;
                cx.notify();
            })
            .detach();
        }
        Self {
            owner,
            csv: Arc::new(csv),
            schema,
            mapping,
            busy: false,
            error: None,
        }
    }

    fn mapping(&self, cx: &App) -> Vec<Option<String>> {
        self.mapping
            .iter()
            .map(|select| {
                let select = select.read(cx);
                select
                    .selected_index(cx)
                    .filter(|ix| ix.row > 0)
                    .and_then(|_| select.selected_value().cloned())
            })
            .collect()
    }
}

impl Render for ImportForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let submit_owner = self.owner.clone();
        let submit_form = cx.entity();
        let cancel_owner = self.owner.clone();
        let cancel_form = cx.entity();
        let (rows, bytes) = self.csv.progress();
        let progress = format!(
            "Importing: {rows} rows processed · {:.1} / {:.1} MiB read",
            bytes as f64 / 1048576.,
            self.csv.byte_len() as f64 / 1048576.
        );
        div().flex().flex_col().gap_3().min_w_0()
            .child("Add rows from this CSV. Map headers to table columns; skipped columns use their database defaults. Existing rows are retained.")
            .child(div().text_sm().text_color(cx.theme().muted_foreground)
                .child("UTF-8, comma-separated, with headers. Unquoted \\N is NULL; empty fields are empty text. Binary columns use hex. Constraint errors and conversion warnings roll back the entire import."))
            .children(self.csv.headers().iter().zip(&self.mapping).enumerate().map(|(ix, (header, select))| {
                Field::new().label(header.clone()).child(Select::new(select)
                    .id(SharedString::from(format!("import-column-{header}")))
                    .accessibility_label(format!("Target column for {header}"))
                    .disabled(self.busy))
                    .description(format!("First value: {}", self.csv.first_row()[ix].as_ref().map(|value| format!("{:?}", truncate_preview(value.chars().take(201).collect()))).unwrap_or_else(|| "NULL".into())))
            }))
            .when_some(self.error.as_ref(), |view, error| {
                view.child(div().id("csv-import-error").test_support().role(Role::Status)
                    .aria_label(error.clone()).text_sm().text_color(cx.theme().danger).child(error.clone()))
            })
            .when(self.busy, |view| view.child(div().id("csv-import-progress").test_support().role(Role::Status)
                .aria_label(progress.clone()).child(progress)))
            .child(div().flex().justify_end().gap_2()
                .child(Button::new("csv-import-cancel").label("Cancel").disabled(self.busy)
                    .on_click(move |_, window, cx| {
                        if cancel_form.read(cx).busy { return; }
                        let _ = cancel_owner.update(cx, |view, cx| { view.importing = false; cx.notify(); });
                        window.close_dialog(cx);
                    }))
                .child(Button::new("csv-import-submit").primary().label("Import").disabled(self.busy)
                    .on_click(move |_, window, cx| {
                        let _ = submit_owner.update(cx, |view, cx| view.submit_import(submit_form.clone(), window, cx));
                    })))
    }
}

impl TableView {
    pub(super) fn choose_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_busy() || self.is_dirty(cx) || self.filter_pending {
            return;
        }
        let (Some(request), Some(table)) = (self.request.clone(), &self.table) else {
            return;
        };
        if !request.session.is_connected() {
            return;
        }
        let schema = table.read(cx).delegate().data.schema.clone();
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import CSV".into()),
        });
        self.importing = true;
        self.import_status = Some("Choose a CSV file…".into());
        self.import_failed = false;
        cx.notify();
        cx.spawn_in(window, async move |view, cx| {
            let result: anyhow::Result<Option<CsvImport>> = async {
                let Some(path) = picker.await??.and_then(|paths| paths.into_iter().next()) else {
                    return Ok(None);
                };
                cx.background_spawn(async move {
                    let limit = async_std::task::block_on(request.session.import_record_limit())
                        .map_err(anyhow::Error::msg)?;
                    CsvImport::read(&path, limit).map(Some)
                })
                .await
            }
            .await;
            let _ = view.update_in(cx, |view, window, cx| {
                match result {
                    Ok(Some(csv)) => {
                        view.import_status = None;
                        view.open_import(csv, schema, window, cx);
                    }
                    Ok(None) => {
                        view.importing = false;
                        view.import_status = None;
                    }
                    Err(error) => {
                        view.importing = false;
                        view.import_failed = true;
                        view.import_status = Some(format!(
                            "Couldn’t read CSV: {error}. No rows were imported."
                        ));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn open_import(
        &mut self,
        csv: CsvImport,
        schema: TableSchema,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let owner = cx.entity().downgrade();
        let form = cx.new(|cx| ImportForm::new(csv, schema, owner.clone(), window, cx));
        let title = format!("Import CSV · {}", self.title());
        window.open_dialog(cx, move |dialog, window, _| {
            let cancel = form.clone();
            let submit = form.clone();
            let submit_owner = owner.clone();
            let close_owner = owner.clone();
            let close_form = form.clone();
            dialog
                .title(title.clone())
                .width(window.rem_size() * 40.)
                .close_button(false)
                .overlay_closable(false)
                .child(form.clone())
                .on_cancel(move |_, _, cx| !cancel.read(cx).busy)
                .on_close(move |_, _, cx| {
                    if !close_form.read(cx).busy {
                        let _ = close_owner.update(cx, |view, cx| {
                            view.importing = false;
                            cx.notify();
                        });
                    }
                })
                .on_ok(move |_, window, cx| {
                    let _ = submit_owner.update(cx, |view, cx| {
                        view.submit_import(submit.clone(), window, cx)
                    });
                    false
                })
        });
    }

    fn submit_import(
        &mut self,
        form: Entity<ImportForm>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if form.read(cx).busy {
            return;
        }
        let mapping = form.read(cx).mapping(cx);
        if let Err(error) = form
            .read(cx)
            .csv
            .validate_mapping(&form.read(cx).schema, &mapping)
        {
            form.update(cx, |form, cx| {
                form.error = Some(error);
                cx.notify();
            });
            return;
        }
        let Some(request) = self.request.clone() else {
            return;
        };
        let csv = form.read(cx).csv.clone();
        let schema = form.read(cx).schema.clone();
        form.update(cx, |form, cx| {
            form.busy = true;
            form.error = None;
            cx.notify();
        });
        let progress_form = form.downgrade();
        cx.spawn_in(window, async move |_, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(250))
                    .await;
                let Ok(busy) = progress_form.update(cx, |form, cx| {
                    cx.notify();
                    form.busy
                }) else {
                    break;
                };
                if !busy {
                    break;
                }
            }
        })
        .detach();
        let task = cx.background_spawn(async move {
            async_std::task::block_on(request.session.import_csv(
                &request.database,
                &request.table,
                &schema,
                &csv,
                &mapping,
            ))
        });
        cx.spawn_in(window, async move |view, cx| {
            let result = task.await;
            let _ = view.update_in(cx, |view, window, cx| {
                match result {
                    Ok(rows) => {
                        view.importing = false;
                        view.import_failed = false;
                        view.import_status = Some(format!(
                            "Imported {rows} {}. Applied filters and sorting are retained.",
                            if rows == 1 { "row" } else { "rows" }
                        ));
                        window.close_dialog(cx);
                        let mut query = view.query.clone();
                        query.page = 0;
                        view.load(query, window, cx);
                    }
                    Err(error) => {
                        form.update(cx, |form, cx| {
                            form.busy = false;
                            form.error = Some(error);
                            cx.notify();
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        connections::OpenTable,
        database::{TableQuery, connect},
        settings::{Connection, DatabaseKind},
        table_view::TableView,
    };
    use gpui_kit::{
        AppContext, TestAppContext,
        component::{Root, WindowExt},
        px, size,
        test::{TestAppContextExt, TestWindowExt},
    };
    use std::{sync::Arc, time::Duration};

    #[gpui_kit::test]
    #[ignore = "requires an isolated MariaDB server and TABLELANE_TEST_MARIADB_PORT"]
    async fn mariadb_csv_import_picker_mapping_retry_and_guards(cx: &mut TestAppContext) {
        let session = Arc::new(
            async_std::task::block_on(connect(&Connection {
                name: "Import UI".into(),
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
            }))
            .unwrap(),
        );
        async_std::task::block_on(async {
            session
                .run_sql("mysql", "CREATE DATABASE csv_import_ui_test")
                .await
                .unwrap();
            session.run_sql("csv_import_ui_test", "CREATE TABLE widgets (id INT PRIMARY KEY AUTO_INCREMENT, body TEXT NOT NULL, bytes BLOB, computed INT AS (id + 1) VIRTUAL) ENGINE=InnoDB").await.unwrap();
        });
        let path =
            std::env::temp_dir().join(format!("tablelane-csv-ui-{}.csv", std::process::id()));
        std::fs::write(&path, "content,bytes\n\"世界,\"\"quoted\"\"\",00FF\n").unwrap();
        cx.update(gpui_kit::init);
        let mut table = None;
        let handle = cx.open_window(size(px(1000.), px(800.)), |window, cx| {
            let view = cx.new(|cx| {
                TableView::new(
                    OpenTable {
                        session: session.clone(),
                        connection_name: "Import UI".into(),
                        database: "csv_import_ui_test".into(),
                        table: "widgets".into(),
                    },
                    window,
                    cx,
                )
            });
            table = Some(view.clone());
            Root::new(view, window, cx)
        });
        let table = table.unwrap();
        let timeout = Duration::from_secs(15);
        cx.wait_for(handle.into(), timeout, |_, cx| !table.read(cx).loading)
            .await;
        let query = TableQuery {
            page: 1,
            filters: vec![crate::database::TableFilter {
                column: Some("body".into()),
                operator: crate::database::FilterOperator::Contains,
                value: "世界".into(),
            }],
            sort: Some(("id".into(), true)),
        };
        cx.update_window(handle.into(), |_, window, cx| {
            table.update(cx, |view, cx| view.load(query.clone(), window, cx))
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| !table.read(cx).loading)
            .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("import-table-csv", cx);
            assert!(table.read(cx).is_busy());
            assert!(!table.read(cx).can_change_schema(cx));
            assert!(!table.update(cx, |view, cx| view.prepare_to_close(cx)));
            window.click("import-table-csv", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.simulate_path_prompt_response(|options| {
            assert!(options.files && !options.directories && !options.multiple);
            None
        });
        cx.wait_for(handle.into(), timeout, |_, cx| !table.read(cx).importing)
            .await;
        // A malformed file never opens a form or touches the database.
        std::fs::write(&path, "body,bytes\nmissing-field").unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("import-table-csv", cx)
        })
        .unwrap();
        cx.run_until_parked();
        cx.simulate_path_prompt_response(|_| Some(vec![path.clone()]));
        cx.wait_for(handle.into(), timeout, |_, cx| !table.read(cx).importing)
            .await;
        cx.update_window(handle.into(), |_, window, _| {
            assert!(
                window
                    .find("table-import-status")
                    .label()
                    .unwrap()
                    .contains("expected 2")
            );
            assert!(window.try_find("csv-import-submit").is_none());
        })
        .unwrap();
        std::fs::write(&path, "content,bytes\n\"世界,\"\"quoted\"\"\",00FF\n").unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("import-table-csv", cx)
        })
        .unwrap();
        cx.run_until_parked();
        cx.simulate_path_prompt_response(|_| Some(vec![path.clone()]));
        cx.wait_for(handle.into(), timeout, |window, _| {
            window.try_find("csv-import-submit").is_some()
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            assert_eq!(
                window.find("import-column-content").value(),
                Some("Skip this column")
            );
            assert_eq!(window.find("import-column-bytes").value(), Some("bytes"));
            assert!(table.read(cx).is_busy());
            window.within("import-column-content").click("input", cx);
            window.press("down", cx);
            window.press("down", cx);
            window.press("enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            assert_eq!(window.find("import-column-content").value(), Some("body"));
            // Duplicate mapping validates in the form before any insert.
            window.within("import-column-bytes").click("input", cx);
            window.press("up", cx);
            window.press("enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("csv-import-submit", cx)
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(
                window
                    .find("csv-import-error")
                    .label()
                    .unwrap()
                    .contains("more than once")
            );
            window.within("import-column-bytes").click("input", cx);
            window.press("down", cx);
            window.press("enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
        // Failed SQL retains the form and mappings for another attempt.
        async_std::task::block_on(session.run_sql("csv_import_ui_test", "START TRANSACTION"))
            .unwrap();
        // Keep the real session occupied so busy controls can be checked before the fast rejection completes.
        let (connection_id, _) = async_std::task::block_on(
            session.run_sql("csv_import_ui_test", "SELECT CONNECTION_ID()"),
        )
        .unwrap();
        let connection_id: u64 = connection_id.rows[0][0].as_ref().unwrap().parse().unwrap();
        let pause_session = session.clone();
        let pause = std::thread::spawn(move || {
            async_std::task::block_on(
                pause_session.run_sql("csv_import_ui_test", "SELECT SLEEP(1)"),
            )
        });
        async_std::task::block_on(async {
            use sqlx::Connection as _;
            let port = std::env::var("TABLELANE_TEST_MARIADB_PORT").unwrap();
            let mut admin =
                sqlx::MySqlConnection::connect(&format!("mysql://root@127.0.0.1:{port}"))
                    .await
                    .unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                let info: Option<String> = sqlx::query_scalar(
                    "SELECT INFO FROM information_schema.PROCESSLIST WHERE ID = ?",
                )
                .bind(connection_id)
                .fetch_one(&mut admin)
                .await
                .unwrap();
                if info.as_deref() == Some("SELECT SLEEP(1)") {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "session did not start its pause query"
                );
                async_std::task::sleep(Duration::from_millis(10)).await;
            }
        });
        cx.update_window(handle.into(), |_, window, cx| {
            assert_eq!(window.find("import-column-bytes").value(), Some("bytes"));
            window.click("csv-import-submit", cx);
            assert!(table.read(cx).is_busy());
        })
        .unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("csv-import-progress").is_some());
            window.click("csv-import-submit", cx);
            window.click("csv-import-cancel", cx);
            window.press("escape", cx);
            assert!(table.read(cx).is_busy());
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |window, _| {
            window.try_find("csv-import-error").is_some()
        })
        .await;
        pause.join().unwrap().unwrap();
        cx.update_window(handle.into(), |_, window, _| {
            assert!(
                window
                    .find("csv-import-error")
                    .label()
                    .unwrap()
                    .contains("open SQL transaction")
            );
            assert_eq!(window.find("import-column-content").value(), Some("body"));
        })
        .unwrap();
        async_std::task::block_on(session.run_sql("csv_import_ui_test", "ROLLBACK")).unwrap();
        cx.update_window(handle.into(), |_, window, cx| window.press("enter", cx))
            .unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| !table.read(cx).is_busy())
            .await;
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(window.try_find("csv-import-submit").is_none());
            assert_eq!(table.read(cx).query.page, 0);
            assert_eq!(table.read(cx).query.filters, query.filters);
            assert_eq!(table.read(cx).query.sort, query.sort);
            assert!(
                window
                    .find("table-import-status")
                    .label()
                    .unwrap()
                    .starts_with("Imported 1 row.")
            );
            assert_eq!(
                table
                    .read(cx)
                    .table
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .delegate()
                    .data
                    .rows
                    .len(),
                1
            );
        })
        .unwrap();
        let data = async_std::task::block_on(session.read_table(
            "csv_import_ui_test",
            "widgets",
            &TableQuery::default(),
        ))
        .unwrap();
        assert_eq!(data.rows[0][1].as_deref(), Some("世界,\"quoted\""));
        assert_eq!(data.rows[0][2].as_deref(), Some("00FF"));
        // Escape cancels an idle form and leaves imported rows intact.
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("import-table-csv", cx)
        })
        .unwrap();
        cx.run_until_parked();
        cx.simulate_path_prompt_response(|_| Some(vec![path.clone()]));
        cx.wait_for(handle.into(), timeout, |window, _| {
            window.try_find("csv-import-submit").is_some()
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| window.press("escape", cx))
            .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(!table.read(cx).is_busy());
            assert!(window.try_find("csv-import-submit").is_none());
            window.close_dialog(cx);
        })
        .unwrap();
        async_std::task::block_on(session.run_sql("mysql", "DROP DATABASE csv_import_ui_test"))
            .unwrap();
        async_std::task::block_on(session.close());
        std::fs::remove_file(path).unwrap();
    }
}
