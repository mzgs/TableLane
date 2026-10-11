use crate::{connections::OpenTable, database::TableRows};
use gpui_kit::{
    component::{
        ActiveTheme, Disableable, Icon, Sizable,
        button::Button,
        input::{Editor, EditorState, InputEvent},
        table::{Column, DataTable, TableDelegate, TableState},
    },
    prelude::FluentBuilder,
    *,
};

actions!(sql_editor, [RunSql, ToggleSqlComment]);

fn sql_comment_edit(
    sql: &str,
    mut selection: std::ops::Range<usize>,
) -> (std::ops::Range<usize>, String, std::ops::Range<usize>) {
    let start = sql[..selection.start].rfind('\n').map_or(0, |ix| ix + 1);
    let last = if selection.end > selection.start && sql[..selection.end].ends_with('\n') {
        selection.end - 1
    } else {
        selection.end
    };
    let end = sql[last..].find('\n').map_or(sql.len(), |ix| last + ix);
    let lines: Vec<_> = sql[start..end].split('\n').collect();
    let uncomment = lines.iter().all(|line| {
        line.trim_start_matches([' ', '\t'])
            .strip_prefix("--")
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
    });
    let mut replacement = sql[start..end].to_string();
    let mut offset = end;
    for line in lines.into_iter().rev() {
        offset -= line.len();
        let content = line.trim_start_matches([' ', '\t']);
        let at = offset + line.len() - content.len();
        let removed = if uncomment {
            if content.starts_with("-- ") { 3 } else { 2 }
        } else {
            0
        };
        let prefix = if uncomment { "" } else { "-- " };
        replacement.replace_range(at - start..at - start + removed, prefix);
        for point in [&mut selection.start, &mut selection.end] {
            if *point >= at {
                *point = *point - (*point - at).min(removed) + prefix.len();
            }
        }
        offset = offset.saturating_sub(1);
    }
    (start..end, replacement, selection)
}

struct Results {
    columns: Vec<Column>,
    data: TableRows,
}

impl TableDelegate for Results {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }
    fn rows_count(&self, _: &App) -> usize {
        self.data.rows.len()
    }
    fn column(&self, ix: usize, _: &App) -> Column {
        self.columns[ix].clone()
    }
    fn render_td(
        &mut self,
        row: usize,
        column: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let value = &self.data.rows[row][column];
        div()
            .truncate()
            .px_2()
            .when(value.is_none(), |cell| {
                cell.text_color(cx.theme().muted_foreground)
            })
            .child(value.clone().unwrap_or_else(|| "NULL".into()))
    }
    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        div()
            .p_3()
            .text_color(cx.theme().muted_foreground)
            .child("The statement returned no rows")
    }
}

pub(crate) struct SqlEditor {
    request: OpenTable,
    input: Entity<EditorState>,
    results: Option<Entity<TableState<Results>>>,
    running: bool,
    status: String,
    failed: bool,
}

impl SqlEditor {
    pub(crate) fn new(request: OpenTable, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.bind_keys([
            KeyBinding::new("cmd-enter", RunSql, Some("SqlEditor")),
            KeyBinding::new("ctrl-enter", RunSql, Some("SqlEditor")),
            KeyBinding::new("cmd-/", ToggleSqlComment, Some("SqlEditor")),
        ]);
        let input = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("sql")
                .placeholder("Enter one SQL statement")
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        cx.subscribe(&input, |_, _, _: &InputEvent, cx| cx.notify())
            .detach();
        Self {
            request,
            input,
            results: None,
            running: false,
            status: "Run a statement to see its results".into(),
            failed: false,
        }
    }

    pub(crate) fn is_running(&self) -> bool {
        self.running
    }

    pub(crate) fn focus(&self, window: &mut Window, cx: &mut App) {
        self.input.read(cx).focus_handle(cx).focus(window, cx);
    }

    pub(crate) fn request(&self) -> &OpenTable {
        &self.request
    }

