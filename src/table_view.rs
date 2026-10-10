use crate::{
    connections::OpenTable,
    database::{PAGE_SIZE, TableQuery, TableRows, TableSchema, truncate_preview},
};
use gpui_kit::{
    component::{
        ActiveTheme, Disableable, Sizable,
        button::{Button, ButtonVariants},
        form::{Field, Form},
        input::{Input, InputEvent, InputState, Textarea, TextareaState},
        select::{Select, SelectEvent, SelectState},
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
            .disabled(view.loading || view.saving || view.is_dirty(cx))
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
    table: Option<Entity<TableState<Rows>>>,
    fields: Vec<RowField>,
    selected_row: Option<usize>,
    load_generation: u64,
    loading_row: bool,
    saving: bool,
    edit_error: Option<String>,
    error: Option<String>,
    query: TableQuery,
    loading: bool,
    filter_column: Option<Entity<SelectState<Vec<String>>>>,
    filter_value: Option<Entity<InputState>>,
}

impl EventEmitter<TableEvent> for TableView {}

impl TableView {
    pub(crate) fn new(request: OpenTable, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let title = request.table.clone();
        let location = format!("{} / {}", request.connection_name, request.database);
        let mut view = Self {
            title,
            location,
            request: Some(request),
            ..Self::default()
        };
        view.load(TableQuery::default(), window, cx);
        view
    }

    fn load(&mut self, query: TableQuery, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading || self.saving || self.is_dirty(cx) {
            return;
        }
        let Some(request) = self.request.clone() else {
            return;
        };
        self.loading = true;
        self.error = None;
        self.fields.clear();
        self.selected_row = None;
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
                    }
                    Err(error) => {
                        view.error = Some(error);
                        cx.notify();
                    }
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn apply_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(column), Some(value)) = (&self.filter_column, &self.filter_value) else {
            return;
        };
        let Some(name) = column.read(cx).selected_value().cloned() else {
            return;
        };
        let value = value.read(cx).value().to_string();
        let mut query = self.query.clone();
        query.page = 0;
        query.filters.retain(|(column, _)| column != &name);
        if !value.is_empty() {
            query.filters.push((name, value));
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
        self.loading_row = false;
        self.load_generation += 1;
        match result {
            Ok(data) => {
                if self.filter_column.is_none() {
                    let column = cx.new(|cx| {
                        SelectState::new(
                            data.schema.columns.clone(),
                            Some(component::IndexPath::new(0)),
                            window,
                            cx,
                        )
                    });
                    cx.subscribe_in(
                        &column,
                        window,
                        |view, _, event: &SelectEvent<Vec<String>>, window, cx| {
                            let SelectEvent::Confirm(name) = event;
                            let value = view
                                .query
                                .filters
                                .iter()
                                .find(|(column, _)| Some(column) == name.as_ref())
                                .map(|(_, value)| value.clone())
                                .unwrap_or_default();
                            if let Some(input) = &view.filter_value {
                                input.update(cx, |input, cx| input.set_value(value, window, cx));
                            }
                        },
                    )
                    .detach();
                    self.filter_column = Some(column);
                    let input = cx.new(|cx| {
                        InputState::new(window, cx)
                            .placeholder("Contains text; empty removes filter")
                    });
                    cx.subscribe_in(&input, window, |view, _, event: &InputEvent, window, cx| {
                        if matches!(event, InputEvent::PressEnter { .. }) {
                            view.apply_filter(window, cx);
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
                    if view.loading {
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
                            if view.saving {
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

    fn select_row(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
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
                    original,
                    editable: schema.is_editable(ix),
                }
            })
            .collect();
    }

    fn is_dirty(&self, cx: &App) -> bool {
        self.fields.iter().any(|field| field.is_edited(cx))
    }

    pub(crate) fn prepare_to_close(&mut self, cx: &mut Context<Self>) -> bool {
        if self.is_dirty(cx) || self.saving {
            self.edit_error =
                Some("Save or cancel your edits before opening another table.".into());
            cx.notify();
            false
        } else {
            true
        }
    }

    pub(crate) fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
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
        if self.saving || self.loading_row || !self.is_dirty(cx) {
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

    pub(crate) fn render_sidebar(&self, cx: &App) -> Div {
        let dirty = self.is_dirty(cx);
        div()
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
                            .child("Row details"),
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
            .child(
                Form::new()
                    .children(self.fields.iter().map(|field| {
                        Field::new()
                            .label(field.name.clone())
                            .when(!field.editable, |field| {
                                field.description("Cannot be edited")
                            })
                            .child(
                                Textarea::new(&field.input)
                                    .accessibility_id(SharedString::from(format!(
                                        "row-field-{}",
                                        field.name
                                    )))
                                    .aria_label(field.name.clone())
                                    .disabled(self.saving || !field.editable)
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
                                    .label("Save")
                                    .disabled(!dirty || self.saving)
                                    .on_click(|_, window, cx| {
                                        window.dispatch_action(Box::new(SaveRow), cx)
                                    }),
                            )
                            .child(
                                Button::new("cancel-row")
                                    .small()
                                    .outline()
                                    .label("Cancel")
                                    .disabled(!dirty || self.saving)
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
        let status = if let Some(error) = &self.error {
            Some(error.clone())
        } else if self.loading || self.table.is_none() {
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
                self.filter_column.as_ref().zip(self.filter_value.as_ref()),
                |view, (column, value)| {
                    let busy = self.loading || self.saving || self.is_dirty(cx);
                    view.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .px_3()
                            .py_2()
                            .flex_shrink_0()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(div().text_sm().child("Filter"))
                                    .child(
                                        div().w_40().flex_none().child(
                                            Select::new(column)
                                                .id("filter-column")
                                                .accessibility_label("Filter column")
                                                .small()
                                                .disabled(busy),
                                        ),
                                    )
                                    .child(
                                        Input::new(value)
                                            .id("filter-value")
                                            .aria_label("Contains text")
                                            .small()
                                            .flex_1()
                                            .min_w_0()
                                            .disabled(busy),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        Button::new("apply-filter")
                                            .small()
                                            .label("Apply")
                                            .disabled(busy)
                                            .on_click(cx.listener(|view, _, window, cx| {
                                                view.apply_filter(window, cx)
                                            })),
                                    )
                                    .child(
                                        Button::new("clear-filters")
                                            .small()
                                            .outline()
                                            .label("Clear filters")
                                            .disabled(busy || self.query.filters.is_empty())
                                            .on_click(cx.listener(|view, _, window, cx| {
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
                            ),
                    )
                    .when(!self.query.filters.is_empty(), |view| {
                        let summary = self
                            .query
                            .filters
                            .iter()
                            .map(|(column, value)| format!("{column} contains {value:?}"))
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
                        .when(self.loading, |status| status.flex_1())
                        .text_sm()
                        .text_color(if self.error.is_some() {
                            cx.theme().danger
                        } else {
                            cx.theme().muted_foreground
                        })
                        .child(status),
                )
            })
            .when_some(self.table.as_ref(), |view, table| {
                view.when(!self.loading, |view| {
                    view.child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .min_w_0()
                            .pl_3()
                            .child(DataTable::new(table).small().bordered(false).stripe(true)),
                    )
                })
                .child({
                    let data = &table.read(cx).delegate().data;
                    let busy = self.loading || self.saving || self.is_dirty(cx);
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
    }
}

#[cfg(test)]
mod tests {
    use super::TableView;
    use crate::database::{TableRows, TableSchema};
    use gpui_kit::{
        App, AppContext, ElementId, Entity, Focusable, ScrollDelta, SharedString, TestAppContext,
        component::{Root, Size},
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
            app.update(cx, |app, cx| app.set_table(table, cx));
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
            window.press("enter", cx);
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
                [("id".into(), "unmatched".into())]
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
            app.update(cx, |app, cx| app.set_table(table, cx));
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
