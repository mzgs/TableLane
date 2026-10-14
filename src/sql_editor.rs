use crate::{
    connections::OpenTable,
    data_export::{ExportFormat, QueryExport},
    database::{QueryCancellation, SqlResult, TableRows},
    workspace_state::SqlDraft,
};
mod history;
use gpui_kit::{
    component::{
        ActiveTheme, Disableable, Icon, IndexPath, Sizable, WindowExt,
        button::{Button, ButtonVariants},
        clipboard::Clipboard,
        input::{CompletionProvider, Editor, EditorState, InputEvent, Rope, RopeExt},
        list::{List, ListDelegate, ListEvent, ListItem, ListState},
        select::{Select, SelectEvent, SelectState},
        table::{Column, DataTable, TableDelegate, TableState},
        text::{SelectionFormat, TextView},
    },
    prelude::FluentBuilder,
    *,
};
use history::History;
use lsp_types::{
    CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit,
    TextEdit,
};
use std::rc::Rc;

struct SqlCompletions {
    tables: Vec<String>,
}

impl SqlCompletions {
    fn items(&self, text: &Rope, offset: usize) -> Vec<CompletionItem> {
        let sql = text.to_string();
        let identifier = |ch: char| ch.is_alphanumeric() || matches!(ch, '_' | '$');
        let mut start = offset;
        for (ix, ch) in sql[..offset].char_indices().rev() {
            if !identifier(ch) {
                break;
            }
            start = ix;
        }
        let prefix = sql[start..offset].to_lowercase();
        if prefix.is_empty() {
            return Vec::new();
        }
        let quoted = sql[..start].ends_with('`');
        if quoted {
            start -= 1;
        }
        let mut end = offset;
        for ch in sql[offset..].chars() {
            if !identifier(ch) {
                break;
            }
            end += ch.len_utf8();
        }
        if quoted && sql[end..].starts_with('`') {
            end += 1;
        }
        let range =
            lsp_types::Range::new(text.offset_to_position(start), text.offset_to_position(end));
        // shortcut: prefix matching without SQL parsing, add context-aware ranking when columns and aliases are supported.
        let keywords = "SELECT FROM WHERE INSERT INTO VALUES UPDATE SET DELETE CREATE ALTER DROP TABLE JOIN LEFT RIGHT INNER OUTER ON AS AND OR NOT NULL IS IN LIKE BETWEEN EXISTS DISTINCT ORDER BY GROUP HAVING LIMIT OFFSET UNION ALL ASC DESC COUNT SUM AVG MIN MAX CASE WHEN THEN ELSE END WITH DATABASE SHOW TABLES TRUNCATE BEGIN COMMIT ROLLBACK";
        keywords
            .split_whitespace()
            .filter(|_| !quoted)
            .map(|name| (name, name.to_string(), CompletionItemKind::KEYWORD))
            .chain(self.tables.iter().map(|name| {
                (
                    name.as_str(),
                    crate::database::quote_identifier(name),
                    CompletionItemKind::CLASS,
                )
            }))
            .filter(|(name, _, _)| name.to_lowercase().starts_with(&prefix))
            .map(|(name, insert, kind)| CompletionItem {
                label: name.to_string(),
                kind: Some(kind),
                text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                    range,
                    new_text: insert,
                })),
                ..Default::default()
            })
            .collect()
    }
}

impl CompletionProvider for SqlCompletions {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _: CompletionContext,
        _: &mut Window,
        _: &mut App,
    ) -> Task<anyhow::Result<CompletionResponse>> {
        Task::ready(Ok(CompletionResponse::Array(self.items(text, offset))))
    }

    fn is_completion_trigger(&self, _: usize, new_text: &str, _: &mut App) -> bool {
        new_text
            .chars()
            .last()
            .is_some_and(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '$'))
    }
}

actions!(sql_editor, [RunSql, CancelSql, ToggleSqlComment]);

fn selected_sql(sql: &str, selection: std::ops::Range<usize>) -> &str {
    if selection.is_empty() {
        sql
    } else {
        &sql[selection]
    }
}

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

pub(crate) struct DraftChanged;

impl EventEmitter<DraftChanged> for SqlEditor {}

struct CompletedResult {
    table: Option<Entity<TableState<Results>>>,
    export: Option<std::sync::Arc<QueryExport>>,
    summary: String,
}

pub(crate) struct SqlEditor {
    request: Option<OpenTable>,
    draft: SqlDraft,
    input: Entity<EditorState>,
    results: Option<Entity<TableState<Results>>>,
    result_sets: Vec<CompletedResult>,
    result_selector: Option<Entity<SelectState<Vec<String>>>>,
    selected_result: usize,
    running: bool,
    cancellation: Option<std::sync::Arc<QueryCancellation>>,
    cancelling: bool,
    cancel_error: Option<String>,
    status: String,
    failed: bool,
    export: Option<std::sync::Arc<QueryExport>>,
    exporting: bool,
    export_status: Option<String>,
    export_failed: bool,
}

impl SqlEditor {
    #[cfg(test)]
    pub(crate) fn isolate_test_history(cx: &mut App) {
        cx.set_global(History::for_test());
    }

    pub(crate) fn new(request: OpenTable, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::from_draft(SqlDraft::new(&request), Some(request), window, cx)
    }

    pub(crate) fn restore(draft: SqlDraft, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::from_draft(draft, None, window, cx)
    }

