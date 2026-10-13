mod import;
mod row_operations;

use crate::{
    connections::OpenTable,
    data_export::ExportFormat,
    database::{
        FilterOperator, PAGE_SIZE, TableFilter, TableQuery, TableRows, TableSchema,
        truncate_preview,
    },
    workspace_state::TableDraft,
};
use gpui_kit::{
    component::{
        ActiveTheme, Disableable, Sizable,
        button::{Button, ButtonVariants},
        checkbox::Checkbox,
        form::{Field, Form},
        input::{Input, InputEvent, InputState, Textarea, TextareaState},
        scroll::ScrollableElement,
        select::{SearchableVec, Select, SelectEvent, SelectState},
        table::{Column, DataTable, TableDelegate, TableEvent, TableState},
        tooltip::Tooltip,
    },
    prelude::FluentBuilder,
    *,
};

struct Rows {
    columns: Vec<Column>,
    data: TableRows,
    owner: WeakEntity<TableView>,
}

impl TableDelegate for Rows {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }
    fn rows_count(&self, _: &App) -> usize {
        self.data.rows.len()
    }
    fn column(&self, ix: usize, _: &App) -> Column {
        self.columns[ix].clone()
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let column = &self.columns[col_ix];
        let kind = &self.data.schema.column_types[col_ix];
        let label = format!("{} · {}", column.name, kind);
        let owner = self.owner.upgrade().expect("table owner exists");
        let view = owner.read(cx);
        let sorted = view
            .query
            .sort
            .as_ref()
            .filter(|(name, _)| name == column.key.as_ref());
        let name = format!(
            "{}{}",
            column.name,
            sorted.map_or("", |(_, descending)| if *descending {
                " ↓"
            } else {
                " ↑"
            })
        );
        let key = column.key.to_string();
        Button::new(SharedString::from(format!("sort-column-{key}")))
            .ghost()
            .small()
            .size_full()
            .p_0()
            .accessibility_label(format!(
                "Sort by {}{}",
                column.name,
                sorted.map_or("", |(_, descending)| if *descending {
                    ", descending"
                } else {
                    ", ascending"
                })
            ))
            .disabled(view.loading || view.saving || view.importing || view.is_dirty(cx))
            .on_click(move |_, window, cx| {
                owner.update(cx, |view, cx| {
                    let mut query = view.query.clone();
                    query.page = 0;
                    query.sort = match &query.sort {
                        Some((name, false)) if name == &key => Some((key.clone(), true)),
                        Some((name, true)) if name == &key => None,
                        _ => Some((key.clone(), false)),
                    };
                    view.load(query, window, cx);
                });
            })
            .child(
                div()
                    .id(SharedString::from(format!("column-title-{}", column.key)))
                    .test_support()
                    .aria_label(label.clone())
                    .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
                    .flex()
                    .flex_col()
                    .size_full()
                    .justify_center()
                    .px_1p5()
                    .gap_0p5()
                    .min_w_0()
                    .overflow_hidden()
                    .child(
                        div()
                            .id(SharedString::from(format!("column-name-{}", column.key)))
                            .test_support()
                            .truncate()
                            .text_sm()
                            .line_height(relative(1.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(cx.theme().foreground)
                            .child(name),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("column-type-{}", column.key)))
                            .test_support()
                            .truncate()
                            .text_xs()
                            .line_height(relative(1.))
                            .font_weight(FontWeight::NORMAL)
                            .text_color(cx.theme().muted_foreground)
                            .child(kind.clone()),
                    ),
            )
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let value = &self.data.rows[row_ix][col_ix];
        div()
            .truncate()
            .size_full()
            .flex()
            .items_center()
            .px_1p5()
            .border_r_1()
            .border_color(cx.theme().table_row_border)
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
            .p_4()
            .text_color(cx.theme().muted_foreground)
            .child("No matching rows. Clear filters or refresh the table.")
    }
}

actions!(row_editor, [SaveRow, CancelRow]);

struct RowField {
    name: SharedString,
    input: Entity<TextareaState>,
    original: Option<String>,
    is_null: bool,
    use_default: bool,
    editable: bool,
}

impl RowField {
    fn value(&self, cx: &App) -> Option<String> {
        if self.is_null {
            None
        } else {
            Some(self.input.read(cx).value().to_string())
        }
    }
    fn is_edited(&self, cx: &App) -> bool {
        self.value(cx) != self.original
    }
}

#[derive(Default)]
pub(crate) struct TableView {
    title: String,
    location: String,
    request: Option<OpenTable>,
    draft: Option<TableDraft>,
    table: Option<Entity<TableState<Rows>>>,
    fields: Vec<RowField>,
    selected_row: Option<usize>,
    load_generation: u64,
    loading_row: bool,
    row_reads: usize,
    saving: bool,
    inserting: bool,
    editor_focus: Option<FocusHandle>,
    edit_error: Option<String>,
    error: Option<String>,
    query: TableQuery,
    loading: bool,
    filter_column: Option<Entity<SelectState<SearchableVec<String>>>>,
    filter_operator: Option<Entity<SelectState<Vec<String>>>>,
    filter_value: Option<Entity<InputState>>,
    filter_task: Option<Task<()>>,
    filter_pending: bool,
    importing: bool,
    import_status: Option<String>,
    import_failed: bool,
    exporting: bool,
    export_status: Option<String>,
    export_failed: bool,
}

pub(crate) struct QueryChanged;
pub(crate) struct RowEditorOpened;

impl EventEmitter<RowEditorOpened> for TableView {}

impl EventEmitter<QueryChanged> for TableView {}
impl EventEmitter<TableEvent> for TableView {}

impl TableView {
    pub(crate) fn can_change_schema(&self, cx: &App) -> bool {
        !self.is_dirty(cx) && !self.is_busy()
    }

    pub(crate) fn begin_schema_change(&mut self, cx: &mut Context<Self>) {
        self.saving = true;
        cx.notify();
    }

    pub(crate) fn finish_schema_change(
        &mut self,
        changed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.saving = false;
        if changed {
            self.load(TableQuery::default(), window, cx);
        }
        cx.notify();
    }

    pub(crate) fn focus(&self, window: &mut Window, cx: &mut App) {
        if let Some(table) = &self.table {
            table.focus_handle(cx).focus(window, cx);
        }
    }

    pub(crate) fn request(&self) -> Option<&OpenTable> {
        self.request.as_ref()
    }

    pub(crate) fn title(&self) -> String {
        format!("{} / {}", self.location, self.title)
    }

    pub(crate) fn new(request: OpenTable, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut view = Self::restore(TableDraft::new(&request));
        view.request = Some(request);
        view.load(view.query.clone(), window, cx);
        view
    }

    pub(crate) fn restore(draft: TableDraft) -> Self {
        Self {
            title: draft.table.clone(),
            location: format!("{} / {}", draft.connection_name, draft.database),
            query: draft.query.clone(),
            draft: Some(draft),
            ..Self::default()
        }
    }

    pub(crate) fn draft(&self) -> Option<TableDraft> {
        self.draft.as_ref().map(|draft| TableDraft {
            query: self.query.clone(),
            ..draft.clone()
        })
    }

    pub(crate) fn detach(&mut self, cx: &mut Context<Self>) {
        self.request = None;
        self.table = None;
        self.fields.clear();
        self.selected_row = None;
        self.inserting = false;
        self.filter_task = None;
        self.filter_pending = false;
        self.filter_column = None;
        self.filter_operator = None;
        self.filter_value = None;
        self.error = None;
        cx.emit(TableEvent::ClearSelection);
        cx.notify();
    }

    pub(crate) fn attach(
        &mut self,
        session: std::sync::Arc<crate::database::Session>,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(draft) = &mut self.draft else { return };
        if self.request.is_some()
            || session.connection_key() != draft.connection_key
            || !session
                .databases()
                .get(&draft.database)
                .is_some_and(|tables| tables.contains(&draft.table))
        {
            return;
        }
        draft.connection_name = name.clone();
        self.location = format!("{} / {}", name, draft.database);
        self.request = Some(OpenTable {
            session,
            connection_name: name,
            database: draft.database.clone(),
            table: draft.table.clone(),
        });
        self.load(self.query.clone(), window, cx);
        cx.emit(QueryChanged);
    }

    pub(crate) fn is_busy(&self) -> bool {
        self.loading || self.saving || self.row_reads > 0 || self.exporting || self.importing
    }