    pub(crate) fn has_draft(&self, cx: &App) -> bool {
        !self.input.read(cx).value().trim().is_empty()
    }

    fn toggle_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.running || !self.input.read(cx).focus_handle(cx).is_focused(window) {
            return;
        }
        self.input.update(cx, |input, cx| {
            let (range, text, selection) = sql_comment_edit(&input.value(), input.selected_range());
            input.set_selected_range(range, cx);
            input.replace(text, window, cx);
            input.set_selected_range(selection, cx);
        });
    }

    fn run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let sql = self.input.read(cx).value().to_string();
        if self.running || sql.trim().is_empty() {
            return;
        }
        self.running = true;
        self.failed = false;
        self.status = "Running SQL…".into();
        self.results = None;
        let request = self.request.clone();
        let task = cx.background_spawn(async move {
            let start = std::time::Instant::now();
            let result =
                async_std::task::block_on(request.session.run_sql(&request.database, &sql));
            (result, start.elapsed())
        });
        cx.spawn_in(window, async move |editor, cx| {
            let (result, elapsed) = task.await;
            let _ = editor.update_in(cx, |editor, window, cx| {
                editor.running = false;
                match result {
                    Ok((data, affected)) => {
                        editor.status = format!(
                            "{} rows{} · {affected} rows affected · {:.2} s",
                            data.rows.len(),
                            if data.has_more {
                                " (first 1,000 shown)"
                            } else {
                                ""
                            },
                            elapsed.as_secs_f64()
                        );
                        if !data.schema.columns.is_empty() {
                            let columns = data
                                .schema
                                .columns
                                .iter()
                                .enumerate()
                                .map(|(ix, name)| {
                                    Column::new(format!("sql-column-{ix}"), name.clone())
                                        .width(rems(12.).to_pixels(window.rem_size()))
                                        .movable(false)
                                })
                                .collect();
                            editor.results = Some(cx.new(|cx| {
                                TableState::new(Results { columns, data }, window, cx)
                                    .col_movable(false)
                                    .cell_selectable(true)
                                    .row_header(false)
                            }));
                        }
                    }
                    Err(error) => {
                        editor.status = error;
                        editor.failed = true;
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

impl Render for SqlEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .min_h_0()
            .min_w_0()
            .key_context("SqlEditor")
            .capture_action(cx.listener(
                |editor, action: &gpui_kit::component::input::Enter, window, cx| {
                    if action.secondary && !action.shift {
                        cx.stop_propagation();
                        editor.run(window, cx);
                    }
                },
            ))
            .on_action(cx.listener(|editor, _: &RunSql, window, cx| editor.run(window, cx)))
            .on_action(cx.listener(|editor, _: &ToggleSqlComment, window, cx| {
                editor.toggle_comment(window, cx)
            }))
            .child(
                div()
                    .id("sql-database")
                    .test_support()
                    .aria_label(format!("Selected database: {}", self.request.database))
                    .px_3()
                    .py_2()
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .text_sm()
                    .child(
                        Icon::new(gpui_kit::assets::IconName::Database)
                            .small()
                            .flex_shrink_0(),
                    )
                    .child(div().truncate().child(self.request.database.clone())),
            )
            .child(
                div().id("sql-input").p_3().flex_shrink_0().child(
                    Editor::new(&self.input)
                        .aria_label("SQL statement")
                        .h(rems(10.))
                        .disabled(self.running),
                ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .pb_2()
                    .flex_shrink_0()
                    .child(
                        Button::new("run-sql")
                            .small()
                            .label("Run")
                            .tooltip("Run SQL (Cmd/Ctrl+Enter)")
                            .loading(self.running)
                            .disabled(self.running || self.input.read(cx).value().trim().is_empty())
                            .on_click(cx.listener(|editor, _, window, cx| editor.run(window, cx))),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Statements run directly on the database"),
                    ),
            )
            .child(
                div()
                    .id("sql-status")
                    .test_support()
                    .role(Role::Status)
                    .aria_label(self.status.clone())
                    .px_3()
                    .pb_2()
                    .text_sm()
                    .flex_shrink_0()
                    .text_color(if self.failed {
                        cx.theme().danger
                    } else {
                        cx.theme().muted_foreground
                    })
                    .child(self.status.clone()),
            )
            .when_some(self.results.as_ref(), |view, results| {
                view.child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .min_w_0()
                        .child(DataTable::new(results).stripe(true)),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::{SqlEditor, sql_comment_edit};
    use crate::{
        connections::OpenTable,
        settings::{Connection, DatabaseKind},
    };
    use gpui_kit::{
        AppContext, TestAppContext,
        component::{Root, WindowExt},
        px, size,
        test::{TestAppContextExt, TestWindowExt},
    };

    #[test]
    fn sql_comments_preserve_selection_indentation_and_unicode() {
        for (sql, selection, expected) in [
            ("SELECT '世界'", 11..11, "-- SELECT '世界'"),
            (
                "  SELECT 1\n\tSELECT 2",
                0..20,
                "  -- SELECT 1\n\t-- SELECT 2",
            ),
            ("SELECT 1\nSELECT 2", 0..9, "-- SELECT 1\nSELECT 2"),
            ("SELECT 1\nSELECT 2", 12..12, "SELECT 1\n-- SELECT 2"),
            ("", 0..0, "-- "),
            ("--value", 3..3, "-- --value"),
            ("SELECT 1\r\nSELECT 2", 0..18, "-- SELECT 1\r\n-- SELECT 2"),
        ] {
            let (range, replacement, selected) = sql_comment_edit(sql, selection.clone());
            let mut commented = sql.to_string();
            commented.replace_range(range, &replacement);
            assert_eq!(commented, expected);
            let (range, replacement, restored_selection) = sql_comment_edit(&commented, selected);
            commented.replace_range(range, &replacement);
            assert_eq!(commented, sql);
            assert_eq!(restored_selection, selection);
        }
        assert_eq!(
            sql_comment_edit("-- SELECT 1\nSELECT 2", 0..20).1,
            "-- -- SELECT 1\n-- SELECT 2"
        );
        assert_eq!(sql_comment_edit("--\tSELECT 1", 5..5).1, "\tSELECT 1");
    }

    #[gpui_kit::test]
    #[ignore = "requires an isolated MariaDB server and TABLELANE_TEST_MARIADB_PORT"]
    async fn mariadb_tabs_preserve_sql_drafts_results_and_confirm_discard(cx: &mut TestAppContext) {
        let session = std::sync::Arc::new(
            async_std::task::block_on(crate::database::connect(&Connection {
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
            }))
            .unwrap(),
        );
        let request = OpenTable {
            session,
            connection_name: "Test".into(),
            database: "tablelane_test".into(),
            table: "widgets".into(),
        };
        cx.update(gpui_kit::init);
        let mut app = None;
        let handle = cx.open_window(size(px(1300.), px(700.)), |window, cx| {
            let view = cx.new(|cx| crate::AppView::new(None, None, None, window, cx));
            view.update(cx, |view, cx| view.open_table(&request, window, cx));
            app = Some(view.clone());
            Root::new(view, window, cx)
        });
        let app = app.unwrap();
        let timeout = std::time::Duration::from_secs(15);
        cx.wait_for(handle.into(), timeout, |window, _| {
            window.try_find("table").is_some()
        })
        .await;
        let table = cx.update(|cx| app.read(cx).table().unwrap().clone());
        let mut first = None;
        let mut second = None;
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("toggle-right-sidebar", cx);
            window.click("toolbar-sql-editor", cx);
            assert!(window.try_find("right-sidebar").is_none());
            let crate::WorkspaceTab::Sql(editor, _) = &app.read(cx).tabs[1] else {
                panic!("SQL tab missing")
            };
            first = Some(editor.clone());
            window.input("SELECT SLEEP(1), 42 AS answer", cx);
            window.click("run-sql", cx);
        })
        .unwrap();
        let first = first.unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            let editor = &first;
            assert!(editor.read(cx).is_running());
            window.click(("close-tab", editor.entity_id()), cx);
            assert_eq!(app.read(cx).tabs.len(), 2);
            window.click("toolbar-sql-editor", cx);
            let crate::WorkspaceTab::Sql(editor, _) = &app.read(cx).tabs[2] else {
                panic!("second SQL tab missing")
            };
            second = Some(editor.clone());
            window.input("SELECT 43", cx);
            app.update(cx, |app, cx| app.open_table(&request, window, cx));
            assert_eq!(app.read(cx).tabs.len(), 3);
            assert_eq!(app.read(cx).table(), Some(&table));
            window.render_frame(cx);
            assert!(window.find("right-sidebar").visible());
            window.within("workspace-tabs").click(2_usize, cx);
            assert_eq!(
                second
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .input
                    .read(cx)
                    .value()
                    .as_ref(),
                "SELECT 43"
            );
        })
        .unwrap();
        let second = second.unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| !first.read(cx).is_running())
            .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.within("workspace-tabs").click(1_usize, cx);
            assert_eq!(
                window.within("workspace-tabs").find(1_usize).label(),
                Some("Test / tablelane_test / SQL 1 · Draft")
            );
            assert_eq!(
                first.read(cx).input.read(cx).value().as_ref(),
                "SELECT SLEEP(1), 42 AS answer"
            );
            let results = first.read(cx).results.as_ref().unwrap().read(cx);
            assert_eq!(results.delegate().data.rows[0][1].as_deref(), Some("42"));
            window.click(("close-tab", second.entity_id()), cx);
            assert!(window.has_active_dialog(cx));
            window.press("escape", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(!window.has_active_dialog(cx));
            assert_eq!(app.read(cx).tabs.len(), 3);
            window.click(("close-tab", second.entity_id()), cx);
            window.within("dialog").click("ok", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(app.read(cx).tabs.len(), 2);
            assert_eq!(app.read(cx).active_tab, 1);
            window.press("cmd-w", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.within("dialog").click("ok", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(app.read(cx).tabs.len(), 1);
            assert_eq!(app.read(cx).table(), Some(&table));
        })
        .unwrap();
    }

    #[gpui_kit::test]
    #[ignore = "requires an isolated MariaDB server and TABLELANE_TEST_MARIADB_PORT"]
    async fn mariadb_sql_editor_runs_statements_with_pointer_and_keyboard(cx: &mut TestAppContext) {
        let port = std::env::var("TABLELANE_TEST_MARIADB_PORT")
            .unwrap()
            .parse()
            .unwrap();
        let session = async_std::task::block_on(crate::database::connect(&Connection {
            name: "Test".into(),
            database_type: DatabaseKind::MariaDB,
            host: "127.0.0.1".into(),
            port: Some(port),
            username: "root".into(),
            password: String::new(),
            database: String::new(),
            file_path: String::new(),
        }))
        .unwrap();
        let session = std::sync::Arc::new(session);
        async_std::task::block_on(async {
            session
                .run_sql("mysql", "CREATE DATABASE `sql``test`")
                .await
                .unwrap();
            session
                .run_sql(
                    "sql`test",
                    "CREATE TABLE widgets (id INT PRIMARY KEY, name TEXT)",
                )
                .await
                .unwrap();
            let (_, affected) = session
                .run_sql("sql`test", "INSERT INTO widgets VALUES (1, 'original')")
                .await
                .unwrap();
            assert_eq!(affected, 1);
            let (empty, _) = session
                .run_sql("sql`test", "SELECT id, name FROM widgets WHERE id = 2")
                .await
                .unwrap();
            assert_eq!(empty.schema.columns, ["id", "name"]);
            assert!(empty.rows.is_empty());
            let (rows, _) = session
                .run_sql("sql`test", "SELECT seq FROM seq_1_to_1001")
                .await
                .unwrap();
            assert_eq!(rows.rows.len(), 1000);
            assert!(rows.has_more);
            assert!(session.run_sql("sql`test", " ").await.is_err());
            assert!(
                session
                    .run_sql("sql`test", "DELETE FROM widgets; SELECT 1")
                    .await
                    .is_err()
            );
            let (rows, _) = session
                .run_sql("sql`test", "SELECT name FROM widgets")
                .await
                .unwrap();
            assert_eq!(rows.rows[0][0].as_deref(), Some("original"));
        });
        cx.update(gpui_kit::init);
        let mut editor = None;
        let handle = cx.open_window(size(px(1000.), px(650.)), |window, cx| {
            let view = cx.new(|cx| {
                SqlEditor::new(
                    OpenTable {
                        session: session.clone(),
                        connection_name: "Test".into(),
                        database: "sql`test".into(),
                        table: "widgets".into(),
                    },
                    window,
                    cx,
                )
            });
            editor = Some(view.clone());
            Root::new(view, window, cx)
        });
        let editor = editor.unwrap();
        let timeout = std::time::Duration::from_secs(15);
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("run-sql", cx);
            assert!(!editor.read(cx).running);
            window.click(("input", editor.read(cx).input.entity_id()), cx);
            window.input("SELECT DATABASE(), 42 AS value, 12.3400 AS amount, '世界' AS text, NULL AS missing, X'00FF' AS bytes, CAST('2026-10-11' AS DATE) AS day, 1 AS value", cx);
            let sql = editor.read(cx).input.read(cx).value();
            window.press("cmd-/", cx);
            assert_eq!(editor.read(cx).input.read(cx).value().as_ref(), format!("-- {sql}"));
            window.press("cmd-z", cx);
            assert_eq!(editor.read(cx).input.read(cx).value(), sql);
            window.press("cmd-shift-z", cx);
            assert_eq!(editor.read(cx).input.read(cx).value().as_ref(), format!("-- {sql}"));
            window.press("cmd-/", cx);
            assert_eq!(editor.read(cx).input.read(cx).value(), sql);
            window.render_frame(cx);
            window.click("run-sql", cx);
            assert!(editor.read(cx).running);
            window.press("cmd-enter", cx);
        }).unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| !editor.read(cx).running)
            .await;
        cx.update_window(handle.into(), |_, window, cx| {
            let view = editor.read(cx);
            assert!(!view.failed, "{}", view.status);
            let data = &view.results.as_ref().unwrap().read(cx).delegate().data;
            assert_eq!(
                data.rows[0],
                [
                    Some("sql`test".into()),
                    Some("42".into()),
                    Some("12.3400".into()),
                    Some("世界".into()),
                    None,
                    Some("00FF".into()),
                    Some("2026-10-11".into()),
                    Some("1".into())
                ]
            );
            window.click(("input", editor.read(cx).input.entity_id()), cx);
            window.press("secondary-a", cx);
            window.input("UPDATE widgets SET name = 'updated' WHERE id = 1", cx);
            window.press("cmd-enter", cx);
            assert!(editor.read(cx).running);
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| !editor.read(cx).running)
            .await;
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(!editor.read(cx).failed);
            assert!(editor.read(cx).status.contains("1 rows affected"));
            window.click(("input", editor.read(cx).input.entity_id()), cx);
            window.press("secondary-a", cx);
            window.input("SELECT * FROM missing_table", cx);
            window.press("ctrl-enter", cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| !editor.read(cx).running)
            .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(editor.read(cx).failed);
            assert!(editor.read(cx).results.is_none());
            assert!(
                window
                    .find("sql-status")
                    .label()
                    .unwrap()
                    .contains("missing_table")
            );
        })
        .unwrap();
        async_std::task::block_on(async {
            let (rows, _) = session
                .run_sql("sql`test", "SELECT name FROM widgets")
                .await
                .unwrap();
            assert_eq!(rows.rows[0][0].as_deref(), Some("updated"));
            session
                .run_sql("mysql", "DROP DATABASE `sql``test`")
                .await
                .unwrap();
        });
    }
}