    fn from_draft(
        mut draft: SqlDraft,
        request: Option<OpenTable>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        if cx.try_global::<History>().is_none() {
            cx.set_global(History::load());
        }
        cx.bind_keys([
            KeyBinding::new("cmd-enter", RunSql, Some("SqlEditor")),
            KeyBinding::new("ctrl-enter", RunSql, Some("SqlEditor")),
            KeyBinding::new("cmd-.", CancelSql, Some("SqlEditor")),
            KeyBinding::new("ctrl-.", CancelSql, Some("SqlEditor")),
            KeyBinding::new("cmd-/", ToggleSqlComment, Some("SqlEditor")),
        ]);
        let input = cx.new(|cx| {
            let mut input = EditorState::new(window, cx)
                .language("sql")
                .placeholder("Enter SQL statements separated by semicolons");
            input.lsp_mut().completion_provider = Some(Rc::new(SqlCompletions {
                tables: request
                    .as_ref()
                    .and_then(|request| request.session.databases().get(&request.database).cloned())
                    .unwrap_or_default(),
            }));
            input
        });
        input.update(cx, |input, cx| {
            input.set_value(std::mem::take(&mut draft.sql), window, cx);
            input.focus(window, cx);
        });
        cx.subscribe(&input, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.emit(DraftChanged);
            }
            cx.notify();
        })
        .detach();
        Self {
            request,
            draft,
            input,
            results: None,
            result_sets: Vec::new(),
            result_selector: None,
            selected_result: 0,
            running: false,
            cancellation: None,
            cancelling: false,
            cancel_error: None,
            status: "Run SQL to see its results".into(),
            failed: false,
            export: None,
            exporting: false,
            export_status: None,
            export_failed: false,
        }
    }

    pub(crate) fn is_busy(&self) -> bool {
        self.running || self.cancelling || self.exporting
    }

    pub(crate) fn is_running(&self) -> bool {
        self.running
    }

    pub(crate) fn set_sql(&mut self, sql: String, window: &mut Window, cx: &mut Context<Self>) {
        self.input
            .update(cx, |input, cx| input.set_value(sql, window, cx));
        cx.emit(DraftChanged);
        cx.notify();
    }

    pub(crate) fn focus(&self, window: &mut Window, cx: &mut App) {
        self.input.read(cx).focus_handle(cx).focus(window, cx);
    }

    pub(crate) fn request(&self) -> Option<&OpenTable> {
        self.request.as_ref()
    }

    pub(crate) fn draft(&self, cx: &App) -> SqlDraft {
        SqlDraft {
            sql: self.input.read(cx).value().to_string(),
            ..self.draft.clone()
        }
    }

    pub(crate) fn connection_name(&self) -> &str {
        &self.draft.connection_name
    }

    pub(crate) fn database(&self) -> &str {
        &self.draft.database
    }

    pub(crate) fn detach(&mut self, cx: &mut Context<Self>) {
        self.request = None;
        self.refresh_completions(cx);
        cx.notify();
    }

    pub(crate) fn refresh_completions(&self, cx: &mut Context<Self>) {
        let tables = self
            .request
            .as_ref()
            .and_then(|request| request.session.databases().get(&request.database).cloned())
            .unwrap_or_default();
        self.input.update(cx, |input, _| {
            input.lsp_mut().completion_provider = Some(Rc::new(SqlCompletions { tables }));
        });
    }

    pub(crate) fn attach(
        &mut self,
        session: std::sync::Arc<crate::database::Session>,
        name: String,
        cx: &mut Context<Self>,
    ) {
        if self.request.is_some()
            || session.connection_key() != self.draft.connection_key
            || !session.databases().contains_key(&self.draft.database)
        {
            return;
        }
        self.input.update(cx, |input, _| {
            input.lsp_mut().completion_provider = Some(Rc::new(SqlCompletions {
                tables: session
                    .databases()
                    .get(&self.draft.database)
                    .cloned()
                    .unwrap_or_default(),
            }));
        });
        self.draft.connection_name = name.clone();
        self.request = Some(OpenTable {
            session,
            connection_name: name,
            database: self.draft.database.clone(),
            table: self.draft.table.clone(),
        });
        cx.emit(DraftChanged);
        cx.notify();
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
        let input = self.input.read(cx);
        let text = input.value();
        let sql = selected_sql(&text, input.selected_range()).to_owned();
        if self.is_busy() || self.request.is_none() || sql.trim().is_empty() {
            return;
        }
        self.running = true;
        let cancellation = std::sync::Arc::new(QueryCancellation::new());
        self.cancellation = Some(cancellation.clone());
        self.cancel_error = None;
        self.failed = false;
        self.status = "Running SQL…".into();
        self.results = None;
        self.result_sets.clear();
        self.result_selector = None;
        self.export = None;
        self.export_status = None;
        let Some(request) = self.request.clone() else {
            return;
        };
        let history_sql = sql.clone();
        let task = cx.background_spawn(async move {
            async_std::task::spawn(async move {
                let start = std::time::Instant::now();
                let result = request
                    .session
                    .run_sql_script(&request.database, &sql, &cancellation)
                    .await;
                (result, start.elapsed(), cancellation.is_result_discarded())
            })
            .await
        });
        cx.spawn_in(window, async move |editor, cx| {
            let (result, elapsed, discarded) = task.await;
            let _ = editor.update_in(cx, |editor, window, cx| {
                editor.running = false;
                editor.cancellation = None;
                if discarded {
                    editor.cancel_error = None;
                }
                match result {
                    Ok(run) => {
                        let affected: u64 = run.results.iter().map(|result| result.affected).sum();
                        editor.set_results(run.results, window, cx);
                        editor.failed = run.error.is_some();
                        editor.status = if let Some(error) = run.error {
                            let count = editor.result_sets.len();
                            let retained = match count {
                                0 => String::new(),
                                1 => " 1 completed result remains available.".into(),
                                _ => format!(" {count} completed results remain available."),
                            };
                            format!("{error} Script stopped. Statements may have changed data; there is no automatic rollback.{retained}")
                        } else if editor.result_sets.len() == 1 {
                            format!("{} · {:.2} s", editor.result_sets[0].summary, elapsed.as_secs_f64())
                        } else {
                            format!("{} results · {affected} rows affected · {:.2} s", editor.result_sets.len(), elapsed.as_secs_f64())
                        };
                    }
                    Err(error) => {
                        editor.status = error;
                        editor.failed = !discarded;
                    }
                }
                cx.global_mut::<History>().record(
                    editor.draft.connection_name.clone(),
                    editor.draft.database.clone(),
                    history_sql,
                    editor.status.clone(),
                    editor.failed,
                );
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn set_results(
        &mut self,
        results: Vec<SqlResult>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.result_sets = results
            .into_iter()
            .map(|result| {
                let SqlResult {
                    data,
                    affected,
                    export,
                } = result;
                let summary = if data.schema.columns.is_empty() {
                    format!("{affected} rows affected · No row data returned")
                } else {
                    format!(
                        "{} rows{} · {affected} rows affected",
                        export
                            .as_ref()
                            .map_or(data.rows.len(), |export| export.row_count()),
                        if data.has_more {
                            " (first 1,000 shown)"
                        } else {
                            ""
                        }
                    )
                };
                let (table, export) = if data.schema.columns.is_empty() {
                    (None, None)
                } else {
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
                    (
                        Some(cx.new(|cx| {
                            TableState::new(Results { columns, data }, window, cx)
                                .col_movable(false)
                                .cell_selectable(true)
                                .row_header(false)
                        })),
                        export,
                    )
                };
                CompletedResult {
                    table,
                    export,
                    summary,
                }
            })
            .collect();
        let selected = self
            .result_sets
            .iter()
            .position(|result| result.table.is_some())
            .unwrap_or(0);
        self.select_result(selected, cx);
        if self.result_sets.len() > 1 {
            let choices = self
                .result_sets
                .iter()
                .enumerate()
                .map(|(ix, result)| format!("Result {} · {}", ix + 1, result.summary))
                .collect::<Vec<_>>();
            let selector =
                cx.new(|cx| SelectState::new(choices, Some(IndexPath::new(selected)), window, cx));
            cx.subscribe_in(
                &selector,
                window,
                |editor, selector, _: &SelectEvent<Vec<String>>, _, cx| {
                    if let Some(ix) = selector.read(cx).selected_index(cx) {
                        editor.select_result(ix.row, cx);
                    }
                },
            )
            .detach();
            self.result_selector = Some(selector);
        }
    }

    fn select_result(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.is_busy() {
            return;
        }
        if let Some(result) = self.result_sets.get(ix) {
            self.selected_result = ix;
            self.results = result.table.clone();
            self.export = result.export.clone();
            self.export_status = None;
            cx.notify();
        }
    }

    pub(crate) fn cancel_sql(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.running || self.cancelling {
            return;
        }
        let (Some(cancellation), Some(request)) = (self.cancellation.clone(), self.request.clone())
        else {
            return;
        };
        self.cancelling = true;
        self.cancel_error = None;
        self.status = "Cancellation requested; waiting for SQL to finish…".into();
        let task = cx.background_spawn(async move {
            async_std::task::spawn(async move { cancellation.cancel(&request.session).await }).await
        });
        cx.spawn_in(window, async move |editor, cx| {
            let result = task.await;
            let _ = editor.update_in(cx, |editor, _, cx| {
                editor.cancelling = false;
                if let Err(error) = result {
                    editor.cancel_error = Some(error);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn export_results(
        &mut self,
        format: ExportFormat,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.running || self.exporting {
            return;
        }
        let Some(export) = self.export.clone() else {
            return;
        };
        let directory = dirs::download_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| ".".into());
        let name = format!("query-results.{}", format.extension());
        let picker = cx.prompt_for_new_path(&directory, Some(&name));
        self.exporting = true;
        self.export_failed = false;
        self.export_status = Some("Choose an export file…".into());
        cx.notify();
        cx.spawn_in(window, async move |editor, cx| {
            let result: anyhow::Result<Option<usize>> = async {
                let Some(path) = picker.await?? else {
                    return Ok(None);
                };
                let _ = editor.update_in(cx, |editor, _, cx| {
                    editor.export_status = Some("Exporting results…".into());
                    cx.notify();
                });
                cx.background_spawn(async move { export.export(&path, format).map(Some) })
                    .await
            }
            .await;
            let _ = editor.update_in(cx, |editor, _, cx| {
                editor.exporting = false;
                editor.export_status = match result {
                    Ok(Some(rows)) => Some(format!("Exported {rows} rows.")),
                    Ok(None) => None,
                    Err(error) => {
                        editor.export_failed = true;
                        Some(format!("Couldn’t export results: {error}."))
                    }
                };
                cx.notify();
            });
        })
        .detach();
    }

    fn open_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.running || window.has_active_dialog(cx) {
            return;
        }
        let history = cx.global::<History>();
        let entries: Vec<_> = history
            .entries(&self.draft.connection_name, &self.draft.database)
            .cloned()
            .collect();
        let error = history.error.clone();
        let list = cx.new(|cx| ListState::new(HistoryList { entries }, window, cx));
        list.update(cx, |list, cx| {
            if !list.delegate().entries.is_empty() {
                list.set_selected_index(Some(IndexPath::new(0)), window, cx);
            }
        });
        cx.subscribe_in(&list, window, |editor, list, event, window, cx| {
            let ListEvent::Confirm(ix) = event else {
                return;
            };
            if editor.running {
                return;
            }
            let sql = list.read(cx).delegate().entries[ix.row].sql.clone();
            window.close_dialog(cx);
            editor
                .input
                .update(cx, |input, cx| input.replace_all(sql, window, cx));
            editor.focus(window, cx);
            cx.notify();
        })
        .detach();
        let title = format!(
            "SQL history · {} / {}",
            self.draft.connection_name, self.draft.database
        );
        let history_list = list.clone();
        window.open_dialog(cx, move |dialog, window, cx| {
            dialog
                .title(title.clone())
                .width(window.rem_size() * 36.)
                .when_some(error.as_ref(), |dialog, error| {
                    dialog.child(div().text_color(cx.theme().danger).child(error.clone()))
                })
                .child(
                    div()
                        .id("sql-history-list")
                        .h(rems(22.))
                        .child(List::new(&history_list)),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("Click a command to load it · ↑/↓ to browse · Enter to load"),
                )
        });
        list.update(cx, |list, cx| list.focus(window, cx));
    }
}

struct HistoryList {
    entries: Vec<history::Entry>,
}

impl ListDelegate for HistoryList {
    type Item = ListItem;

    fn items_count(&self, _: usize, _: &App) -> usize {
        self.entries.len()
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<ListItem> {
        let entry = self.entries.get(ix.row)?;
        Some(
            ListItem::new(("sql-history-entry", entry.id))
                .accessibility_label(entry.sql.clone())
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .min_w_0()
                        .w_full()
                        .child(
                            div()
                                .text_sm()
                                .truncate()
                                .child(entry.sql.split_whitespace().collect::<Vec<_>>().join(" ")),
                        )
                        .child(
                            div()
                                .text_xs()
                                .truncate()
                                .text_color(if entry.failed {
                                    cx.theme().danger
                                } else {
                                    cx.theme().muted_foreground
                                })
                                .child(format!(
                                    "{} · {}",
                                    if entry.failed { "Failed" } else { "Succeeded" },
                                    entry.status
                                )),
                        ),
                ),
        )
    }

    fn set_selected_index(
        &mut self,
        _: Option<IndexPath>,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) {
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> impl IntoElement {
        div()
            .p_3()
            .text_color(cx.theme().muted_foreground)
            .child("No commands yet. Run a statement to add it to history.")
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
            .capture_action(cx.listener(
                |editor, _: &gpui_kit::component::input::ToggleCodeActions, window, cx| {
                    if editor.running {
                        cx.stop_propagation();
                        editor.cancel_sql(window, cx);
                    }
                },
            ))
            .on_action(cx.listener(|editor, _: &RunSql, window, cx| editor.run(window, cx)))
            .on_action(cx.listener(|editor, _: &CancelSql, window, cx| editor.cancel_sql(window, cx)))
            .on_action(cx.listener(|editor, _: &ToggleSqlComment, window, cx| {
                editor.toggle_comment(window, cx)
            }))
            .child(
                div()
                    .id("sql-database")
                    .test_support()
                    .aria_label(format!("Selected database: {}", self.draft.database))
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
                    .child(div().truncate().child(self.draft.database.clone())),
            )
            .when(self.request.as_ref().is_none_or(|request| !request.session.is_connected()), |view| view.child(
                div().id("sql-offline").test_support().role(Role::Status)
                    .aria_label("Offline. Connect its saved connection in the sidebar to run SQL.")
                    .px_3().pb_2().text_sm().text_color(cx.theme().muted_foreground)
                    .child("Offline. Connect its saved connection in the sidebar to run SQL.")))
            .child(
                div().id("sql-input").p_3().flex_shrink_0().child(
                    Editor::new(&self.input)
                        .aria_label("SQL script")
                        .h(rems(10.))
                        .readonly(self.running),
                ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .flex_wrap()
                    .px_3()
                    .pb_2()
                    .flex_shrink_0()
                    .child(
                        Button::new("run-sql")
                            .small()
                            .label("Run")
                            .tooltip("Run selected SQL, or the whole script (Cmd/Ctrl+Enter)")
                            .loading(self.running)
                            .disabled(self.is_busy() || self.request.as_ref().is_none_or(|request| !request.session.is_connected()) || self.input.read(cx).value().trim().is_empty())
                            .on_click(cx.listener(|editor, _, window, cx| editor.run(window, cx))),
                    )
                    .when(self.running || self.cancelling, |controls| controls.child(
                        Button::new("cancel-sql").small().outline().label("Cancel")
                            .tooltip("Cancel this SQL execution (Cmd/Ctrl+.)")
                            .loading(self.cancelling).disabled(!self.running || self.cancelling)
                            .on_click(cx.listener(|editor, _, window, cx| editor.cancel_sql(window, cx)))
                    ))
                    .child(
                        Button::new("sql-history")
                            .small()
                            .ghost()
                            .label("History…")
                            .disabled(self.running)
                            .on_click(
                                cx.listener(|editor, _, window, cx| {
                                    editor.open_history(window, cx)
                                }),
                            ),
                    )
                    .child(
                        Button::new("export-sql-csv")
                            .small().ghost().label("Export CSV…")
                            .disabled(self.running || self.exporting || self.export.is_none())
                            .tooltip("Export the completed result with full values, including rows beyond the preview")
                            .on_click(cx.listener(|editor, _, window, cx| editor.export_results(ExportFormat::Csv, window, cx))),
                    )
                    .child(
                        Button::new("export-sql-json")
                            .small().ghost().label("Export JSON…")
                            .disabled(self.running || self.exporting || self.export.is_none())
                            .tooltip("Export the completed result with full values, including rows beyond the preview")
                            .on_click(cx.listener(|editor, _, window, cx| editor.export_results(ExportFormat::Json, window, cx))),
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
                    .flex()
                    .items_start()
                    .gap_2()
                    .min_w_0()
                    .text_color(if self.failed {
                        cx.theme().danger
                    } else {
                        cx.theme().muted_foreground
                    })
                    .child(
                        div()
                            .id("sql-message")
                            .test_support()
                            .flex_1()
                            .min_w_0()
                            .child(
                                TextView::html(
                                    "sql-message-text",
                                    self.status
                                        .replace('&', "&amp;")
                                        .replace('<', "&lt;")
                                        .replace('>', "&gt;")
                                        .replace('\n', "<br>"),
                                )
                                .selectable(true)
                                .scrollable(false)
                                .selection_format(SelectionFormat::Plain),
                            ),
                    )
                    .when(self.failed, |status| {
                        status.child(
                            Clipboard::new("copy-sql-error")
                                .value(self.status.clone())
                                .accessibility_label("Copy SQL error")
                                .tooltip("Copy SQL error"),
                        )
                    }),
            )
            .when_some(self.export_status.as_ref(), |view, status| {
                view.child(div().id("sql-export-status").test_support().role(Role::Status)
                    .aria_label(status.clone()).px_3().pb_2().text_sm()
                    .text_color(if self.export_failed { cx.theme().danger } else { cx.theme().muted_foreground })
                    .child(status.clone()))
            })
            .when_some(self.cancel_error.as_ref(), |view, error| {
                view.child(div().id("sql-cancel-error").test_support().role(Role::Status)
                    .aria_label(error.clone()).px_3().pb_2().text_sm().text_color(cx.theme().danger)
                    .child(error.clone()))
            })
            .when_some(self.result_selector.as_ref(), |view, selector| {
                view.child(div().px_3().pb_2().flex_shrink_0().min_w_0().child(
                    Select::new(selector).id("sql-result-selector").small()
                        .accessibility_label("Result set").w_full().disabled(self.is_busy())
                ))
            })
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
    use super::{SqlCompletions, SqlEditor, sql_comment_edit};
    use crate::{
        connections::OpenTable,
        settings::{Connection, DatabaseKind},
    };
    use gpui_kit::{
        AppContext, Focusable, TestAppContext,
        component::{Root, WindowExt},
        px, size,
        test::{TestAppContextExt, TestWindowExt},
    };

    #[test]
    fn sql_execution_uses_selection_or_whole_statement() {
        let sql = "SELECT '世界'; SELECT 2";
        assert_eq!(super::selected_sql(sql, 0..0), sql);
        let start = sql.find("SELECT 2").unwrap();
        assert_eq!(super::selected_sql(sql, start..sql.len()), "SELECT 2");
        assert_eq!(super::selected_sql(sql, 7..15), "'世界'");
    }

    #[gpui_kit::test]
    #[ignore = "requires an isolated MariaDB server and TABLELANE_TEST_MARIADB_PORT"]
    async fn mariadb_sql_scripts_select_and_export_completed_results_through_ui(
        cx: &mut TestAppContext,
    ) {
        cx.background_executor.allow_parking();
        let session = std::sync::Arc::new(
            async_std::task::block_on(crate::database::connect(&Connection {
                name: "Script UI".into(),
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
        cx.update(gpui_kit::init);
        cx.update(|cx| cx.set_global(super::History::for_test()));
        let mut editor = None;
        let handle = cx.open_window(size(px(1100.), px(750.)), |window, cx| {
            let view = cx.new(|cx| {
                SqlEditor::new(
                    OpenTable {
                        session: session.clone(),
                        connection_name: "Script UI".into(),
                        database: "mysql".into(),
                        table: String::new(),
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
        let script = "SET @script_ui_runs = COALESCE(@script_ui_runs, 0) + 1; SELECT 11 AS first_result; SELECT seq, REPEAT('世界', 300) AS body FROM seq_1_to_1005; SELECT 1 AS empty_result WHERE FALSE;";
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.input(script, cx);
            window.press("cmd-enter", cx);
            assert!(editor.read(cx).running);
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| !editor.read(cx).running)
            .await;
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(!editor.read(cx).failed, "{}", editor.read(cx).status);
            assert_eq!(editor.read(cx).result_sets.len(), 4);
            assert_eq!(editor.read(cx).selected_result, 1);
            assert_eq!(
                editor
                    .read(cx)
                    .results
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .delegate()
                    .data
                    .rows[0][0]
                    .as_deref(),
                Some("11")
            );
            window.render_frame(cx);
            window.click("sql-result-selector", cx);
            window.press("down", cx);
            window.press("enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            assert_eq!(editor.read(cx).selected_result, 2);
            assert_eq!(editor.read(cx).export.as_ref().unwrap().row_count(), 1005);
            assert_eq!(
                editor
                    .read(cx)
                    .results
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .delegate()
                    .data
                    .rows
                    .len(),
                1000
            );
            window.click("export-sql-json", cx);
            window.click("sql-result-selector", cx);
            assert!(editor.read(cx).exporting);
            assert_eq!(editor.read(cx).selected_result, 2);
        })
        .unwrap();
        cx.run_until_parked();
        let (file, output) = crate::data_export::TemporaryFile::new(&std::env::temp_dir()).unwrap();
        drop(output);
        cx.simulate_new_path_selection(|_| Some(file.path().to_owned()));
        cx.wait_for(handle.into(), timeout, |_, cx| !editor.read(cx).exporting)
            .await;
        let json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(file.path()).unwrap()).unwrap();
        assert_eq!(json["rows"].as_array().unwrap().len(), 1005);
        assert_eq!(json["rows"][1004][1], "世界".repeat(300));
        assert_eq!(
            async_std::task::block_on(session.run_sql("mysql", "SELECT @script_ui_runs"))
                .unwrap()
                .0
                .rows[0][0]
                .as_deref(),
            Some("1")
        );
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("sql-result-selector", cx);
            window.press("down", cx);
            window.press("enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            assert_eq!(editor.read(cx).selected_result, 3);
            assert_eq!(editor.read(cx).export.as_ref().unwrap().row_count(), 0);
            assert_eq!(
                editor
                    .read(cx)
                    .results
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .delegate()
                    .data
                    .schema
                    .columns,
                ["empty_result"]
            );
            // Select a write completion: no export or stale table from the previous result.
            window.click("sql-result-selector", cx);
            window.press("up", cx);
            window.press("up", cx);
            window.press("up", cx);
            window.press("enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            assert_eq!(editor.read(cx).selected_result, 0);
            assert!(editor.read(cx).export.is_none());
            assert!(editor.read(cx).results.is_none());
            window.click("export-sql-json", cx);
            assert!(!editor.read(cx).exporting);
            window.click(("input", editor.read(cx).input.entity_id()), cx);
            window.press("secondary-a", cx);
            window.input(
                "SELECT 42; SELECT * FROM tablelane_script_missing; SET @script_ui_runs = 99",
                cx,
            );
            window.click("run-sql", cx);
            assert!(editor.read(cx).result_sets.is_empty());
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| !editor.read(cx).running)
            .await;
        cx.update_window(handle.into(), |_, window, cx| {
            let view = editor.read(cx);
            assert!(view.failed);
            assert!(view.status.contains("Script stopped"));
            assert_eq!(view.result_sets.len(), 1);
            assert_eq!(
                view.results.as_ref().unwrap().read(cx).delegate().data.rows[0][0].as_deref(),
                Some("42")
            );
            assert!(view.export.is_some());
            assert!(view.result_selector.is_none());
            let history = cx.global::<super::History>();
            let entries: Vec<_> = history.entries("Script UI", "mysql").collect();
            assert_eq!(entries.len(), 2);
            assert!(entries[0].failed);
            assert_eq!(entries[1].sql, script);
            window.render_frame(cx);
            assert!(
                window
                    .find("sql-status")
                    .label()
                    .unwrap()
                    .contains("1 completed result")
            );
        })
        .unwrap();
        assert_eq!(
            async_std::task::block_on(session.run_sql("mysql", "SELECT @script_ui_runs"))
                .unwrap()
                .0
                .rows[0][0]
                .as_deref(),
            Some("1")
        );
    }

    #[gpui_kit::test]
    #[ignore = "requires an isolated MariaDB server and TABLELANE_TEST_MARIADB_PORT"]
    async fn mariadb_restored_drafts_attach_only_to_their_connection_without_execution(
        cx: &mut TestAppContext,
    ) {
        cx.background_executor.allow_parking();
        use crate::workspace_state::{SavedTab, SqlDraft, WorkspaceFile, WorkspaceState};
        let directory =
            std::env::temp_dir().join(format!("tablelane-workspace-live-{}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let config = Connection {
            name: "Renamed".into(),
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
        let mut wrong = config.clone();
        wrong.username = "another-user".into();
        let mut state = WorkspaceState::default();
        state.tabs = vec![
            SavedTab::Sql(SqlDraft {
                number: 1,
                connection_key: config.key(),
                connection_name: "Old name".into(),
                database: "tablelane_test".into(),
                table: "widgets".into(),
                sql: "SELECT @recovery_runs := COALESCE(@recovery_runs, 0) + 1".into(),
            }),
            SavedTab::Sql(SqlDraft {
                number: 2,
                connection_key: config.key(),
                connection_name: "Old name".into(),
                database: "missing_database".into(),
                table: String::new(),
                sql: "SELECT 2".into(),
            }),
            SavedTab::Sql(SqlDraft {
                number: 3,
                connection_key: wrong.key(),
                connection_name: "Old name".into(),
                database: "tablelane_test".into(),
                table: "widgets".into(),
                sql: "SELECT 3".into(),
            }),
        ];
        assert!(
            WorkspaceFile::load(Some(directory.join("workspace.json")))
                .0
                .save(&state)
        );
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(super::History::for_test());
            cx.set_global(crate::settings::Settings {
                connections: vec![config.clone()],
                ..crate::settings::Settings::default()
            });
        });
        let mut app = None;
        let handle = cx.open_window(size(px(1100.), px(700.)), |window, cx| {
            let view = cx.new(|cx| {
                crate::AppView::new(
                    Some(directory.join("settings.json")),
                    None,
                    None,
                    window,
                    cx,
                )
            });
            app = Some(view.clone());
            Root::new(view, window, cx)
        });
        let app = app.unwrap();
        let editors = cx.update(|cx| {
            app.read(cx)
                .tabs
                .iter()
                .map(|tab| {
                    let crate::WorkspaceTab::Sql(editor, _) = tab else {
                        panic!("SQL tab missing")
                    };
                    editor.clone()
                })
                .collect::<Vec<_>>()
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(
                editors
                    .iter()
                    .all(|editor| editor.read(cx).request().is_none())
            );
            window.click("run-sql", cx);
            assert!(!editors[0].read(cx).is_running());
            window.double_click("connection-1", cx);
        })
        .unwrap();
        let timeout = std::time::Duration::from_secs(15);
        cx.wait_for(handle.into(), timeout, |_, cx| {
            editors[0].read(cx).request().is_some()
        })
        .await;
        let session = cx.update(|cx| {
            assert_eq!(editors[0].read(cx).connection_name(), "Renamed");
            assert!(!editors[0].read(cx).is_running());
            assert!(editors[0].read(cx).results.is_none());
            assert_eq!(editors[0].read(cx).draft(cx).sql, state.tabs[0].sql().sql);
            assert!(editors[1].read(cx).request().is_none());
            assert!(editors[2].read(cx).request().is_none());
            editors[0].read(cx).request().unwrap().session.clone()
        });
        let (result, _) =
            async_std::task::block_on(session.run_sql("tablelane_test", "SELECT @recovery_runs"))
                .unwrap();
        assert_eq!(result.rows[0][0], None);
        // Metadata refresh replaces completions on already attached editors without rerunning drafts.
        for refreshed in [false, true] {
            if refreshed {
                async_std::task::block_on(session.run_sql(
                    "tablelane_test",
                    "CREATE TABLE refresh_completions_added (id INT PRIMARY KEY)",
                ))
                .unwrap();
                cx.update_window(handle.into(), |_, window, cx| {
                    window.click("refresh-connection-metadata", cx)
                })
                .unwrap();
                cx.wait_for(handle.into(), timeout, |_, cx| {
                    !app.read(cx).connections.read(cx).is_connecting()
                })
                .await;
            }
            let task = cx
                .update_window(handle.into(), |_, window, cx| {
                    let text = super::Rope::from("SELECT * FROM refresh_completions_added");
                    let provider = editors[0]
                        .read(cx)
                        .input
                        .read(cx)
                        .lsp()
                        .completion_provider
                        .as_ref()
                        .unwrap()
                        .clone();
                    provider.completions(
                        &text,
                        text.len(),
                        lsp_types::CompletionContext {
                            trigger_kind: lsp_types::CompletionTriggerKind::INVOKED,
                            trigger_character: None,
                        },
                        window,
                        cx,
                    )
                })
                .unwrap();
            let lsp_types::CompletionResponse::Array(items) = task.await.unwrap() else {
                panic!("completion array")
            };
            assert_eq!(
                items
                    .iter()
                    .any(|item| item.label == "refresh_completions_added"),
                refreshed
            );
            cx.update(|cx| assert_eq!(editors[0].read(cx).draft(cx).sql, state.tabs[0].sql().sql));
        }
        async_std::task::block_on(
            session.run_sql("tablelane_test", "DROP TABLE refresh_completions_added"),
        )
        .unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("sql-offline").is_none());
            window.click("run-sql", cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| {
            !editors[0].read(cx).is_running()
        })
        .await;
        cx.update(|cx| {
            assert!(!editors[0].read(cx).failed);
            assert_eq!(
                editors[0]
                    .read(cx)
                    .results
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .delegate()
                    .data
                    .rows[0][0]
                    .as_deref(),
                Some("1")
            );
        });
        let (result, _) =
            async_std::task::block_on(session.run_sql("tablelane_test", "SELECT @recovery_runs"))
                .unwrap();
        assert_eq!(result.rows[0][0].as_deref(), Some("1"));
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("toolbar-sql-editor", cx);
            window.input("SELECT 'new draft 保存'", cx);
            app.update(cx, |view, cx| {
                assert!(view.save_workspace(cx));
            });
            window.within("workspace-tabs").click(1_usize, cx);
            assert!(window.find("sql-offline").visible());
            window.click("run-sql", cx);
            assert!(!editors[1].read(cx).is_running());
        })
        .unwrap();
        let saved = WorkspaceFile::load(Some(directory.join("workspace.json"))).1;
        assert_eq!(saved.tabs.len(), 4);
        assert_eq!(saved.tabs[3].sql().number, 4);
        assert_eq!(saved.tabs[3].sql().sql, "SELECT 'new draft 保存'");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[gpui_kit::test]
    #[ignore = "requires an isolated MariaDB server and TABLELANE_TEST_MARIADB_PORT"]
    async fn mariadb_exports_tables_and_completed_sql_without_repeating_statements(
        cx: &mut TestAppContext,
    ) {
        cx.background_executor.allow_parking();
        let session = std::sync::Arc::new(
            async_std::task::block_on(crate::database::connect(&Connection {
                name: "Export test".into(),
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
                .run_sql("mysql", "CREATE DATABASE export_test")
                .await
                .unwrap();
            session.run_sql("export_test", "CREATE TABLE widgets (id INT PRIMARY KEY, body LONGTEXT, bytes BLOB, nullable TEXT, empty TEXT)").await.unwrap();
            session.run_sql("export_test", "INSERT INTO widgets SELECT seq, REPEAT('世界', 300), REPEAT(X'00FF', 150), NULL, '' FROM seq_1_to_1005").await.unwrap();
        });
        let directory =
            std::env::temp_dir().join(format!("tablelane-export-ui-{}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        cx.update(gpui_kit::init);
        cx.update(|cx| cx.set_global(super::History::for_test()));
        let request = OpenTable {
            session: session.clone(),
            connection_name: "Export test".into(),
            database: "export_test".into(),
            table: "widgets".into(),
        };
        let mut editor = None;
        let handle = cx.open_window(size(px(1100.), px(650.)), |window, cx| {
            let view = cx.new(|cx| SqlEditor::new(request.clone(), window, cx));
            editor = Some(view.clone());
            Root::new(view, window, cx)
        });
        let editor = editor.unwrap();
        let timeout = std::time::Duration::from_secs(15);
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("export-sql-json", cx);
            assert!(!editor.read(cx).exporting);
            window.input("SELECT id AS duplicate, id AS duplicate, body, bytes, nullable, empty, @export_runs := COALESCE(@export_runs, 0) + 1 AS invocation FROM widgets ORDER BY id", cx);
            window.click("run-sql", cx);
            window.click("export-sql-json", cx);
            assert!(!editor.read(cx).exporting);
        }).unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| !editor.read(cx).running)
            .await;
        cx.update_window(handle.into(), |_, window, cx| {
            let view = editor.read(cx);
            assert!(!view.failed, "{}", view.status);
            assert_eq!(view.export.as_ref().unwrap().row_count(), 1005);
            assert_eq!(
                view.results
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .delegate()
                    .data
                    .rows
                    .len(),
                1000
            );
            assert!(view.status.contains("1005 rows (first 1,000 shown)"));
            window.click("export-sql-json", cx);
            window.click("export-sql-csv", cx);
            assert!(editor.read(cx).exporting);
            assert!(editor.read(cx).is_busy());
        })
        .unwrap();
        cx.run_until_parked();
        cx.simulate_new_path_selection(|_| None);
        cx.wait_for(handle.into(), timeout, |_, cx| !editor.read(cx).exporting)
            .await;
        cx.update(|cx| assert!(editor.read(cx).export_status.is_none()));
        let json_path = directory.join("query.json");
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("export-sql-json", cx)
        })
        .unwrap();
        cx.run_until_parked();
        cx.simulate_new_path_selection(|_| Some(json_path.clone()));
        cx.wait_for(handle.into(), timeout, |_, cx| !editor.read(cx).exporting)
            .await;
        cx.update(|cx| assert!(!editor.read(cx).export_failed));
        let json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&json_path).unwrap()).unwrap();
        assert_eq!(json["columns"][0], json["columns"][1]);
        assert_eq!(json["rows"].as_array().unwrap().len(), 1005);
        assert_eq!(json["rows"][1004][2], "世界".repeat(300));
        assert_eq!(json["rows"][1004][3], "00FF".repeat(150));
        assert!(json["rows"][1004][4].is_null());
        assert_eq!(json["rows"][1004][5], "");
        assert_eq!(json["rows"][1004][6], "1005");
        let csv_path = directory.join("query.csv");
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("export-sql-csv", cx)
        })
        .unwrap();
        cx.run_until_parked();
        cx.simulate_new_path_selection(|_| Some(csv_path.clone()));
        cx.wait_for(handle.into(), timeout, |_, cx| !editor.read(cx).exporting)
            .await;
        let csv = std::fs::read_to_string(&csv_path).unwrap();
        assert_eq!(csv.lines().count(), 1006);
        assert!(csv.contains(&format!("\"{}\"", "00FF".repeat(150))));
        let (count, _) =
            async_std::task::block_on(session.run_sql("export_test", "SELECT @export_runs"))
                .unwrap();
        assert_eq!(count.rows[0][0].as_deref(), Some("1005"));
        // An unwritable destination keeps the result available for another export attempt.
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("export-sql-json", cx)
        })
        .unwrap();
        cx.run_until_parked();
        cx.simulate_new_path_selection(|_| Some(directory.clone()));
        cx.wait_for(handle.into(), timeout, |_, cx| !editor.read(cx).exporting)
            .await;
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(editor.read(cx).export_failed);
            assert!(editor.read(cx).export.is_some());
            window.render_frame(cx);
            assert!(
                window
                    .find("sql-export-status")
                    .label()
                    .unwrap()
                    .contains("Couldn’t export")
            );
        })
        .unwrap();

        let mut table = None;
        let table_handle = cx.open_window(size(px(1100.), px(650.)), |window, cx| {
            let view = cx.new(|cx| crate::table_view::TableView::new(request, window, cx));
            table = Some(view.clone());
            Root::new(view, window, cx)
        });
        let table = table.unwrap();
        cx.wait_for(table_handle.into(), timeout, |window, _| {
            window.try_find("export-table-json").is_some()
        })
        .await;
        cx.update_window(table_handle.into(), |_, window, cx| {
            window.click("export-table-json", cx)
        })
        .unwrap();
        cx.run_until_parked();
        let table_path = directory.join("table.json");
        cx.simulate_new_path_selection(|_| Some(table_path.clone()));
        cx.wait_for(table_handle.into(), timeout, |window, _| {
            window
                .try_find("table-export-status")
                .and_then(|status| status.label().map(str::to_owned))
                .is_some_and(|label| label.contains("Exported 1005 matching rows"))
        })
        .await;
        let json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&table_path).unwrap()).unwrap();
        assert_eq!(json["rows"][1004][0], "1005");
        assert_eq!(json["rows"][1004][1], "世界".repeat(300));
        assert_eq!(json["rows"][1004][2], "00FF".repeat(150));
        assert!(json["rows"][1004][3].is_null());
        assert_eq!(json["rows"][1004][4], "");
        cx.update_window(table_handle.into(), |_, window, cx| {
            window.click("export-table-csv", cx);
            assert!(!table.read(cx).can_change_schema(cx));
        })
        .unwrap();
        cx.run_until_parked();
        let table_csv = directory.join("table.csv");
        cx.simulate_new_path_selection(|_| Some(table_csv.clone()));
        cx.wait_for(table_handle.into(), timeout, |window, _| {
            window
                .try_find("table-export-status")
                .and_then(|status| status.label().map(str::to_owned))
                .is_some_and(|label| label.contains("Exported 1005 matching rows"))
        })
        .await;
        assert_eq!(
            std::fs::read_to_string(table_csv).unwrap().lines().count(),
            1006
        );
        async_std::task::block_on(session.run_sql("mysql", "DROP DATABASE export_test")).unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn sql_completions_match_prefixes_and_replace_complete_identifiers() {
        let provider = SqlCompletions {
            tables: vec!["widgets".into(), "we`ird table".into()],
        };
        for (sql, offset, label, replacement, start, end) in [
            ("sEl", 3, "SELECT", "SELECT", 0, 3),
            ("SELECT * FROM WID", 17, "widgets", "`widgets`", 14, 17),
            ("SELECT * FROM widg", 16, "widgets", "`widgets`", 14, 18),
            ("SELECT * FROM `wid`", 18, "widgets", "`widgets`", 14, 19),
            ("we", 2, "we`ird table", "`we``ird table`", 0, 2),
            ("SELECT '😀'; sel", 18, "SELECT", "SELECT", 12, 15),
        ] {
            let items = provider.items(&gpui_kit::component::input::Rope::from(sql), offset);
            let item = items.iter().find(|item| item.label == label).unwrap();
            let Some(lsp_types::CompletionTextEdit::Edit(edit)) = &item.text_edit else {
                panic!("completion must replace the typed prefix")
            };
            assert_eq!(edit.new_text, replacement);
            assert_eq!(edit.range.start.character, start);
            assert_eq!(edit.range.end.character, end);
        }
        assert!(
            provider
                .items(&gpui_kit::component::input::Rope::from("SELECT "), 7)
                .is_empty()
        );
        assert!(
            provider
                .items(&gpui_kit::component::input::Rope::from("zzzz"), 4)
                .is_empty()
        );
    }

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
        cx.background_executor.allow_parking();
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
        cx.update(|cx| cx.set_global(super::History::for_test()));
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
            window.input("sel", cx);
        })
        .unwrap();
        let first = first.unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.press("enter", cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| {
            first.read(cx).input.read(cx).value().as_ref() == "SELECT"
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            assert_eq!(first.read(cx).input.read(cx).value().as_ref(), "SELECT");
            window.press("secondary-a", cx);
            window.input("SELECT * FROM wid", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.press("enter", cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| {
            first.read(cx).input.read(cx).value().as_ref() == "SELECT * FROM `widgets`"
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            assert_eq!(
                first.read(cx).input.read(cx).value().as_ref(),
                "SELECT * FROM `widgets`"
            );
            window.press("secondary-a", cx);
            window.input("SELECT SLEEP(1), 42 AS answer", cx);
            window.click("run-sql", cx);
        })
        .unwrap();
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
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(first.read(cx).is_running());
            app.update(cx, |app, cx| assert!(!app.prepare_close(false, window, cx)));
            window.render_frame(cx);
            assert_eq!(window.within("dialog").find("ok").label(), Some("OK"));
            window.press("escape", cx);
        })
        .unwrap();
        cx.run_until_parked();
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
        cx.background_executor.allow_parking();
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
        cx.update(|cx| cx.set_global(super::History::for_test()));
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
            assert!(window.try_find("copy-sql-error").is_none());
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
            let statement = "UPDATE widgets SET name = 'updated' WHERE id = 1";
            let draft = format!("THIS IS NOT SQL;\n{statement}");
            window.input(&draft, cx);
            let input = editor.read(cx).input.clone();
            input.update(cx, |input, cx| {
                input.set_selected_range(draft.len() - statement.len()..draft.len(), cx);
            });
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
            let error = editor.read(cx).status.clone();
            let bounds = window.find("sql-message").bounds();
            window.drag(
                gpui_kit::point(bounds.left() + px(1.), bounds.top() + px(8.)),
                gpui_kit::point(bounds.right() - px(1.), bounds.bottom() - px(1.)),
                cx,
            );
            window.press("secondary-c", cx);
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().as_deref(),
                Some(error.as_str())
            );
            window.click("copy-sql-error", cx);
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().as_deref(),
                Some(error.as_str())
            );
        })
        .unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            let history = cx.global::<super::History>();
            let entries: Vec<_> = history.entries("Test", "sql`test").collect();
            assert_eq!(entries.len(), 3);
            assert!(entries[0].failed);
            assert_eq!(entries[0].sql, "SELECT * FROM missing_table");
            assert!(!entries[1].failed);
            window.click("sql-history", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.has_active_dialog(cx));
            window.press("escape", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(!window.has_active_dialog(cx));
            window.click("sql-history", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            let id = cx
                .global::<super::History>()
                .entries("Test", "sql`test")
                .nth(1)
                .unwrap()
                .id;
            window.click(("sql-history-entry", id), cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(!window.has_active_dialog(cx));
            assert_eq!(
                editor.read(cx).input.read(cx).value().as_ref(),
                "UPDATE widgets SET name = 'updated' WHERE id = 1"
            );
            assert!(!editor.read(cx).running);
            assert_eq!(
                cx.global::<super::History>()
                    .entries("Test", "sql`test")
                    .count(),
                3
            );
            assert!(
                editor
                    .read(cx)
                    .input
                    .read(cx)
                    .focus_handle(cx)
                    .is_focused(window)
            );
            window.press("cmd-z", cx);
            assert_eq!(
                editor.read(cx).input.read(cx).value().as_ref(),
                "SELECT * FROM missing_table"
            );
        })
        .unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("sql-history", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.press("down", cx);
            window.press("enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(!window.has_active_dialog(cx));
            assert_eq!(
                editor.read(cx).input.read(cx).value().as_ref(),
                "UPDATE widgets SET name = 'updated' WHERE id = 1"
            );
            assert!(!editor.read(cx).running);
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
    #[gpui_kit::test]
    #[ignore = "requires an isolated MariaDB server and TABLELANE_TEST_MARIADB_PORT"]
    async fn mariadb_sql_cancel_controls_queue_keyboard_history_and_session_reuse(
        cx: &mut TestAppContext,
    ) {
        cx.background_executor.allow_parking();
        use std::{sync::Arc, time::Duration};
        let config = Connection {
            name: "Cancel UI".into(),
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
        let session =
            Arc::new(async_std::task::block_on(crate::database::connect(&config)).unwrap());
        let observer = async_std::task::block_on(crate::database::connect(&config)).unwrap();
        let thread_id = async_std::task::block_on(async {
            session
                .run_sql("mysql", "SET @queued_ui_runs = 0, @preserved_ui = 42")
                .await
                .unwrap();
            session
                .run_sql("mysql", "SELECT CONNECTION_ID()")
                .await
                .unwrap()
                .0
                .rows[0][0]
                .clone()
                .unwrap()
        });
        let request = OpenTable {
            session: session.clone(),
            connection_name: "Cancel UI".into(),
            database: "mysql".into(),
            table: "".into(),
        };
        cx.update(gpui_kit::init);
        cx.update(SqlEditor::isolate_test_history);
        let mut app = None;
        let handle = cx.open_window(size(px(1250.), px(800.)), |window, cx| {
            let view = cx.new(|cx| crate::AppView::new(None, None, None, window, cx));
            view.update(cx, |view, cx| {
                view.open_sql_draft(request.clone(), Some("SELECT SLEEP(30)".into()), window, cx);
                view.open_sql_draft(
                    request.clone(),
                    Some("SET @queued_ui_runs = @queued_ui_runs + 1".into()),
                    window,
                    cx,
                );
                view.select_tab(0, window, cx);
            });
            app = Some(view.clone());
            Root::new(view, window, cx)
        });
        let app = app.unwrap();
        let (first, queued) = cx.update(|cx| {
            let crate::WorkspaceTab::Sql(first, _) = &app.read(cx).tabs[0] else {
                panic!("SQL missing")
            };
            let crate::WorkspaceTab::Sql(queued, _) = &app.read(cx).tabs[1] else {
                panic!("SQL missing")
            };
            (first.clone(), queued.clone())
        });
        let timeout = Duration::from_secs(10);
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("run-sql", cx);
        })
        .unwrap();
        cx.run_until_parked();
        async_std::task::block_on(async_std::future::timeout(Duration::from_secs(5), async {
            loop {
                let rows = observer.run_sql("mysql", &format!("SELECT COUNT(*) FROM information_schema.PROCESSLIST WHERE ID = {thread_id} AND INFO LIKE '%SLEEP(30)%' AND COMMAND = 'Query'")).await.unwrap().0;
                if rows.rows[0][0].as_deref() == Some("1") { break; }
                async_std::task::sleep(Duration::from_millis(10)).await;
            }
        })).unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.within("workspace-tabs").click(1_usize, cx);
            window.click("run-sql", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(first.read(cx).is_busy());
            assert!(queued.read(cx).is_busy());
            // Quit checks inactive tabs as well as the queued active execution.
            app.update(cx, |app, cx| assert!(!app.prepare_close(false, window, cx)));
            window.press("escape", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("cancel-sql", cx);
            assert!(queued.read(cx).cancelling);
            window.click("cancel-sql", cx);
            window.click("run-sql", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.wait_for(handle.into(), timeout, |_, cx| !queued.read(cx).is_busy())
            .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(first.read(cx).is_running());
            assert!(
                queued
                    .read(cx)
                    .status
                    .starts_with("SQL cancelled before execution")
            );
            assert!(!queued.read(cx).failed);
            assert!(queued.read(cx).export.is_none());
            assert!(queued.read(cx).results.is_none());
            assert_eq!(
                queued.read(cx).input.read(cx).value().as_ref(),
                "SET @queued_ui_runs = @queued_ui_runs + 1"
            );
            window.within("workspace-tabs").click(0_usize, cx);
            window.input("changed", cx);
            assert_eq!(
                first.read(cx).input.read(cx).value().as_ref(),
                "SELECT SLEEP(30)"
            );
            window.press("cmd-.", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.wait_for(handle.into(), timeout, |_, cx| !first.read(cx).is_busy())
            .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(first.read(cx).status.starts_with("SQL cancelled"));
            assert!(!first.read(cx).failed);
            assert!(first.read(cx).export.is_none());
            assert!(first.read(cx).results.is_none());
            assert!(window.try_find("cancel-sql").is_none());
            assert_eq!(
                first.read(cx).input.read(cx).value().as_ref(),
                "SELECT SLEEP(30)"
            );
            let history = cx
                .global::<super::History>()
                .entries("Cancel UI", "mysql")
                .collect::<Vec<_>>();
            assert_eq!(history.len(), 2);
            assert!(
                history
                    .iter()
                    .all(|entry| !entry.failed && entry.status.starts_with("SQL cancelled"))
            );
            first.update(cx, |editor, cx| {
                editor.set_sql(
                    "SELECT @preserved_ui, @queued_ui_runs, CONNECTION_ID()".into(),
                    window,
                    cx,
                )
            });
            window.click("run-sql", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.wait_for(handle.into(), timeout, |_, cx| !first.read(cx).is_busy())
            .await;
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(!first.read(cx).failed, "{}", first.read(cx).status);
            let values = &first
                .read(cx)
                .results
                .as_ref()
                .unwrap()
                .read(cx)
                .delegate()
                .data
                .rows[0];
            assert_eq!(
                values,
                &[Some("42".into()), Some("0".into()), Some(thread_id.clone())]
            );
            assert!(first.read(cx).export.is_some());
            window.press("ctrl-.", cx);
            assert!(!first.read(cx).is_busy());
            first.update(cx, |editor, cx| {
                editor.set_sql("SELECT SLEEP(30)".into(), window, cx)
            });
            window.click("run-sql", cx);
            window.press("ctrl-.", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.wait_for(handle.into(), timeout, |_, cx| !first.read(cx).is_busy())
            .await;
        cx.update(|cx| {
            assert!(first.read(cx).status.starts_with("SQL cancelled"));
            assert!(!first.read(cx).failed);
            assert!(first.read(cx).export.is_none());
        });
        async_std::task::block_on(session.check_health());
        assert!(session.is_connected());
    }
}
