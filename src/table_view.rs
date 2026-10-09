use crate::{connections::OpenTable, database::TableRows};
use gpui_kit::{
    component::{
        ActiveTheme, Sizable,
        table::{Column, DataTable, TableDelegate, TableState},
    },
    prelude::FluentBuilder,
    *,
};

struct Rows {
    columns: Vec<Column>,
    rows: Vec<Vec<Option<String>>>,
}

impl TableDelegate for Rows {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }
    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }
    fn column(&self, ix: usize, _: &App) -> Column {
        self.columns[ix].clone()
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let value = &self.rows[row_ix][col_ix];
        div()
            .truncate()
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

pub(crate) struct TableView {
    title: String,
    table: Option<Entity<TableState<Rows>>>,
    error: Option<String>,
}

impl TableView {
    pub(crate) fn new(request: OpenTable, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let title = format!(
            "{} / {} / {}",
            request.connection_name, request.database, request.table
        );
        let task = cx.background_spawn(async move {
            async_std::task::block_on(
                request
                    .session
                    .read_table(&request.database, &request.table),
            )
        });
        cx.spawn_in(window, async move |view, cx| {
            let result = task.await;
            let _ = view.update_in(cx, |view, window, cx| view.finish(result, window, cx));
        })
        .detach();
        Self {
            title,
            table: None,
            error: None,
        }
    }

    fn finish(
        &mut self,
        result: Result<TableRows, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(rows) => {
                let delegate = Rows {
                    columns: rows
                        .columns
                        .into_iter()
                        .map(|name| {
                            Column::new(name.clone(), name)
                                .width(rems(12.).to_pixels(window.rem_size()))
                                .movable(false)
                        })
                        .collect(),
                    rows: rows.rows,
                };
                self.table = Some(cx.new(|cx| {
                    TableState::new(delegate, window, cx)
                        .col_movable(false)
                        .cell_selectable(true)
                }));
            }
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }
}

impl Render for TableView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let status = if let Some(error) = &self.error {
            error.clone()
        } else if let Some(table) = &self.table {
            format!(
                "{} rows shown · Limit 1,000 · Binary values shown as hex",
                table.read(cx).delegate().rows.len()
            )
        } else {
            "Loading table…".into()
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
                    .aria_label(self.title.clone())
                    .truncate()
                    .p_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(self.title.clone()),
            )
            .child(
                div()
                    .id("table-status")
                    .test_support()
                    .role(Role::Status)
                    .aria_label(status.clone())
                    .p_2()
                    .text_sm()
                    .text_color(if self.error.is_some() {
                        cx.theme().danger
                    } else {
                        cx.theme().muted_foreground
                    })
                    .child(status),
            )
            .when_some(self.table.as_ref(), |view, table| {
                view.child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .min_w_0()
                        .child(DataTable::new(table).small().bordered(false).stripe(true)),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::TableView;
    use crate::database::TableRows;
    use gpui_kit::{
        AppContext, Focusable, ScrollDelta, TestAppContext, component::Root, point, px, size,
        test::TestWindowExt,
    };

    #[gpui_kit::test]
    fn preview_renders_rows_empty_and_error_states(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let mut view = None;
        let handle = cx.open_window(size(px(800.), px(500.)), |window, cx| {
            let entity = cx.new(|_| TableView {
                title: "Local / test / widgets".into(),
                table: None,
                error: None,
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
                        columns: vec!["id".into(), "name".into()],
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
            assert!(
                window
                    .find("table-status")
                    .label()
                    .unwrap()
                    .starts_with("1000 rows shown")
            );
            assert!(window.find("table").visible());
            assert!(window.find("table").bounds().bottom() <= px(500.));
            let table = view.read(cx).table.as_ref().unwrap().clone();
            assert!(table.read(cx).visible_range().rows().len() < 1000);
            table.focus_handle(cx).focus(window, cx);
            window.press("down", cx);
            assert!(table.read(cx).selected_row().is_some());
            window.scroll("table", ScrollDelta::Pixels(point(px(0.), px(-1000.))), cx);
            assert!(table.read(cx).visible_range().rows().start > 0);
            view.update(cx, |view, cx| {
                view.finish(
                    Ok(TableRows {
                        columns: vec!["id".into()],
                        rows: vec![],
                    }),
                    window,
                    cx,
                )
            });
            window.render_frame(cx);
            assert!(
                window
                    .find("table-status")
                    .label()
                    .unwrap()
                    .starts_with("0 rows shown")
            );
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
