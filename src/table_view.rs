use crate::{
    connections::OpenTable,
    database::{TableRows, TableSchema, truncate_preview},
};
use gpui_kit::{
    component::{
        ActiveTheme, Disableable, Sizable,
        button::Button,
        form::{Field, Form},
        input::{InputEvent, Textarea, TextareaState},
        table::{Column, DataTable, TableDelegate, TableEvent, TableState},
        tooltip::Tooltip,
    },
    prelude::FluentBuilder,
    *,
};

struct Rows {
    columns: Vec<Column>,
    data: TableRows,
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
                    .child(column.name.clone()),
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
            .child("No rows in this table")
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
}

impl EventEmitter<TableEvent> for TableView {}

impl TableView {
    pub(crate) fn new(request: OpenTable, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let title = request.table.clone();
        let location = format!("{} / {}", request.connection_name, request.database);
        let load_request = request.clone();
        let task = cx.background_spawn(async move {
            async_std::task::block_on(
                load_request
                    .session
                    .read_table(&load_request.database, &load_request.table),
            )
        });
        cx.spawn_in(window, async move |view, cx| {
            let result = task.await;
            let _ = view.update_in(cx, |view, window, cx| view.finish(result, window, cx));
        })
        .detach();
        Self {
            title,
            location,
            request: Some(request),
            ..Self::default()
        }
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
                let table = cx.new(|cx| {
                    TableState::new(Rows { columns, data }, window, cx)
                        .col_movable(false)
                        .cell_selectable(true)
                        .row_header(false)
                });
                cx.subscribe_in(&table, window, |view, table, event, window, cx| {
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
                view.child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .min_w_0()
                        .pl_3()
                        .child(DataTable::new(table).small().bordered(false).stripe(true)),
                )
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
            assert_eq!(window.find("row-edit-status").label(), Some("Edited"));
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
            assert_eq!(window.find(field_id(&view, "id", cx)).value(), Some("1"));
            assert!(window.try_find("row-edit-error").is_none());
        })
        .unwrap();
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