    fn load(&mut self, query: TableQuery, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading || self.saving || self.importing || self.is_dirty(cx) {
            return;
        }
        let Some(request) = self.request.clone() else {
            return;
        };
        self.filter_task = None;
        self.filter_pending = false;
        self.loading = true;
        self.error = None;
        self.fields.clear();
        self.selected_row = None;
        self.inserting = false;
        self.loading_row = false;
        self.load_generation += 1;
        let generation = self.load_generation;
        cx.emit(TableEvent::ClearSelection);
        let load_query = query.clone();
        let task = cx.background_spawn(async move {
            async_std::task::block_on(request.session.read_table(
                &request.database,
                &request.table,
                &load_query,
            ))
        });
        cx.spawn_in(window, async move |view, cx| {
            let result = task.await;
            let _ = view.update_in(cx, |view, window, cx| {
                if view.load_generation != generation {
                    return;
                }
                view.loading = false;
                match result {
                    Ok(data) => {
                        view.query = query;
                        view.finish(Ok(data), window, cx);
                        cx.emit(QueryChanged);
                    }
                    Err(error) => {
                        view.error = Some(error);
                        cx.notify();
                    }
                }
                if view.filter_pending {
                    view.apply_filter(window, cx);
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn schedule_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.filter_pending = false;
        self.filter_task = Some(cx.spawn_in(window, async move |view, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(300))
                .await;
            let _ = view.update_in(cx, |view, window, cx| view.apply_filter(window, cx));
        }));
    }

    fn apply_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.filter_task = None;
        self.filter_pending = false;
        if self.saving || self.importing || self.is_dirty(cx) {
            return;
        }
        if self.loading {
            self.filter_pending = true;
            return;
        }
        let (Some(column), Some(operator), Some(value)) = (
            &self.filter_column,
            &self.filter_operator,
            &self.filter_value,
        ) else {
            return;
        };
        let Some(selected) = column.read(cx).selected_index(cx) else {
            return;
        };
        let name = if selected.row == 0 {
            None
        } else {
            column.read(cx).selected_value().cloned()
        };
        let label = operator.read(cx).selected_value().cloned();
        let Some(operator) = FilterOperator::ALL
            .iter()
            .copied()
            .find(|operator| Some(operator.label()) == label.as_deref())
        else {
            return;
        };
        let value = value.read(cx).value().to_string();
        let filter = (!value.is_empty()).then(|| TableFilter {
            column: name.clone(),
            operator,
            value,
        });
        if self
            .query
            .filters
            .iter()
            .find(|filter| filter.column == name)
            == filter.as_ref()
        {
            return;
        }
        let mut query = self.query.clone();
        query.page = 0;
        query.filters.retain(|filter| filter.column != name);
        if let Some(filter) = filter {
            query.filters.push(filter);
        }
        self.load(query, window, cx);
    }

    fn finish(
        &mut self,
        result: Result<TableRows, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.fields.clear();
        self.selected_row = None;
        self.inserting = false;
        self.loading_row = false;
        self.load_generation += 1;
        match result {
            Ok(data) => {
                if self.filter_column.is_none() {
                    let column = cx.new(|cx| {
                        SelectState::new(
                            SearchableVec::new(
                                std::iter::once("All columns".to_owned())
                                    .chain(data.schema.columns.clone())
                                    .collect::<Vec<_>>(),
                            ),
                            Some(component::IndexPath::new(0)),
                            window,
                            cx,
                        )
                        .searchable(true)
                    });
                    cx.subscribe_in(
                        &column,
                        window,
                        |view, column, event: &SelectEvent<SearchableVec<String>>, window, cx| {
                            view.filter_task = None;
                            view.filter_pending = false;
                            let SelectEvent::Confirm(name) = event;
                            let name = if column
                                .read(cx)
                                .selected_index(cx)
                                .is_some_and(|ix| ix.row == 0)
                            {
                                None
                            } else {
                                name.clone()
                            };
                            let filter = view
                                .query
                                .filters
                                .iter()
                                .find(|filter| filter.column == name);
                            let value = filter
                                .map(|filter| filter.value.clone())
                                .unwrap_or_default();
                            let operator =
                                filter.map_or(FilterOperator::Contains, |filter| filter.operator);
                            if let Some(select) = &view.filter_operator {
                                select.update(cx, |select, cx| {
                                    select.set_selected_value(
                                        &operator.label().to_owned(),
                                        window,
                                        cx,
                                    )
                                });
                            }
                            if let Some(input) = &view.filter_value {
                                input.update(cx, |input, cx| input.set_value(value, window, cx));
                            }
                        },
                    )
                    .detach();
                    self.filter_column = Some(column);
                    let filter = self
                        .query
                        .filters
                        .iter()
                        .find(|filter| filter.column.is_none());
                    let operator =
                        filter.map_or(FilterOperator::Contains, |filter| filter.operator);
                    let value = filter
                        .map(|filter| filter.value.clone())
                        .unwrap_or_default();
                    self.filter_operator = Some(cx.new(|cx| {
                        SelectState::new(
                            FilterOperator::ALL
                                .iter()
                                .map(|operator| operator.label().to_owned())
                                .collect::<Vec<_>>(),
                            Some(component::IndexPath::new(
                                FilterOperator::ALL
                                    .iter()
                                    .position(|value| *value == operator)
                                    .unwrap(),
                            )),
                            window,
                            cx,
                        )
                    }));
                    cx.subscribe_in(
                        self.filter_operator.as_ref().unwrap(),
                        window,
                        |view, _, _: &SelectEvent<Vec<String>>, window, cx| {
                            view.schedule_filter(window, cx);
                        },
                    )
                    .detach();
                    let input = cx.new(|cx| {
                        let mut input =
                            InputState::new(window, cx).placeholder("Value; empty removes filter");
                        input.set_value(value, window, cx);
                        input
                    });
                    cx.subscribe_in(&input, window, |view, _, event: &InputEvent, window, cx| {
                        match event {
                            InputEvent::Change => view.schedule_filter(window, cx),
                            InputEvent::PressEnter { .. } => view.apply_filter(window, cx),
                            _ => {}
                        }
                    })
                    .detach();
                    self.filter_value = Some(input);
                }
                let columns = data
                    .schema
                    .columns
                    .iter()
                    .map(|name| {
                        Column::new(name.clone(), name.clone())
                            .width(rems(12.).to_pixels(window.rem_size()))
                            .p_0()
                            .movable(false)
                    })
                    .collect();
                let owner = cx.entity().downgrade();
                let table = cx.new(|cx| {
                    TableState::new(
                        Rows {
                            columns,
                            data,
                            owner,
                        },
                        window,
                        cx,
                    )
                    .col_movable(false)
                    .cell_selectable(true)
                    .row_header(false)
                });
                cx.subscribe_in(&table, window, |view, table, event, window, cx| {
                    if view.loading || view.importing {
                        return;
                    }
                    match event {
                        TableEvent::SelectRow(ix) | TableEvent::SelectCell(ix, _) => {
                            if view.selected_row == Some(*ix) {
                                return;
                            }
                            if view.is_dirty(cx) || view.saving {
                                view.edit_error = Some(
                                    "Save or cancel your edits before selecting another row."
                                        .into(),
                                );
                                if let Some(selected) = view.selected_row {
                                    table.update(cx, |table, cx| {
                                        table.set_selected_row(selected, cx)
                                    });
                                }
                                cx.notify();
                                return;
                            }
                            view.select_row(*ix, window, cx);
                        }
                        TableEvent::ClearSelection | TableEvent::SelectColumn(_) => {
                            if view.saving || view.importing {
                                return;
                            }
                            if view.is_dirty(cx) {
                                view.cancel(window, cx);
                            } else {
                                view.fields.clear();
                                view.selected_row = None;
                                view.loading_row = false;
                                view.load_generation += 1;
                            }
                        }
                        _ => return,
                    }
                    cx.emit(event.clone());
                    cx.notify();
                })
                .detach();
                self.table = Some(table);
                self.error = None;
            }
            Err(error) => {
                self.table = None;
                self.error = Some(error);
            }
        }
        cx.notify();
    }

    fn export_rows(&mut self, format: ExportFormat, window: &mut Window, cx: &mut Context<Self>) {
        if self.importing
            || self.exporting
            || self.loading
            || self.saving
            || self.loading_row
            || self.filter_pending
            || self.is_dirty(cx)
        {
            return;
        }
        let Some(request) = self.request.clone() else {
            return;
        };
        let query = self.query.clone();
        let directory = dirs::download_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| ".".into());
        let picker = cx.prompt_for_new_path(
            &directory,
            Some(&format!("table-results.{}", format.extension())),
        );
        self.exporting = true;
        self.export_failed = false;
        self.export_status = Some("Choose an export file…".into());
        cx.notify();
        cx.spawn_in(window, async move |view, cx| {
            let result: anyhow::Result<Option<usize>> = async {
                let Some(path) = picker.await?? else {
                    return Ok(None);
                };
                let _ = view.update_in(cx, |view, _, cx| {
                    view.export_status = Some("Exporting all matching rows…".into());
                    cx.notify();
                });
                cx.background_spawn(async move {
                    async_std::task::block_on(request.session.export_table(
                        &request.database,
                        &request.table,
                        &query,
                        &path,
                        format,
                    ))
                    .map(Some)
                })
                .await
            }
            .await;
            let _ = view.update_in(cx, |view, _, cx| {
                view.exporting = false;
                view.export_status = match result {
                    Ok(Some(rows)) => Some(format!("Exported {rows} matching rows.")),
                    Ok(None) => None,
                    Err(error) => {
                        view.export_failed = true;
                        Some(format!("Couldn’t export table: {error}."))
                    }
                };
                cx.notify();
            });
        })
        .detach();
    }

    fn select_row(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.importing {
            return;
        }
        let Some(table) = &self.table else {
            return;
        };
        let data = &table.read(cx).delegate().data;
        let Some(preview) = data.rows.get(ix).cloned() else {
            return;
        };
        let schema = data.schema.clone();
        let key = data.keys.get(ix).cloned().unwrap_or_default();
        self.selected_row = Some(ix);
        self.fields.clear();
        self.edit_error = None;
        self.load_generation += 1;
        let generation = self.load_generation;
        let Some(request) = self.request.clone() else {
            self.set_fields(&schema, preview, window, cx);
            return;
        };
        if schema.primary_key.is_empty() {
            self.set_fields(&schema, preview, window, cx);
            self.edit_error = Some("Editing requires a table with a primary key.".into());
            return;
        }
        self.loading_row = true;
        self.row_reads += 1;
        let load_schema = schema.clone();
        let task = cx.background_spawn(async move {
            async_std::task::block_on(request.session.read_row(
                &request.database,
                &request.table,
                &load_schema,
                &key,
            ))
        });
        cx.spawn_in(window, async move |view, cx| {
            let result = task.await;
            let _ = view.update_in(cx, |view, window, cx| {
                view.row_reads -= 1;
                cx.notify();
                if view.load_generation != generation {
                    return;
                }
                view.loading_row = false;
                match result {
                    Ok(values) => view.set_fields(&schema, values, window, cx),
                    Err(error) => view.edit_error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn set_fields(
        &mut self,
        schema: &TableSchema,
        values: Vec<Option<String>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.fields = schema
            .columns
            .iter()
            .cloned()
            .zip(values)
            .enumerate()
            .map(|(ix, (name, original))| {
                let input = cx.new(|cx| {
                    let mut input = TextareaState::new(window, cx)
                        .auto_grow(1, 4)
                        .placeholder(if original.is_none() { "NULL" } else { "" });
                    input.set_value(original.clone().unwrap_or_default(), window, cx);
                    input
                });
                cx.subscribe_in(
                    &input,
                    window,
                    move |view, input, event: &InputEvent, window, cx| {
                        if !matches!(event, InputEvent::Change) {
                            return;
                        }
                        if let Some(field) = view.fields.get_mut(ix)
                            && field.input == *input
                            && field.editable
                        {
                            field.is_null = false;
                            input.update(cx, |input, cx| input.set_placeholder("", window, cx));
                            view.edit_error = None;
                            cx.notify();
                        }
                    },
                )
                .detach();
                RowField {
                    name: name.into(),
                    input,
                    is_null: original.is_none(),
                    use_default: false,
                    original,
                    editable: schema.is_editable(ix),
                }
            })
            .collect();
    }

    pub(crate) fn is_dirty(&self, cx: &App) -> bool {
        self.inserting || self.fields.iter().any(|field| field.is_edited(cx))
    }

    pub(crate) fn prepare_to_close(&mut self, cx: &mut Context<Self>) -> bool {
        if self.is_dirty(cx) || self.is_busy() {
            self.edit_error = Some(
                "Save or cancel your edits and wait for table operations before closing this tab."
                    .into(),
            );
            cx.notify();
            false
        } else {
            true
        }
    }

    pub(crate) fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving || self.importing {
            return;
        }
        if self.inserting {
            self.inserting = false;
            self.fields.clear();
            self.selected_row = None;
            self.edit_error = None;
            cx.emit(TableEvent::ClearSelection);
            self.focus(window, cx);
            cx.notify();
            return;
        }
        for field in &mut self.fields {
            field.is_null = field.original.is_none();
            field.input.update(cx, |input, cx| {
                input.set_value(field.original.clone().unwrap_or_default(), window, cx);
                input.set_placeholder(if field.is_null { "NULL" } else { "" }, window, cx);
            });
        }
        self.edit_error = None;
        cx.notify();
    }

    pub(crate) fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving || self.importing || self.loading_row || !self.is_dirty(cx) {
            return;
        }
        if self.inserting {
            self.insert(window, cx);
            return;
        }
        let (Some(request), Some(table), Some(ix)) =
            (self.request.clone(), self.table.clone(), self.selected_row)
        else {
            self.edit_error = Some("This preview is not connected to a database.".into());
            cx.notify();
            return;
        };
        let schema = table.read(cx).delegate().data.schema.clone();
        let original = self
            .fields
            .iter()
            .map(|field| field.original.clone())
            .collect::<Vec<_>>();
        let values = self
            .fields
            .iter()
            .map(|field| field.value(cx))
            .collect::<Vec<_>>();
        self.saving = true;
        self.edit_error = None;
        cx.notify();
        let save_schema = schema.clone();
        let task = cx.background_spawn(async move {
            async_std::task::block_on(request.session.save_row(
                &request.database,
                &request.table,
                &save_schema,
                &original,
                &values,
            ))
        });
        cx.spawn_in(window, async move |view, cx| {
            let result = task.await;
            let _ = view.update_in(cx, |view, window, cx| {
                view.saving = false;
                match result {
                    Ok(values) => {
                        table.update(cx, |table, cx| {
                            let data = &mut table.delegate_mut().data;
                            data.rows[ix] = values
                                .iter()
                                .cloned()
                                .map(|value| value.map(truncate_preview))
                                .collect();
                            data.keys[ix] = schema.key(&values);
                            cx.notify();
                        });
                        for (field, value) in view.fields.iter_mut().zip(values) {
                            field.original = value;
                        }
                        view.cancel(window, cx);
                    }
                    Err(error) => view.edit_error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn render_sidebar(&self, cx: &Context<Self>) -> Div {
        let dirty = self.is_dirty(cx);
        div()
            .when_some(self.editor_focus.as_ref(), |panel, focus| panel.track_focus(focus))
            .flex()
            .flex_col()
            .gap_3()
            .p_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(if self.inserting { "New row" } else { "Row details" }),
                    )
                    .when(dirty || self.saving, |header| {
                        header.child(
                            div()
                                .id("row-edit-status")
                                .test_support()
                                .role(Role::Status)
                                .aria_label(if self.saving { "Saving…" } else { "Edited" })
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(if self.saving { "Saving…" } else { "Edited" }),
                        )
                    }),
            )
            .when(self.loading_row, |panel| panel.child("Loading row…"))
            .when(
                self.fields.is_empty() && !self.loading_row && self.edit_error.is_none(),
                |panel| {
                    panel.child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("Select a row to view its fields"),
                    )
                },
            )
            .when_some(self.edit_error.as_ref(), |panel, error| {
                panel.child(
                    div()
                        .id("row-edit-error")
                        .test_support()
                        .role(Role::Status)
                        .aria_label(error.clone())
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(error.clone()),
                )
            })
            .when(!self.fields.is_empty() && !self.inserting, |panel| {
                let disabled = self.is_busy() || dirty || self.filter_pending || self.selected_row.is_none() || self.request.as_ref().is_none_or(|request| !request.session.is_connected()) || self.table.as_ref().is_none_or(|table| table.read(cx).delegate().data.schema.primary_key.is_empty());
                panel.child(div().flex().flex_wrap().gap_2()
                    .child(Button::new("duplicate-row").small().outline().label("Duplicate…").disabled(disabled)
                        .on_click(cx.listener(|view, _, window, cx| view.new_row(true, window, cx))))
                    .child(Button::new("delete-row").small().outline().label("Delete…").disabled(disabled)
                        .on_click(cx.listener(|view, _, window, cx| view.confirm_delete(window, cx)))))
            })
            .when(self.inserting, |panel| panel.child(div().text_sm().text_color(cx.theme().muted_foreground)
                .child("Unchecked defaults use the value or NULL below. Change copied unique keys before inserting.")))
            .child(
                Form::new()
                    .children(self.fields.iter().enumerate().map(|(ix, field)| {
                        Field::new()
                            .label(field.name.clone())
                            .when(!field.editable, |field| {
                                field.description("Cannot be edited")
                            })
                            .when(self.inserting, |element| {
                                element.child(Checkbox::new(SharedString::from(format!("row-default-{}", field.name)))
                                    .label("Use database default")
                                    .accessibility_label(format!("Use database default for {}", field.name))
                                    .checked(field.use_default)
                                    .disabled(self.saving || !field.editable)
                                    .on_click(cx.listener(move |view, checked, _, cx| {
                                        if view.saving { return; }
                                        if let Some(field) = view.fields.get_mut(ix) && field.editable {
                                            field.use_default = *checked;
                                            view.edit_error = None;
                                            cx.notify();
                                        }
                                    })))
                            })
                            .child(
                                Checkbox::new(SharedString::from(format!(
                                    "row-null-{}",
                                    field.name
                                )))
                                .label("Use NULL")
                                .accessibility_label(format!("Use NULL for {}", field.name))
                                .checked(field.is_null)
                                .disabled(self.saving || self.importing || !field.editable || field.use_default)
                                .on_click(cx.listener(
                                    move |view, checked, _, cx| {
                                        if view.saving || view.importing {
                                            return;
                                        }
                                        if let Some(field) = view.fields.get_mut(ix)
                                            && field.editable && !field.use_default
                                        {
                                            field.is_null = *checked;
                                            view.edit_error = None;
                                            cx.notify();
                                        }
                                    },
                                )),
                            )
                            .child(
                                Textarea::new(&field.input)
                                    .accessibility_id(SharedString::from(format!(
                                        "row-field-{}",
                                        field.name
                                    )))
                                    .aria_label(field.name.clone())
                                    .disabled(
                                        self.saving
                                            || self.importing
                                            || !field.editable
                                            || field.is_null
                                            || field.use_default,
                                    )
                                    .small(),
                            )
                    }))
                    .footer(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                Button::new("save-row")
                                    .small()
                                    .label(if self.inserting { "Insert" } else { "Save" })
                                    .disabled(
                                        !dirty
                                            || self.saving
                                            || self.request.as_ref().is_none_or(|request| {
                                                !request.session.is_connected()
                                            }),
                                    )
                                    .on_click(|_, window, cx| {
                                        window.dispatch_action(Box::new(SaveRow), cx)
                                    }),
                            )
                            .child(
                                Button::new("cancel-row")
                                    .small()
                                    .outline()
                                    .label("Cancel")
                                    .disabled(!dirty || self.saving || self.importing)
                                    .on_click(|_, window, cx| {
                                        window.dispatch_action(Box::new(CancelRow), cx)
                                    }),
                            ),
                    ),
            )
    }
}

impl Render for TableView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let status = if self.request.is_none() && self.draft.is_some() {
            Some("Offline. Connect to the matching saved connection to restore this table’s filters, sorting, and page.".to_owned())
        } else if let Some(error) = self
            .request
            .as_ref()
            .and_then(|request| request.session.error())
        {
            Some(format!(
                "{error}. Cancel row edits before reconnecting in the sidebar."
            ))
        } else if let Some(error) = &self.error {
            Some(error.clone())
        } else if self.table.is_none() {
            Some("Loading table…".to_owned())
        } else {
            None
        };
        div()
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .min_h_0()
            .child(
                div()
                    .id("table-title")
                    .test_support()
                    .aria_label(format!("{} / {}", self.location, self.title))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_3()
                    .py_2()
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .truncate()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(self.title.clone()),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(self.location.clone()),
                    ),
            )
            .when_some(
                self.filter_column
                    .as_ref()
                    .zip(self.filter_operator.as_ref())
                    .zip(self.filter_value.as_ref()),
                |view, ((column, operator), value)| {
                    let busy = self.loading || self.saving || self.importing || self.is_dirty(cx);
                    view.child(
                        div().id("filter-bar").test_support().flex_shrink_0().child(
                            div()
                                .h_10()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .px_3()
                                        .py_2()
                                        .min_w(rems(34.))
                                        .child(
                                            div().w_32().flex_none().child(
                                                Select::new(column)
                                                    .id("filter-column")
                                                    .accessibility_label("Filter column")
                                                    .search_placeholder("Search columns")
                                                    .small()
                                                    .disabled(busy),
                                            ),
                                        )
                                        .child(
                                            div().w_48().flex_none().child(
                                                Select::new(operator)
                                                    .id("filter-operator")
                                                    .accessibility_label("Filter operator")
                                                    .small()
                                                    .disabled(busy),
                                            ),
                                        )
                                        .child(
                                            Input::new(value)
                                                .id("filter-value")
                                                .aria_label("Filter value")
                                                .small()
                                                .flex_1()
                                                .min_w_16()
                                                .disabled(self.saving || self.importing || self.is_dirty(cx)),
                                        )
                                        .child(
                                            Button::new("apply-filter")
                                                .small()
                                                .flex_shrink_0()
                                                .label("Apply")
                                                .disabled(self.saving || self.importing || self.is_dirty(cx))
                                                .on_click(cx.listener(|view, _, window, cx| {
                                                    view.apply_filter(window, cx)
                                                })),
                                        )
                                        .child(
                                            Button::new("clear-filters")
                                                .small()
                                                .flex_shrink_0()
                                                .outline()
                                                .label("Clear")
                                                .accessibility_label("Clear filters")
                                                .disabled(busy || self.query.filters.is_empty())
                                                .on_click(cx.listener(|view, _, window, cx| {
                                                    view.filter_task = None;
                                                    view.filter_pending = false;
                                                    let mut query = view.query.clone();
                                                    query.page = 0;
                                                    query.filters.clear();
                                                    if let Some(input) = &view.filter_value {
                                                        input.update(cx, |input, cx| {
                                                            input.set_value("", window, cx)
                                                        });
                                                    }
                                                    view.load(query, window, cx);
                                                })),
                                        ),
                                )
                                .overflow_x_scrollbar(),
                        ),
                    )
                    .when(!self.query.filters.is_empty(), |view| {
                        let summary = self
                            .query
                            .filters
                            .iter()
                            .map(|filter| {
                                format!(
                                    "{} {} {:?}",
                                    filter.column.as_deref().unwrap_or("All columns"),
                                    filter.operator.label().to_lowercase(),
                                    filter.value
                                )
                            })
                            .collect::<Vec<_>>()
                            .join(" · ");
                        let tooltip_summary = summary.clone();
                        view.child(
                            div()
                                .id("active-filters")
                                .test_support()
                                .aria_label(summary.clone())
                                .truncate()
                                .flex_shrink_0()
                                .tooltip(move |window, cx| {
                                    Tooltip::new(tooltip_summary.clone()).build(window, cx)
                                })
                                .px_3()
                                .pb_2()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(summary),
                        )
                    })
                },
            )
            .when_some(status, |view, status| {
                view.child(
                    div()
                        .id("table-status")
                        .test_support()
                        .role(Role::Status)
                        .aria_label(status.clone())
                        .px_3()
                        .py_2()
                        .flex_shrink_0()
                        .when(self.table.is_none(), |status| status.flex_1())
                        .text_sm()
                        .text_color(if self.error.is_some() {
                            cx.theme().danger
                        } else {
                            cx.theme().muted_foreground
                        })
                        .child(status),
                )
            })
            .when(self.table.is_none() && self.error.is_some() && self.request.is_some(), |view| {
                view.child(div().flex().gap_2().px_3().py_2()
                    .child(Button::new("retry-table").small().label("Retry").disabled(self.is_busy())
                        .on_click(cx.listener(|view, _, window, cx| view.load(view.query.clone(), window, cx))))
                    .child(Button::new("reset-table-query").small().outline().label("Reset filters and sorting").disabled(self.is_busy())
                        .on_click(cx.listener(|view, _, window, cx| view.load(TableQuery::default(), window, cx)))))
            })
            .when_some(self.import_status.as_ref(), |view, status| {
                view.child(div().id("table-import-status").test_support().role(Role::Status)
                    .aria_label(status.clone()).px_3().py_2().text_sm()
                    .text_color(if self.import_failed { cx.theme().danger } else { cx.theme().muted_foreground })
                    .child(status.clone()))
            })
            .when_some(self.export_status.as_ref(), |view, status| {
                view.child(div().id("table-export-status").test_support().role(Role::Status)
                    .aria_label(status.clone()).px_3().py_2().text_sm()
                    .text_color(if self.export_failed { cx.theme().danger } else { cx.theme().muted_foreground })
                    .child(status.clone()))
            })
            .when_some(self.table.as_ref(), |view, table| {
                view.child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .min_w_0()
                        .pl_3()
                        .child(DataTable::new(table).small().bordered(false).stripe(true)),
                )
                .child({
                    let data = &table.read(cx).delegate().data;
                    let busy = self.loading || self.saving || self.importing || self.is_dirty(cx);
                    let start = self.query.page * PAGE_SIZE;
                    let range = if data.rows.is_empty() {
                        "No rows".to_owned()
                    } else {
                        format!("Rows {}–{}", start + 1, start + data.rows.len())
                    };
                    let label = format!("Page {} · {range}", self.query.page + 1);
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .px_3()
                        .py_2()
                        .flex_shrink_0()
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .child(
                            Button::new("previous-page")
                                .small()
                                .outline()
                                .label("Previous")
                                .disabled(busy || self.query.page == 0)
                                .on_click(cx.listener(|view, _, window, cx| {
                                    if view.query.page == 0 {
                                        return;
                                    }
                                    let mut query = view.query.clone();
                                    query.page -= 1;
                                    view.load(query, window, cx);
                                })),
                        )
                        .child(
                            div()
                                .id("page-status")
                                .test_support()
                                .role(Role::Status)
                                .aria_label(label.clone())
                                .text_xs()
                                .child(label),
                        )
                        .child(
                            Button::new("next-page")
                                .small()
                                .outline()
                                .label("Next")
                                .disabled(busy || !data.has_more)
                                .on_click(cx.listener(|view, _, window, cx| {
                                    let mut query = view.query.clone();
                                    query.page += 1;
                                    view.load(query, window, cx);
                                })),
                        )
                        .child(
                            Button::new("insert-row").small().ghost().label("Insert row…")
                                .disabled(self.is_busy() || self.is_dirty(cx) || self.filter_pending || self.request.as_ref().is_none_or(|request| !request.session.is_connected()))
                                .on_click(cx.listener(|view, _, window, cx| view.new_row(false, window, cx))),
                        )
                        .child(
                            Button::new("import-table-csv")
                                .small().ghost().label("Import CSV…")
                                .disabled(self.is_busy() || self.is_dirty(cx) || self.filter_pending || self.request.as_ref().is_none_or(|request| !request.session.is_connected()))
                                .on_click(cx.listener(|view, _, window, cx| view.choose_import(window, cx))),
                        )
                        .child(
                            Button::new("export-table-csv")
                                .small().ghost().label("Export CSV…")
                                .disabled(busy || self.exporting || self.loading_row || self.filter_pending || self.request.is_none())
                                .tooltip("Export all matching rows with full values, using the applied filters and sort order")
                                .on_click(cx.listener(|view, _, window, cx| view.export_rows(ExportFormat::Csv, window, cx))),
                        )
                        .child(
                            Button::new("export-table-json")
                                .small().ghost().label("Export JSON…")
                                .disabled(busy || self.exporting || self.loading_row || self.filter_pending || self.request.is_none())
                                .tooltip("Export all matching rows with full values, using the applied filters and sort order")
                                .on_click(cx.listener(|view, _, window, cx| view.export_rows(ExportFormat::Json, window, cx))),
                        )
                        .child(
                            Button::new("refresh-table")
                                .small()
                                .ghost()
                                .label("Refresh")
                                .disabled(busy)
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.load(view.query.clone(), window, cx)
                                })),
                        )
                })
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::TableView;
    use crate::database::{TableRows, TableSchema};
    use gpui_kit::{
        App, AppContext, ElementId, Entity, Focusable, ScrollDelta, SharedString, TestAppContext,
        component::{Root, Size, WindowExt},
        point, px, size,
        test::TestWindowExt,
    };

    fn field_id(view: &Entity<TableView>, name: &str, cx: &App) -> ElementId {
        view.read(cx)
            .fields
            .iter()
            .find(|field| field.name.as_ref() == name)
            .map(|field| ("input", field.input.entity_id()).into())
            .unwrap_or_else(|| SharedString::from(format!("row-field-{name}")).into())
    }

    #[gpui_kit::test]
    fn mixed_workspace_restores_offline_and_close_protects_work(cx: &mut TestAppContext) {
        use crate::{
            database::{FilterOperator, TableFilter, TableQuery},
            workspace_state::{SavedTab, SqlDraft, TableDraft, WorkspaceFile, WorkspaceState},
        };
        let directory =
            std::env::temp_dir().join(format!("tablelane-table-recovery-{}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("workspace.json");
        let draft = TableDraft {
            connection_key: "offline-key".into(),
            connection_name: "Local".into(),
            database: "db".into(),
            table: "widgets".into(),
            query: TableQuery {
                page: 2,
                sort: Some(("name".into(), true)),
                filters: vec![TableFilter {
                    column: None,
                    operator: FilterOperator::EndsWith,
                    value: "世界".into(),
                }],
            },
        };
        let mut state = WorkspaceState::default();
        state.tabs = vec![
            SavedTab::Table(draft.clone()),
            SavedTab::Sql(SqlDraft {
                number: 5,
                connection_key: draft.connection_key.clone(),
                connection_name: draft.connection_name.clone(),
                database: draft.database.clone(),
                table: draft.table.clone(),
                sql: "SELECT 'keep draft'".into(),
            }),
        ];
        state.active = 0;
        assert!(WorkspaceFile::load(Some(path.clone())).0.save(&state));
        cx.update(gpui_kit::init);
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
        let table = cx.update(|cx| {
            let crate::WorkspaceTab::Table(table) = &app.read(cx).tabs[0] else {
                panic!("Table missing")
            };
            assert_eq!(table.read(cx).draft(), Some(draft.clone()));
            assert!(table.read(cx).request().is_none());
            assert!(!table.read(cx).is_busy());
            assert_eq!(app.read(cx).capture_workspace(cx), state);
            table.clone()
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window
                    .find("table-status")
                    .label()
                    .unwrap()
                    .starts_with("Offline.")
            );
            window.press("ctrl-tab", cx);
            assert_eq!(app.read(cx).active_tab, 1);
            window.press("ctrl-shift-tab", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.background_executor
            .advance_clock(std::time::Duration::from_millis(300));
        cx.run_until_parked();
        assert_eq!(WorkspaceFile::load(Some(path.clone())).1, state);
        // Every kind of table operation blocks both native close and tab close.
        for operation in 0..5 {
            cx.update(|cx| {
                table.update(cx, |table, _| {
                    table.loading = operation == 0;
                    table.saving = operation == 1;
                    table.exporting = operation == 2;
                    table.importing = operation == 4;
                    table.row_reads = usize::from(operation == 3);
                })
            });
            let mut visual = gpui_kit::VisualTestContext::from_window(handle.into(), cx);
            assert!(!visual.simulate_close());
            cx.update_window(handle.into(), |_, window, cx| {
                window.render_frame(cx);
                assert_eq!(window.within("dialog").find("ok").label(), Some("OK"));
                window.press("escape", cx);
            })
            .unwrap();
            cx.run_until_parked();
            cx.update_window(handle.into(), |_, window, cx| {
                window.click(("close-tab", table.entity_id()), cx);
                assert_eq!(app.read(cx).tabs.len(), 2);
            })
            .unwrap();
        }
        cx.update_window(handle.into(), |_, window, cx| {
            table.update(cx, |table, cx| {
                table.loading = false;
                table.saving = false;
                table.exporting = false;
                table.importing = false;
                table.row_reads = 0;
                table.set_fields(
                    &TableSchema {
                        columns: vec!["name".into()],
                        column_types: vec!["text".into()],
                        ..TableSchema::default()
                    },
                    vec![Some("original".into())],
                    window,
                    cx,
                );
                table.fields[0]
                    .input
                    .update(cx, |input, cx| input.set_value("unsaved", window, cx));
            });
            // Quit must inspect inactive tabs too.
            app.update(cx, |app, cx| app.select_tab(1, window, cx));
        })
        .unwrap();
        cx.run_until_parked();
        let mut visual = gpui_kit::VisualTestContext::from_window(handle.into(), cx);
        assert!(!visual.simulate_close());
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(
                window.within("dialog").find("ok").label(),
                Some("Discard edits and close")
            );
            window.press("escape", cx);
            assert!(table.read(cx).is_dirty(cx));
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            // The app Quit route uses the same guard as native window close.
            app.update(cx, |app, cx| assert!(!app.prepare_close(true, window, cx)));
            window.press("escape", cx);
        })
        .unwrap();
        cx.run_until_parked();
        let mut visual = gpui_kit::VisualTestContext::from_window(handle.into(), cx);
        assert!(!visual.simulate_close());
        cx.update_window(handle.into(), |_, window, cx| {
            // Recheck operations at confirmation time as well.
            table.update(cx, |table, _| table.loading = true);
            window.within("dialog").click("ok", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(window.has_active_dialog(cx));
            table.update(cx, |table, _| table.loading = false);
            window.within("dialog").click("ok", cx);
        })
        .unwrap();
        cx.run_until_parked();
        let saved = WorkspaceFile::load(Some(path.clone())).1;
        assert_eq!(saved.tabs, state.tabs);
        assert_eq!(saved.active, 1);
        assert!(!std::fs::read_to_string(path).unwrap().contains("unsaved"));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[gpui_kit::test]
    #[ignore = "requires an isolated MariaDB server and TABLELANE_TEST_MARIADB_PORT"]
    async fn mariadb_connection_lifecycle_preserves_drafts_guards_work_and_refreshes_metadata(
        cx: &mut TestAppContext,
    ) {
        use crate::{
            database::{FilterOperator, TableFilter, TableQuery},
            settings::{Connection, DatabaseKind, Settings},
            workspace_state::{SavedTab, SqlDraft, TableDraft, WorkspaceFile, WorkspaceState},
        };
        use gpui_kit::{
            AppContext,
            component::Root,
            test::{TestAppContextExt, TestWindowExt},
        };
        let config = Connection {
            name: "Lifecycle".into(),
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
        let admin = async_std::task::block_on(crate::database::connect(&config)).unwrap();
        async_std::task::block_on(async {
            admin
                .run_sql("mysql", "CREATE DATABASE lifecycle_ui_test")
                .await
                .unwrap();
            admin
                .run_sql(
                    "lifecycle_ui_test",
                    "CREATE TABLE widgets (id INT PRIMARY KEY, name TEXT)",
                )
                .await
                .unwrap();
            admin
                .run_sql(
                    "lifecycle_ui_test",
                    "INSERT INTO widgets VALUES (1, 'row one'), (2, 'row two')",
                )
                .await
                .unwrap();
        });
        let directory =
            std::env::temp_dir().join(format!("tablelane-lifecycle-ui-{}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let draft = TableDraft {
            connection_key: config.key(),
            connection_name: config.name.clone(),
            database: "lifecycle_ui_test".into(),
            table: "widgets".into(),
            query: TableQuery {
                sort: Some(("id".into(), true)),
                filters: vec![TableFilter {
                    column: Some("name".into()),
                    operator: FilterOperator::StartsWith,
                    value: "row".into(),
                }],
                ..TableQuery::default()
            },
        };
        let sql = "SELECT @lifecycle_runs := COALESCE(@lifecycle_runs, 0) + 1";
        let mut state = WorkspaceState::default();
        state.tabs = vec![
            SavedTab::Table(draft.clone()),
            SavedTab::Sql(SqlDraft {
                connection_key: config.key(),
                connection_name: config.name.clone(),
                database: draft.database.clone(),
                table: draft.table.clone(),
                number: 1,
                sql: sql.into(),
            }),
        ];
        assert!(
            WorkspaceFile::load(Some(directory.join("workspace.json")))
                .0
                .save(&state)
        );
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::sql_editor::SqlEditor::isolate_test_history(cx);
            cx.set_global(Settings {
                connections: vec![config.clone()],
                ..Settings::default()
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
        let (table, editor) = cx.update(|cx| {
            let crate::WorkspaceTab::Table(table) = &app.read(cx).tabs[0] else {
                panic!("table")
            };
            let crate::WorkspaceTab::Sql(editor, _) = &app.read(cx).tabs[1] else {
                panic!("editor")
            };
            (table.clone(), editor.clone())
        });
        let timeout = std::time::Duration::from_secs(15);
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("connection-1", cx);
            window.click("reconnect-connection", cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| {
            table.read(cx).table.is_some() && editor.read(cx).request().is_some()
        })
        .await;
        let first = cx.update(|cx| table.read(cx).request().unwrap().session.clone());
        assert_eq!(
            async_std::task::block_on(first.run_sql("lifecycle_ui_test", "SELECT @lifecycle_runs"))
                .unwrap()
                .0
                .rows[0][0],
            None
        );
        // Refresh keeps the same SQL session and exposes newly created metadata.
        async_std::task::block_on(admin.run_sql(
            "lifecycle_ui_test",
            "CREATE TABLE added (id INT PRIMARY KEY)",
        ))
        .unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("connection-1/database/lifecycle_ui_test", cx);
            assert!(
                window
                    .try_find("connection-1/database/lifecycle_ui_test/table/added")
                    .is_none()
            );
            window.click("refresh-connection-metadata", cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| {
            !app.read(cx).connections.read(cx).is_connecting()
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(
                window
                    .find("connection-1/database/lifecycle_ui_test/table/added")
                    .visible()
            );
            assert!(std::sync::Arc::ptr_eq(
                &first,
                &editor.read(cx).request().unwrap().session
            ));
            app.update(cx, |view, cx| view.open_structure(window, cx));
        })
        .unwrap();
        let structure = cx.update(|cx| {
            let crate::WorkspaceTab::Structure(view) = &app.read(cx).tabs[2] else {
                panic!("structure")
            };
            view.clone()
        });
        cx.wait_for(handle.into(), timeout, |_, cx| {
            !structure.read(cx).is_busy()
        })
        .await;
        // All table operation types protect even inactive tabs.
        for operation in 0..5 {
            cx.update_window(handle.into(), |_, window, cx| {
                table.update(cx, |table, _| {
                    table.loading = operation == 0;
                    table.saving = operation == 1;
                    table.row_reads = usize::from(operation == 2);
                    table.exporting = operation == 3;
                    table.importing = operation == 4;
                });
                window.click("reconnect-connection", cx);
            })
            .unwrap();
            cx.run_until_parked();
            cx.update_window(handle.into(), |_, window, cx| {
                assert_eq!(window.within("dialog").find("ok").label(), Some("OK"));
                assert!(first.is_connected());
                window.press("escape", cx);
                table.update(cx, |table, _| {
                    table.loading = false;
                    table.saving = false;
                    table.row_reads = 0;
                    table.exporting = false;
                    table.importing = false;
                });
            })
            .unwrap();
            cx.run_until_parked();
        }
        cx.update_window(handle.into(), |_, window, cx| {
            table.update(cx, |table, cx| {
                let schema = table
                    .table
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .delegate()
                    .data
                    .schema
                    .clone();
                table.set_fields(
                    &schema,
                    vec![Some("1".into()), Some("row one".into())],
                    window,
                    cx,
                );
                table.fields[1]
                    .input
                    .update(cx, |input, cx| input.set_value("unsaved", window, cx));
            });
            window.click("disconnect-connection", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            assert_eq!(window.within("dialog").find("ok").label(), Some("OK"));
            assert!(table.read(cx).is_dirty(cx));
            window.press("escape", cx);
            table.update(cx, |table, _| table.fields.clear());
            app.update(cx, |view, cx| view.select_tab(1, window, cx));
            window.click("run-sql", cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| !editor.read(cx).is_busy())
            .await;
        cx.update_window(handle.into(), |_, window, cx| {
            assert_ne!(window.find("export-sql-csv").disabled(), Some(true));
            editor.update(cx, |editor, cx| {
                editor.set_sql("SELECT SLEEP(0.3)".into(), window, cx)
            });
            window.click("run-sql", cx);
            window.click("disconnect-connection", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            assert_eq!(window.within("dialog").find("ok").label(), Some("OK"));
            assert!(first.is_connected());
            window.press("escape", cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| !editor.read(cx).is_busy())
            .await;
        cx.update_window(handle.into(), |_, window, cx| {
            editor.update(cx, |editor, cx| editor.set_sql(sql.into(), window, cx));
            window.click("disconnect-connection", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            assert_eq!(
                window.within("dialog").find("ok").label(),
                Some("Disconnect")
            );
            window.press("escape", cx);
            assert!(first.is_connected());
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("disconnect-connection", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            // Confirmation cannot override work that started after it opened.
            table.update(cx, |table, _| table.row_reads = 1);
            window.within("dialog").click("ok", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(window.has_active_dialog(cx));
            assert!(first.is_connected());
            table.update(cx, |table, _| table.row_reads = 0);
            window.within("dialog").click("ok", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(!first.is_connected());
            assert!(table.read(cx).request().is_none());
            assert!(editor.read(cx).request().is_none());
            assert_eq!(table.read(cx).draft().unwrap().query, draft.query);
            assert_eq!(editor.read(cx).draft(cx).sql, sql);
            assert!(window.find("sql-offline").visible());
            window.click("run-sql", cx);
            assert!(!editor.read(cx).is_busy());
            assert_ne!(window.find("export-sql-csv").disabled(), Some(true));
            app.update(cx, |view, cx| view.select_tab(2, window, cx));
            window.render_frame(cx);
            window.click("refresh-structure", cx);
            assert!(!structure.read(cx).is_busy());
            window.click("reconnect-connection", cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| {
            table.read(cx).table.is_some()
                && editor.read(cx).request().is_some()
                && !structure.read(cx).is_busy()
                && structure.read(cx).request().session.is_connected()
        })
        .await;
        let second = cx.update(|cx| {
            let second = editor.read(cx).request().unwrap().session.clone();
            assert!(!std::sync::Arc::ptr_eq(&first, &second));
            assert!(std::sync::Arc::ptr_eq(
                &second,
                &structure.read(cx).request().session
            ));
            assert_eq!(table.read(cx).query, draft.query);
            assert_eq!(editor.read(cx).draft(cx).sql, sql);
            second
        });
        assert_eq!(
            async_std::task::block_on(
                second.run_sql("lifecycle_ui_test", "SELECT @lifecycle_runs")
            )
            .unwrap()
            .0
            .rows[0][0],
            None
        );
        // Editing credentials retains a separately controllable previous session.
        cx.update(|cx| {
            let mut settings = cx.global::<Settings>().clone();
            settings.connections[0].name = "Renamed".into();
            cx.set_global(settings);
        });
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("connection-1", cx);
            assert_eq!(
                app.read(cx).connections.read(cx).selected_connection(cx),
                None
            );
            window.click("reconnect-connection", cx);
            assert!(!app.read(cx).connections.read(cx).is_connecting());
            assert!(second.is_connected());
            assert_ne!(window.find("disconnect-connection").disabled(), Some(true));
            window.click("connection-2", cx);
            window.click("reconnect-connection", cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| {
            !app.read(cx).connections.read(cx).is_connecting()
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("connection-1", cx);
            window.click("disconnect-connection", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.within("dialog").click("ok", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(window.try_find("connection-1").is_none());
            assert!(editor.read(cx).request().is_none());
            // Explicitly connecting another session never immediately undoes Disconnect.
            assert!(table.read(cx).request().is_none());
            window.click("connection-2", cx);
            window.click("refresh-connection-metadata", cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| {
            editor.read(cx).request().is_some() && table.read(cx).table.is_some()
        })
        .await;
        cx.update(|cx| {
            assert_eq!(editor.read(cx).connection_name(), "Renamed");
        });
        let third = cx.update(|cx| editor.read(cx).request().unwrap().session.clone());
        let (rows, _) =
            async_std::task::block_on(third.run_sql("mysql", "SELECT CONNECTION_ID()")).unwrap();
        async_std::task::block_on(admin.run_sql(
            "mysql",
            &format!("KILL CONNECTION {}", rows.rows[0][0].as_ref().unwrap()),
        ))
        .unwrap();
        cx.background_executor
            .advance_clock(std::time::Duration::from_secs(30));
        cx.wait_for(handle.into(), timeout, |_, cx| {
            editor.read(cx).request().is_none()
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("connection-2-connected").is_none());
            assert!(
                window
                    .find("connection-status")
                    .label()
                    .unwrap()
                    .contains("Connection lost")
            );
            window.click("refresh-connection-metadata", cx);
            assert!(!app.read(cx).connections.read(cx).is_connecting());
            window.double_click("connection-2", cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| {
            editor.read(cx).request().is_some() && table.read(cx).table.is_some()
        })
        .await;
        assert!(cx.update(|cx| editor.read(cx).request().unwrap().session.is_connected()));
        async_std::task::block_on(admin.run_sql("mysql", "DROP DATABASE lifecycle_ui_test"))
            .unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[gpui_kit::test]
    #[ignore = "requires an isolated MariaDB server and TABLELANE_TEST_MARIADB_PORT"]
    async fn mariadb_table_workspace_reattaches_filters_sort_page_and_recovers_stale_columns(
        cx: &mut TestAppContext,
    ) {
        use crate::{
            database::{FilterOperator, TableFilter, TableQuery},
            settings::{Connection, DatabaseKind, Settings},
            workspace_state::{SavedTab, TableDraft, WorkspaceFile, WorkspaceState},
        };
        use gpui_kit::test::TestAppContextExt;
        use sqlx::Connection as _;
        let port = std::env::var("TABLELANE_TEST_MARIADB_PORT")
            .unwrap()
            .parse()
            .unwrap();
        let config = Connection {
            name: "Renamed".into(),
            database_type: DatabaseKind::MariaDB,
            host: "127.0.0.1".into(),
            port: Some(port),
            username: "root".into(),
            password: String::new(),
            database: String::new(),
            file_path: String::new(),
        };
        let mut database = async_std::task::block_on(async {
            let mut database =
                sqlx::MySqlConnection::connect(&format!("mysql://root@127.0.0.1:{port}"))
                    .await
                    .unwrap();
            sqlx::query("CREATE DATABASE recovery_test")
                .execute(&mut database)
                .await
                .unwrap();
            sqlx::query("CREATE TABLE recovery_test.widgets (id INT PRIMARY KEY, name TEXT)")
                .execute(&mut database)
                .await
                .unwrap();
            let values = (1..=1005)
                .map(|id| format!("({id}, 'row {id}')"))
                .collect::<Vec<_>>()
                .join(",");
            sqlx::query(&format!(
                "INSERT INTO recovery_test.widgets VALUES {values}"
            ))
            .execute(&mut database)
            .await
            .unwrap();
            database
        });
        let directory = std::env::temp_dir().join(format!(
            "tablelane-table-recovery-live-{}",
            std::process::id()
        ));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("workspace.json");
        let draft = TableDraft {
            connection_key: config.key(),
            connection_name: "Old name".into(),
            database: "recovery_test".into(),
            table: "widgets".into(),
            query: TableQuery {
                page: 1,
                sort: Some(("id".into(), true)),
                filters: vec![
                    TableFilter {
                        column: None,
                        operator: FilterOperator::StartsWith,
                        value: "row".into(),
                    },
                    TableFilter {
                        column: Some("id".into()),
                        operator: FilterOperator::GreaterThan,
                        value: "1".into(),
                    },
                ],
            },
        };
        let mut stale = draft.clone();
        stale.query.filters[1].column = Some("removed_column".into());
        let mut unavailable = draft.clone();
        unavailable.table = "missing_table".into();
        let mut wrong = draft.clone();
        let mut other_config = config.clone();
        other_config.username = "other-user".into();
        wrong.connection_key = other_config.key();
        let mut state = WorkspaceState::default();
        state.tabs = vec![
            SavedTab::Table(draft.clone()),
            SavedTab::Table(stale.clone()),
            SavedTab::Table(unavailable),
            SavedTab::Table(wrong),
        ];
        assert!(WorkspaceFile::load(Some(path.clone())).0.save(&state));
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(Settings {
                connections: vec![config],
                ..Settings::default()
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
        let tables = cx.update(|cx| {
            app.read(cx)
                .tabs
                .iter()
                .map(|tab| {
                    let crate::WorkspaceTab::Table(table) = tab else {
                        panic!("Table missing")
                    };
                    assert!(table.read(cx).request().is_none());
                    table.clone()
                })
                .collect::<Vec<_>>()
        });
        let timeout = std::time::Duration::from_secs(15);
        cx.update_window(handle.into(), |_, window, cx| {
            window.double_click("connection-1", cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| {
            tables[0].read(cx).table.is_some() && tables[1].read(cx).error.is_some()
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            let table = tables[0].read(cx);
            assert_eq!(table.query, draft.query);
            assert_eq!(table.title(), "Renamed / recovery_test / widgets");
            let rows = &table.table.as_ref().unwrap().read(cx).delegate().data.rows;
            assert_eq!(rows.len(), 4);
            assert_eq!(rows[0][0].as_deref(), Some("5"));
            assert_eq!(rows[3][0].as_deref(), Some("2"));
            assert_eq!(window.find("filter-value").value(), Some("row"));
            assert_eq!(window.find("filter-operator").value(), Some("Starts with"));
            assert_eq!(
                window.find("page-status").label(),
                Some("Page 2 · Rows 1001–1004")
            );
            assert!(tables[2].read(cx).request().is_none());
            assert!(tables[3].read(cx).request().is_none());
            let request = table.request().unwrap().clone();
            app.update(cx, |app, cx| app.open_table(&request, window, cx));
            assert_eq!(app.read(cx).tabs.len(), 4);
            assert_eq!(app.read(cx).active_tab, 0);
            window.click("previous-page", cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| {
            !tables[0].read(cx).loading && tables[0].read(cx).query.page == 0
        })
        .await;
        cx.run_until_parked();
        cx.background_executor
            .advance_clock(std::time::Duration::from_millis(300));
        cx.run_until_parked();
        let SavedTab::Table(saved) = &WorkspaceFile::load(Some(path.clone())).1.tabs[0] else {
            panic!("Saved table missing")
        };
        assert_eq!(saved.query.page, 0);
        assert_eq!(saved.query.filters, draft.query.filters);
        cx.update_window(handle.into(), |_, window, cx| {
            window.within("workspace-tabs").click(1_usize, cx);
            window.render_frame(cx);
            assert_eq!(tables[1].read(cx).query, stale.query);
            assert!(
                window
                    .find("table-status")
                    .label()
                    .unwrap()
                    .contains("Filter column is unavailable")
            );
            window.click("reset-table-query", cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| {
            tables[1].read(cx).table.is_some() && !tables[1].read(cx).loading
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(tables[1].read(cx).query, TableQuery::default());
            assert_eq!(
                tables[1]
                    .read(cx)
                    .table
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .delegate()
                    .data
                    .rows
                    .len(),
                1000
            );
            // Successful filter changes autosave the applied query.
            window.within("workspace-tabs").click(0_usize, cx);
            window.click("filter-value", cx);
            window.press("secondary-a", cx);
            window.input("missing row", cx);
            window.click("apply-filter", cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| {
            !tables[0].read(cx).loading
                && tables[0].read(cx).query.filters[1].value == "missing row"
        })
        .await;
        cx.run_until_parked();
        cx.background_executor
            .advance_clock(std::time::Duration::from_millis(300));
        cx.run_until_parked();
        cx.update(|cx| {
            let saved = WorkspaceFile::load(Some(path.clone())).1;
            assert_eq!(saved, app.read(cx).capture_workspace(cx));
            assert_eq!(tables[0].read(cx).query.page, 0);
            assert!(
                tables[0]
                    .read(cx)
                    .table
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .delegate()
                    .data
                    .rows
                    .is_empty()
            );
        });
        async_std::task::block_on(
            sqlx::query("DROP DATABASE recovery_test").execute(&mut database),
        )
        .unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[gpui_kit::test]
    fn filter_controls_restore_conditions_and_debounce_live_search(cx: &mut TestAppContext) {
        use crate::database::{FilterOperator, TableFilter};
        cx.update(gpui_kit::init);
        let mut view = None;
        let handle = cx.open_window(size(px(600.), px(500.)), |window, cx| {
            let entity = cx.new(|_| TableView::default());
            entity.update(cx, |view, cx| {
                view.finish(
                    Ok(TableRows {
                        schema: TableSchema {
                            columns: vec!["id".into()],
                            column_types: vec!["int".into()],
                            ..TableSchema::default()
                        },
                        rows: vec![],
                        keys: vec![],
                        has_more: false,
                    }),
                    window,
                    cx,
                )
            });
            view = Some(entity.clone());
            Root::new(entity, window, cx)
        });
        let view = view.unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("filter-column").value(), Some("All columns"));
            assert_eq!(window.find("filter-operator").value(), Some("Contains"));
            let column = window.find("filter-column").bounds();
            for id in [
                "filter-operator",
                "filter-value",
                "apply-filter",
                "clear-filters",
            ] {
                let control = window.find(id).bounds();
                assert_eq!(control.top(), column.top());
                assert_eq!(control.bottom(), column.bottom());
                assert!(control.right() <= window.find("filter-bar").bounds().right());
            }
            assert!(window.find("filter-value").bounds().size.width >= window.rem_size() * 4.);
            window.within("filter-operator").click("input", cx);
            window.press("down", cx);
            window.press("enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(
                window.find("filter-operator").value(),
                Some("Does not contain")
            );
            view.update(cx, |view, _| {
                view.query.filters = vec![
                    TableFilter {
                        column: None,
                        operator: FilterOperator::NotContains,
                        value: "hidden".into(),
                    },
                    TableFilter {
                        column: Some("id".into()),
                        operator: FilterOperator::GreaterThan,
                        value: "10".into(),
                    },
                ]
            });
            window.within("filter-column").click("input", cx);
            window.input("id", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.press("enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("filter-column").value(), Some("id"));
            assert_eq!(window.find("filter-operator").value(), Some("Greater than"));
            assert_eq!(window.find("filter-value").value(), Some("10"));
            window.within("filter-column").click("input", cx);
            window.press("up", cx);
            window.press("enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("filter-column").value(), Some("All columns"));
            assert_eq!(
                window.find("filter-operator").value(),
                Some("Does not contain")
            );
            assert_eq!(window.find("filter-value").value(), Some("hidden"));
            view.update(cx, |view, cx| {
                view.loading = true;
                cx.notify();
            });
            window.render_frame(cx);
            window.within("filter-operator").click("input", cx);
            assert_eq!(window.find("filter-operator").expanded(), Some(false));
            window.click("filter-value", cx);
            window.press("secondary-a", cx);
            window.input("n", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.background_executor
            .advance_clock(std::time::Duration::from_millis(299));
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(!view.read(cx).filter_pending);
            window.input("ew", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.background_executor
            .advance_clock(std::time::Duration::from_millis(299));
        cx.run_until_parked();
        cx.update(|cx| assert!(!view.read(cx).filter_pending));
        cx.background_executor
            .advance_clock(std::time::Duration::from_millis(1));
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(view.read(cx).filter_pending);
            assert_eq!(window.find("filter-value").value(), Some("new"));
            window.input("est", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(!view.read(cx).filter_pending);
            window.press("enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(view.read(cx).filter_pending);
            assert!(view.read(cx).filter_task.is_none());
            assert_eq!(window.find("filter-value").value(), Some("newest"));
            view.update(cx, |view, cx| {
                view.loading = false;
                cx.notify();
            });
            window.click("clear-filters", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update(|cx| {
            assert!(!view.read(cx).filter_pending);
            assert!(view.read(cx).filter_task.is_none());
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("filter-value", cx);
            window.input("blocked", cx);
            view.update(cx, |view, cx| {
                view.set_fields(
                    &TableSchema {
                        columns: vec!["id".into()],
                        column_types: vec!["int".into()],
                        ..TableSchema::default()
                    },
                    vec![Some("1".into())],
                    window,
                    cx,
                );
                view.fields[0]
                    .input
                    .update(cx, |input, cx| input.set_value("2", window, cx));
                view.loading = true;
            });
        })
        .unwrap();
        cx.run_until_parked();
        cx.background_executor
            .advance_clock(std::time::Duration::from_millis(300));
        cx.run_until_parked();
        cx.update(|cx| {
            assert!(view.read(cx).is_dirty(cx));
            assert!(!view.read(cx).filter_pending);
            assert!(view.read(cx).filter_task.is_none());
        });
    }

    #[gpui_kit::test]
    #[ignore = "requires a temporary MariaDB server and TABLELANE_TEST_MARIADB_PORT"]
    async fn mariadb_sidebar_cmd_s_saves_and_escape_cancels(cx: &mut TestAppContext) {
        use crate::{
            connections::OpenTable,
            settings::{Connection, DatabaseKind, Settings},
        };
        use gpui_kit::test::TestAppContextExt;
        use sqlx::Connection as _;
        let port = std::env::var("TABLELANE_TEST_MARIADB_PORT")
            .unwrap()
            .parse()
            .unwrap();
        let initial_notes = format!("{}\r\nSecond line\nThird line", "x".repeat(400));
        let (session, mut database) = async_std::task::block_on(async {
            let mut database =
                sqlx::MySqlConnection::connect(&format!("mysql://root@127.0.0.1:{port}"))
                    .await
                    .unwrap();
            sqlx::query("CREATE DATABASE sidebar_edit_test")
                .execute(&mut database)
                .await
                .unwrap();
            sqlx::query("CREATE TABLE sidebar_edit_test.widgets (id INT PRIMARY KEY, name VARCHAR(20), notes LONGTEXT, nullable TEXT) ENGINE=InnoDB").execute(&mut database).await.unwrap();
            sqlx::query("INSERT INTO sidebar_edit_test.widgets VALUES (1, 'Original', ?, NULL), (2, 'Other', '', NULL)").bind(&initial_notes).execute(&mut database).await.unwrap();
            sqlx::query("INSERT INTO sidebar_edit_test.widgets SELECT seq, 'Other', '', NULL FROM sidebar_edit_test.seq_3_to_1002").execute(&mut database).await.unwrap();
            let session = crate::database::connect(&Connection {
                name: "Test".into(),
                database_type: DatabaseKind::MariaDB,
                host: "127.0.0.1".into(),
                port: Some(port),
                username: "root".into(),
                password: String::new(),
                database: String::new(),
                file_path: String::new(),
            })
            .await
            .unwrap();
            (std::sync::Arc::new(session), database)
        });
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(Settings::default());
        });
        let mut table_view = None;
        let handle = cx.open_window(size(px(1000.), px(700.)), |window, cx| {
            let table = cx.new(|cx| {
                TableView::new(
                    OpenTable {
                        session,
                        connection_name: "Test".into(),
                        database: "sidebar_edit_test".into(),
                        table: "widgets".into(),
                    },
                    window,
                    cx,
                )
            });
            table_view = Some(table.clone());
            let app = cx.new(|cx| crate::AppView::new(None, None, None, window, cx));
            app.update(cx, |app, cx| app.set_table(table, window, cx));
            Root::new(app, window, cx)
        });
        let view = table_view.unwrap();
        let timeout = std::time::Duration::from_secs(15);
        cx.wait_for(handle.into(), timeout, |window, _| {
            window.try_find("table").is_some()
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.click_at(
                ("row", 0_usize),
                point(window.rem_size(), Size::Small.table_row_height() / 2.),
                cx,
            );
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |window, cx| {
            window
                .try_find(field_id(&view, "notes", cx))
                .is_some_and(|field| field.value() == Some(initial_notes.as_str()))
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.click(field_id(&view, "name", cx), cx);
            window.press("secondary-a", cx);
            window.input("Updated", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            assert_eq!(window.find("row-edit-status").label(), Some("Edited"));
            window.click("next-page", cx);
            window.click("sort-column-id", cx);
            assert_eq!(view.read(cx).query.page, 0);
            assert!(view.read(cx).query.sort.is_none());
            assert!(view.read(cx).is_dirty(cx));
            assert_eq!(
                view.read(cx)
                    .table
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .delegate()
                    .data
                    .rows[0][1],
                Some("Original".into())
            );
            window.click_at(
                ("row", 1_usize),
                point(window.rem_size(), Size::Small.table_row_height() / 2.),
                cx,
            );
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            assert_eq!(
                window.find(field_id(&view, "name", cx)).value(),
                Some("Updated")
            );
            assert_eq!(view.read(cx).selected_row, Some(0));
            window.click(field_id(&view, "name", cx), cx);
            window.press("cmd-s", cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |window, cx| {
            window.try_find("row-edit-status").is_none()
                && window.find(field_id(&view, "name", cx)).value() == Some("Updated")
        })
        .await;
        let (name, notes, nullable): (String, String, Option<String>) = async_std::task::block_on(
            sqlx::query_as(
                "SELECT name, notes, nullable FROM sidebar_edit_test.widgets WHERE id = 1",
            )
            .fetch_one(&mut database),
        )
        .unwrap();
        assert_eq!(name, "Updated");
        assert_eq!(notes, initial_notes);
        assert_eq!(nullable, None);
        cx.update_window(handle.into(), |_, window, cx| {
            assert_eq!(
                window.find(field_id(&view, "name", cx)).focused(),
                Some(true)
            );
            assert_eq!(
                view.read(cx)
                    .table
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .delegate()
                    .data
                    .rows[0][1],
                Some("Updated".into())
            );
            window.click("row-null-nullable", cx);
            window.click(field_id(&view, "nullable", cx), cx);
            window.input("Discard", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.press("escape", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            assert_eq!(
                window.find(field_id(&view, "nullable", cx)).value(),
                Some("")
            );
            assert!(view.read(cx).fields[3].is_null);
            assert!(window.try_find("row-edit-status").is_none());
            window.click("row-null-nullable", cx);
            window.click("save-row", cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| !view.read(cx).saving)
            .await;
        let nullable: Option<String> = async_std::task::block_on(
            sqlx::query_scalar("SELECT nullable FROM sidebar_edit_test.widgets WHERE id = 1")
                .fetch_one(&mut database),
        )
        .unwrap();
        assert_eq!(nullable, Some(String::new()));
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("row-null-nullable", cx);
            window.click("save-row", cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| !view.read(cx).saving)
            .await;
        let nullable: Option<String> = async_std::task::block_on(
            sqlx::query_scalar("SELECT nullable FROM sidebar_edit_test.widgets WHERE id = 1")
                .fetch_one(&mut database),
        )
        .unwrap();
        assert_eq!(nullable, None);
        cx.update_window(handle.into(), |_, window, cx| {
            window.click(field_id(&view, "id", cx), cx);
            window.press("secondary-a", cx);
            window.input("invalid", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| window.press("cmd-s", cx))
            .unwrap();
        cx.wait_for(handle.into(), timeout, |window, _| {
            window.try_find("row-edit-error").is_some()
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            assert_eq!(
                window.find(field_id(&view, "id", cx)).value(),
                Some("invalid")
            );
            assert_eq!(window.find("row-edit-status").label(), Some("Edited"));
            window.press("escape", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            assert_eq!(window.find(field_id(&view, "id", cx)).value(), Some("1"));
            assert!(window.try_find("row-edit-error").is_none());
            window.click("next-page", cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| {
            !view.read(cx).loading && view.read(cx).query.page == 1
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            assert_eq!(
                window.find("page-status").label(),
                Some("Page 2 · Rows 1001–1002")
            );
            assert!(
                window.find("filter-value").bounds().bottom()
                    <= window.find("table").bounds().top()
            );
            assert!(
                window.find("apply-filter").bounds().bottom()
                    <= window.find("table").bounds().top()
            );
            assert!(window.find("filter-value").bounds().size.width >= window.rem_size() * 4.);
            assert_eq!(
                view.read(cx)
                    .table
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .delegate()
                    .data
                    .rows[0][0]
                    .as_deref(),
                Some("1001")
            );
            window.click("filter-value", cx);
            window.input("1002", cx);
            assert_eq!(window.find("filter-value").value(), Some("1002"));
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| {
            !view.read(cx).loading && !view.read(cx).query.filters.is_empty()
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            assert_eq!(
                window.find("page-status").label(),
                Some("Page 1 · Rows 1–1")
            );
            assert_eq!(
                view.read(cx)
                    .table
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .delegate()
                    .data
                    .rows[0][0]
                    .as_deref(),
                Some("1002")
            );
            window.scroll(
                "filter-bar",
                ScrollDelta::Pixels(point(px(-600.), px(0.))),
                cx,
            );
            window.click("clear-filters", cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| {
            !view.read(cx).loading && view.read(cx).query.filters.is_empty()
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("sort-column-id", cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| {
            !view.read(cx).loading && view.read(cx).query.sort == Some(("id".into(), false))
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            window.click("sort-column-id", cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| {
            !view.read(cx).loading && view.read(cx).query.sort == Some(("id".into(), true))
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            assert_eq!(
                view.read(cx)
                    .table
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .delegate()
                    .data
                    .rows[0][0]
                    .as_deref(),
                Some("1002")
            );
            window.click("next-page", cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| {
            !view.read(cx).loading && view.read(cx).query.page == 1
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            assert_eq!(
                view.read(cx)
                    .table
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .delegate()
                    .data
                    .rows[0][0]
                    .as_deref(),
                Some("2")
            );
            window.click("previous-page", cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| {
            !view.read(cx).loading && view.read(cx).query.page == 0
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("sort-column-id", cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| {
            !view.read(cx).loading && view.read(cx).query.sort.is_none()
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("filter-value", cx);
            window.press("secondary-a", cx);
            window.input("unmatched", cx);
            window.scroll(
                "filter-bar",
                ScrollDelta::Pixels(point(px(-600.), px(0.))),
                cx,
            );
            window.click("apply-filter", cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| {
            !view.read(cx).loading && !view.read(cx).query.filters.is_empty()
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            assert_eq!(window.find("page-status").label(), Some("Page 1 · No rows"));
        })
        .unwrap();
        async_std::task::block_on(
            sqlx::query("RENAME TABLE sidebar_edit_test.widgets TO sidebar_edit_test.unavailable")
                .execute(&mut database),
        )
        .unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("refresh-table", cx)
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| {
            !view.read(cx).loading && view.read(cx).error.is_some()
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window
                    .find("table-status")
                    .label()
                    .unwrap()
                    .contains("Couldn’t load table")
            );
            assert!(view.read(cx).table.is_some());
            assert_eq!(
                view.read(cx).query.filters,
                [crate::database::TableFilter {
                    column: None,
                    operator: crate::database::FilterOperator::Contains,
                    value: "unmatched".into()
                }]
            );
        })
        .unwrap();
        async_std::task::block_on(
            sqlx::query("RENAME TABLE sidebar_edit_test.unavailable TO sidebar_edit_test.widgets")
                .execute(&mut database),
        )
        .unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("refresh-table", cx)
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| {
            !view.read(cx).loading && view.read(cx).error.is_none()
        })
        .await;
        async_std::task::block_on(
            sqlx::query("DROP DATABASE sidebar_edit_test").execute(&mut database),
        )
        .unwrap();
    }

    #[gpui_kit::test]
    fn tabs_preserve_row_edits_and_protect_dirty_tabs(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let mut app = None;
        let mut first = None;
        let mut second = None;
        let handle = cx.open_window(size(px(1100.), px(650.)), |window, cx| {
            let view = cx.new(|cx| crate::AppView::new(None, None, None, window, cx));
            for title in ["widgets", "orders"] {
                let table = cx.new(|_| TableView {
                    title: title.into(),
                    location: "Local / test".into(),
                    ..TableView::default()
                });
                table.update(cx, |table, cx| {
                    table.finish(
                        Ok(TableRows {
                            schema: TableSchema {
                                columns: vec!["name".into()],
                                column_types: vec!["text".into()],
                                ..TableSchema::default()
                            },
                            rows: vec![vec![Some("Original".into())]],
                            keys: vec![vec![]],
                            has_more: false,
                        }),
                        window,
                        cx,
                    )
                });
                view.update(cx, |view, cx| view.set_table(table.clone(), window, cx));
                if first.is_none() {
                    first = Some(table);
                } else {
                    second = Some(table);
                }
            }
            app = Some(view.clone());
            Root::new(view, window, cx)
        });
        let app = app.unwrap();
        let first = first.unwrap();
        let second = second.unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(
                window.find("table-title").label(),
                Some("Local / test / orders")
            );
            window.within("workspace-tabs").click(0_usize, cx);
            window.click_at(
                ("row", 0_usize),
                point(window.rem_size(), Size::Small.table_row_height() / 2.),
                cx,
            );
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click(field_id(&first, "name", cx), cx);
            window.press("secondary-a", cx);
            window.input("Draft value", cx);
            assert!(!first.read(cx).can_change_schema(cx));
            assert_eq!(
                window.within("workspace-tabs").find(0_usize).label(),
                Some("Local / test / widgets · Edited")
            );
            window.press("ctrl-tab", cx);
            assert_eq!(
                window.find("table-title").label(),
                Some("Local / test / orders")
            );
            window.press("ctrl-tab", cx);
            assert_eq!(
                window.find(field_id(&first, "name", cx)).value(),
                Some("Draft value")
            );
            window.press("ctrl-shift-tab", cx);
            assert_eq!(app.read(cx).active_tab, 1);
            window.click(("close-tab", first.entity_id()), cx);
            assert_eq!(app.read(cx).tabs.len(), 2);
            assert_eq!(app.read(cx).active_tab, 0);
            assert!(window.find("row-edit-error").visible());
            window.click("cancel-row", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(!first.read(cx).is_dirty(cx));
            window.press("cmd-w", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(app.read(cx).tabs.len(), 1);
            assert_eq!(app.read(cx).table(), Some(&second));
            window.press("cmd-w", cx);
            assert!(app.read(cx).tabs.is_empty());
            assert!(window.try_find("table-title").is_none());
            window.press("ctrl-tab", cx);
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn selected_row_opens_sidebar_with_vertical_editable_fields(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(crate::settings::Settings::default());
        });
        let mut table_view = None;
        let handle = cx.open_window(size(px(1000.), px(600.)), |window, cx| {
            let table = cx.new(|_| TableView {
                title: "widgets".into(),
                location: "Local / test".into(),
                table: None,
                fields: Vec::new(),
                error: None,
                ..TableView::default()
            });
            table.update(cx, |view, cx| {
                view.finish(
                    Ok(TableRows {
                        has_more: false,
                        schema: TableSchema {
                            columns: vec!["id".into(), "name".into()],
                            column_types: vec!["int".into(), "text".into()],
                            ..TableSchema::default()
                        },
                        keys: vec![
                            vec![Some("1".into())],
                            vec![Some("2".into())],
                            vec![Some("3".into())],
                        ],
                        rows: vec![
                            vec![Some("1".into()), None],
                            vec![Some("2".into()), Some(String::new())],
                            vec![Some("3".into()), Some("NULL".into())],
                        ],
                    }),
                    window,
                    cx,
                )
            });
            table_view = Some(table.clone());
            let app = cx.new(|cx| crate::AppView::new(None, None, None, window, cx));
            app.update(cx, |app, cx| app.set_table(table, window, cx));
            Root::new(app, window, cx)
        });
        let view = table_view.unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            assert!(window.try_find("right-sidebar").is_none());
            window.click_at(
                ("row", 0_usize),
                point(window.rem_size(), Size::Small.table_row_height() / 2.),
                cx,
            );
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("right-sidebar").visible());
            assert_eq!(window.find(field_id(&view, "id", cx)).value(), Some("1"));
            assert_eq!(
                window.find(field_id(&view, "name", cx)).label(),
                Some("name")
            );
            let fields = &view.read(cx).fields;
            assert_eq!(
                fields[1]
                    .input
                    .read(cx)
                    .presentation()
                    .placeholder()
                    .as_ref(),
                "NULL"
            );
            let id = window.find(field_id(&view, "id", cx)).bounds();
            let name = window.find(field_id(&view, "name", cx)).bounds();
            assert_eq!(id.left(), name.left());
            assert_eq!(id.right(), name.right());
            assert!(name.top() > id.bottom());
            window.press("down", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find(field_id(&view, "id", cx)).value(), Some("2"));
            assert_eq!(window.find(field_id(&view, "name", cx)).value(), Some(""));
            assert_eq!(
                view.read(cx).fields[1]
                    .input
                    .read(cx)
                    .presentation()
                    .placeholder()
                    .as_ref(),
                ""
            );
            window.click("row-null-name", cx);
            assert_eq!(view.read(cx).fields[1].value(cx), None);
            assert!(view.read(cx).is_dirty(cx));
            window.click(field_id(&view, "name", cx), cx);
            window.input("blocked", cx);
            assert_eq!(view.read(cx).fields[1].input.read(cx).value().as_ref(), "");
            window.click("row-null-name", cx);
            assert_eq!(view.read(cx).fields[1].value(cx), Some(String::new()));
            assert!(!view.read(cx).is_dirty(cx));
            window.click("toggle-right-sidebar", cx);
            assert!(window.try_find("right-sidebar").is_none());
            window.click_at(
                ("row", 2_usize),
                point(window.rem_size(), Size::Small.table_row_height() / 2.),
                cx,
            );
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find(field_id(&view, "id", cx)).value(), Some("3"));
            assert_eq!(
                window.find(field_id(&view, "name", cx)).value(),
                Some("NULL")
            );
            window.click(field_id(&view, "name", cx), cx);
            window.press("secondary-a", cx);
            window.input("Changed", cx);
            assert_eq!(
                window.find(field_id(&view, "name", cx)).value(),
                Some("Changed")
            );
            window.press("escape", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(
                window.find(field_id(&view, "name", cx)).value(),
                Some("NULL")
            );
            assert!(window.try_find("row-edit-status").is_none());
            let table = view.read(cx).table.as_ref().unwrap().clone();
            table.focus_handle(cx).focus(window, cx);
            window.press("escape", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find(field_id(&view, "id", cx)).is_none());
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn preview_renders_rows_empty_and_error_states(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let mut view = None;
        let handle = cx.open_window(size(px(800.), px(500.)), |window, cx| {
            let entity = cx.new(|_| TableView {
                title: "widgets".into(),
                location: "Local / test".into(),
                table: None,
                fields: Vec::new(),
                error: None,
                ..TableView::default()
            });
            view = Some(entity.clone());
            Root::new(entity, window, cx)
        });
        let view = view.unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("table-status").label(), Some("Loading table…"));
            view.update(cx, |view, cx| {
                view.finish(
                    Ok(TableRows {
                        has_more: false,
                        schema: TableSchema {
                            columns: vec!["id".into(), "name".into()],
                            column_types: vec!["bigint unsigned".into(), "varchar(255)".into()],
                            ..TableSchema::default()
                        },
                        keys: vec![],
                        rows: (0..1000)
                            .map(|ix| vec![Some(ix.to_string()), None])
                            .collect(),
                    }),
                    window,
                    cx,
                )
            });
            window.render_frame(cx);
            window.render_frame(cx);
            assert!(window.try_find("table-status").is_none());
            assert!(window.find("table").visible());
            assert_eq!(
                window.find("table-title").label(),
                Some("Local / test / widgets")
            );
            assert_eq!(
                window.find("column-title-id").label(),
                Some("id · bigint unsigned")
            );
            assert_eq!(
                window.find("column-title-name").label(),
                Some("name · varchar(255)")
            );
            let name = window.find("column-name-id").bounds();
            let kind = window.find("column-type-id").bounds();
            let header = window.find(("col-header", 0_usize)).bounds();
            let grid = window.find("table").bounds();
            view.update(cx, |view, cx| {
                view.loading = true;
                cx.notify();
            });
            window.render_frame(cx);
            assert!(window.try_find("table-status").is_none());
            assert!(window.find("table").visible());
            assert_eq!(window.find("table").bounds(), grid);
            assert_eq!(window.find(("col-header", 0_usize)).bounds(), header);
            view.update(cx, |view, cx| {
                view.loading = false;
                cx.notify();
            });
            window.render_frame(cx);
            assert_eq!(grid.left(), window.rem_size() * 0.75);
            assert_eq!(grid.right(), px(800.));
            assert_eq!(header.left(), grid.left());
            assert!(header.size.height <= Size::Small.table_row_height());
            assert_eq!(name.left(), kind.left());
            assert!(kind.top() >= name.bottom());
            assert!(name.top() >= header.top());
            assert!(kind.bottom() <= header.bottom());
            assert!(window.find("table").bounds().bottom() <= px(500.));
            let table = view.read(cx).table.as_ref().unwrap().clone();
            assert!(table.read(cx).visible_range().rows().len() < 1000);
            table.focus_handle(cx).focus(window, cx);
            window.press("down", cx);
            assert!(table.read(cx).selected_row().is_some());
            window.scroll("table", ScrollDelta::Pixels(point(px(0.), px(-1000.))), cx);
            assert!(table.read(cx).visible_range().rows().start > 0);
            assert_eq!(window.find(("col-header", 0_usize)).bounds(), header);
            assert_eq!(window.find("column-name-id").bounds(), name);
            assert_eq!(window.find("column-type-id").bounds(), kind);
            let title = window.find("table-title").bounds();
            window.scroll("table", ScrollDelta::Pixels(point(px(0.), px(-30000.))), cx);
            assert!(table.read(cx).visible_range().rows().end >= 1000);
            assert_eq!(window.find(("col-header", 0_usize)).bounds(), header);
            assert_eq!(window.find("column-name-id").bounds(), name);
            assert_eq!(window.find("column-type-id").bounds(), kind);
            assert_eq!(window.find("table-title").bounds(), title);
            view.update(cx, |view, cx| {
                view.finish(
                    Ok(TableRows {
                        has_more: false,
                        schema: TableSchema {
                            columns: vec!["id".into()],
                            column_types: vec!["bigint unsigned".into()],
                            ..TableSchema::default()
                        },
                        keys: vec![],
                        rows: vec![],
                    }),
                    window,
                    cx,
                )
            });
            window.render_frame(cx);
            assert!(window.try_find("table-status").is_none());
            view.update(cx, |view, cx| {
                view.table = None;
                view.finish(Err("Couldn’t load table".into()), window, cx);
            });
            window.render_frame(cx);
            assert_eq!(
                window.find("table-status").label(),
                Some("Couldn’t load table")
            );
            assert!(window.try_find("table").is_none());
        })
        .unwrap();
    }
}
