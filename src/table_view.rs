use crate::{connections::OpenTable, database::TableRows};
use gpui_kit::{
    component::{
        ActiveTheme, Sizable,
        table::{Column, DataTable, TableDelegate, TableState},
        tooltip::Tooltip,
    },
    prelude::FluentBuilder,
    *,
};

struct Rows {
    columns: Vec<Column>,
    column_types: Vec<String>,
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

    fn render_th(
        &mut self,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let column = &self.columns[col_ix];
        let kind = &self.column_types[col_ix];
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
        let value = &self.rows[row_ix][col_ix];
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

pub(crate) struct TableView {
    title: String,
    location: String,
    table: Option<Entity<TableState<Rows>>>,
    error: Option<String>,
}

impl TableView {
    pub(crate) fn new(request: OpenTable, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let title = request.table.clone();
        let location = format!("{} / {}", request.connection_name, request.database);
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
            location,
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
                                .p_0()
                                .movable(false)
                        })
                        .collect(),
                    rows: rows.rows,
                    column_types: rows.column_types,
                };
                self.table = Some(cx.new(|cx| {
                    TableState::new(delegate, window, cx)
                        .col_movable(false)
                        .cell_selectable(true)
                        .row_header(false)
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
    use crate::database::TableRows;
    use gpui_kit::{
        AppContext, Focusable, ScrollDelta, TestAppContext,
        component::{Root, Size},
        point, px, size,
        test::TestWindowExt,
    };

    #[gpui_kit::test]
    fn preview_renders_rows_empty_and_error_states(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let mut view = None;
        let handle = cx.open_window(size(px(800.), px(500.)), |window, cx| {
            let entity = cx.new(|_| TableView {
                title: "widgets".into(),
                location: "Local / test".into(),
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
                        column_types: vec!["bigint unsigned".into(), "varchar(255)".into()],
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
                        columns: vec!["id".into()],
                        column_types: vec!["bigint unsigned".into()],
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
