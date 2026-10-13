use super::*;
use crate::database::RowChange;
use gpui_kit::component::{WindowExt, button::ButtonVariant};

impl TableView {
    pub(super) fn new_row(&mut self, duplicate: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_busy() || self.is_dirty(cx) || self.filter_pending {
            return;
        }
        let (Some(request), Some(table)) = (&self.request, &self.table) else {
            return;
        };
        if !request.session.is_connected() {
            return;
        }
        let schema = table.read(cx).delegate().data.schema.clone();
        let values = if duplicate {
            if self.selected_row.is_none()
                || schema.primary_key.is_empty()
                || self.fields.len() != schema.columns.len()
            {
                return;
            }
            if (0..schema.columns.len())
                .any(|ix| !schema.is_editable(ix) && !schema.generated_columns.contains(&ix))
            {
                self.edit_error = Some("This row has unsupported columns. Use SQL to duplicate it without losing values.".into());
                cx.notify();
                return;
            }
            self.fields
                .iter()
                .map(|field| field.original.clone())
                .collect()
        } else {
            vec![None; schema.columns.len()]
        };
        self.load_generation += 1;
        self.selected_row = None;
        self.edit_error = None;
        self.set_fields(&schema, values, window, cx);
        for (ix, field) in self.fields.iter_mut().enumerate() {
            field.use_default =
                !duplicate || schema.auto_increment.contains(&ix) || !field.editable;
        }
        self.inserting = true;
        self.editor_focus
            .get_or_insert_with(|| cx.focus_handle())
            .focus(window, cx);
        cx.emit(RowEditorOpened);
        cx.notify();
    }

    pub(super) fn insert(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.inserting || self.is_busy() {
            return;
        }
        let (Some(request), Some(table)) = (self.request.clone(), &self.table) else {
            return;
        };
        if !request.session.is_connected() {
            return;
        }
        let schema = table.read(cx).delegate().data.schema.clone();
        let values = self
            .fields
            .iter()
            .map(|field| (!field.use_default).then(|| field.value(cx)))
            .collect::<Vec<_>>();
        self.saving = true;
        self.edit_error = None;
        cx.notify();
        let task = cx.background_spawn(async move {
            async_std::task::block_on(request.session.change_row(
                &request.database,
                &request.table,
                &schema,
                RowChange::Insert(&values),
            ))
        });
        self.finish_row_change(task, window, cx);
    }

    pub(super) fn confirm_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx)
            || self.is_busy()
            || self.is_dirty(cx)
            || self.filter_pending
        {
            return;
        }
        let (Some(request), Some(table), Some(_)) =
            (self.request.clone(), &self.table, self.selected_row)
        else {
            return;
        };
        if !request.session.is_connected() {
            return;
        }
        let schema = table.read(cx).delegate().data.schema.clone();
        if schema.primary_key.is_empty() || self.fields.len() != schema.columns.len() {
            return;
        }
        let original = self
            .fields
            .iter()
            .map(|field| field.original.clone())
            .collect::<Vec<_>>();
        let identity = schema
            .primary_key
            .iter()
            .map(|ix| {
                format!(
                    "{} = {:?}",
                    schema.columns[*ix],
                    original[*ix].as_deref().unwrap_or("NULL")
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let generation = self.load_generation;
        let owner = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let owner = owner.clone();
            let request = request.clone();
            let schema = schema.clone();
            let original = original.clone();
            dialog.confirm().title(format!("Delete row from {}?", request.table))
                .description(format!("{identity}\nThis permanently deletes this row and may cascade to related rows through foreign keys. The full original row is checked for concurrent changes."))
                .ok_text("Delete").ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    owner.update(cx, |view, cx| {
                        if view.is_busy() || view.is_dirty(cx) || view.filter_pending || view.load_generation != generation
                            || view.request.as_ref().is_none_or(|current| !std::sync::Arc::ptr_eq(&current.session, &request.session))
                            || view.fields.iter().map(|field| field.original.clone()).collect::<Vec<_>>() != original {
                            return false;
                        }
                        view.saving = true;
                        view.edit_error = None;
                        cx.notify();
                        let request = request.clone();
                        let schema = schema.clone();
                        let original = original.clone();
                        let task = cx.background_spawn(async move {
                            async_std::task::block_on(request.session.change_row(&request.database, &request.table, &schema, RowChange::Delete(&original)))
                        });
                        view.finish_row_change(task, window, cx);
                        true
                    }).unwrap_or(true)
                })
        });
    }

    fn finish_row_change(
        &mut self,
        task: Task<Result<(), String>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |view, cx| {
            let result = task.await;
            let _ = view.update_in(cx, |view, window, cx| {
                view.saving = false;
                match result {
                    Ok(()) => {
                        view.inserting = false;
                        view.fields.clear();
                        view.selected_row = None;
                        let mut query = view.query.clone();
                        query.page = 0;
                        view.load(query, window, cx);
                        view.focus(window, cx);
                    }
                    Err(error) => view.edit_error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::super::TableView;
    use crate::{
        connections::OpenTable,
        database::connect,
        settings::{Connection, DatabaseKind},
    };
    use gpui_kit::{
        App, AppContext, ElementId, Entity, TestAppContext,
        component::{Root, Size, WindowExt},
        point, px, size,
        test::{TestAppContextExt, TestWindowExt},
    };
    use std::{sync::Arc, time::Duration};

    fn field_id(table: &Entity<TableView>, name: &str, cx: &App) -> ElementId {
        let field = table
            .read(cx)
            .fields
            .iter()
            .find(|field| field.name.as_ref() == name)
            .unwrap();
        ("input", field.input.entity_id()).into()
    }

    #[gpui_kit::test]
    #[ignore = "requires an isolated MariaDB server and TABLELANE_TEST_MARIADB_PORT"]
    async fn mariadb_row_controls_insert_duplicate_confirm_delete_and_guard_drafts(
        cx: &mut TestAppContext,
    ) {
        let session = Arc::new(
            async_std::task::block_on(connect(&Connection {
                name: "Row UI".into(),
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
                .run_sql("mysql", "CREATE DATABASE row_controls_test")
                .await
                .unwrap();
            session.run_sql("row_controls_test", "CREATE TABLE widgets (id INT PRIMARY KEY AUTO_INCREMENT, body VARCHAR(4000) NOT NULL UNIQUE, bytes BLOB, computed INT AS (id + 1) VIRTUAL) ENGINE=InnoDB").await.unwrap();
        });
        cx.update(gpui_kit::init);
        let mut app = None;
        let mut table = None;
        let handle = cx.open_window(size(px(1250.), px(1100.)), |window, cx| {
            let view = cx.new(|cx| crate::AppView::new(None, None, None, window, cx));
            let data = cx.new(|cx| {
                TableView::new(
                    OpenTable {
                        session: session.clone(),
                        connection_name: "Row UI".into(),
                        database: "row_controls_test".into(),
                        table: "widgets".into(),
                    },
                    window,
                    cx,
                )
            });
            view.update(cx, |view, cx| view.set_table(data.clone(), window, cx));
            app = Some(view.clone());
            table = Some(data);
            Root::new(view, window, cx)
        });
        let app = app.unwrap();
        let table = table.unwrap();
        let timeout = Duration::from_secs(15);
        cx.wait_for(handle.into(), timeout, |_, cx| !table.read(cx).loading)
            .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("insert-row", cx)
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(app.read(cx).right_sidebar_visible);
            assert!(table.read(cx).is_dirty(cx));
            assert_eq!(window.find("row-default-id").checked(), Some(true));
            assert_eq!(window.find("row-default-computed").checked(), Some(true));
            assert!(!table.read(cx).can_change_schema(cx));
            assert!(!table.update(cx, |table, cx| table.prepare_to_close(cx)));
            window.click("row-default-body", cx);
            window.press("escape", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(!table.read(cx).is_dirty(cx));
            window.click("insert-row", cx);
        })
        .unwrap();
        cx.run_until_parked();
        let body = "世界".repeat(300);
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("row-default-body", cx);
            window.click("row-null-body", cx);
            let field = field_id(&table, "body", cx);
            window.click(field.clone(), cx);
            window.input(&body, cx);
            window.click("insert-row", cx);
            assert_eq!(field_id(&table, "body", cx), field);
            assert_eq!(
                table.read(cx).fields[1].value(cx).as_deref(),
                Some(body.as_str())
            );
            window.click("save-row", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.wait_for(handle.into(), timeout, |_, cx| !table.read(cx).is_busy())
            .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(!table.read(cx).inserting);
            let data = &table
                .read(cx)
                .table
                .as_ref()
                .unwrap()
                .read(cx)
                .delegate()
                .data;
            assert_eq!(data.rows.len(), 1);
            assert!(data.rows[0][1].as_ref().unwrap().ends_with('…'));
            window.click_at(
                ("row", 0_usize),
                point(window.rem_size(), Size::Small.table_row_height() / 2.),
                cx,
            );
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| {
            !table.read(cx).loading_row && !table.read(cx).fields.is_empty()
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("duplicate-row", cx);
            assert!(table.read(cx).inserting);
            assert_eq!(
                table.read(cx).fields[1].value(cx).as_deref(),
                Some(body.as_str())
            );
            assert_eq!(window.find("row-default-id").checked(), Some(true));
            assert_eq!(window.find("row-default-body").checked(), Some(false));
            window.click("save-row", cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| !table.read(cx).saving)
            .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("row-edit-error").visible());
            assert!(table.read(cx).inserting);
            let field = field_id(&table, "body", cx);
            window.click(field, cx);
            window.press("secondary-a", cx);
            window.input("copy", cx);
            window.press("cmd-s", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.wait_for(handle.into(), timeout, |_, cx| !table.read(cx).is_busy())
            .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(!table.read(cx).inserting);
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
                2
            );
            window.click_at(
                ("row", 0_usize),
                point(window.rem_size(), Size::Small.table_row_height() / 2.),
                cx,
            );
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| {
            !table.read(cx).loading_row && !table.read(cx).fields.is_empty()
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("delete-row", cx)
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(window.has_active_dialog(cx));
            assert_eq!(window.within("dialog").find("ok").label(), Some("Delete"));
            window.press("escape", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(!window.has_active_dialog(cx));
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
                2
            );
            window.click("delete-row", cx);
        })
        .unwrap();
        cx.run_until_parked();
        // Another session changes only a full-value suffix after confirmation opens.
        let other = async_std::task::block_on(connect(&Connection {
            name: "Concurrent UI".into(),
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
        .unwrap();
        async_std::task::block_on(other.run_sql(
            "row_controls_test",
            "UPDATE widgets SET body = CONCAT(body, 'changed') WHERE id = 1",
        ))
        .unwrap();
        cx.update_window(handle.into(), |_, window, cx| window.press("enter", cx))
            .unwrap();
        cx.run_until_parked();
        cx.wait_for(handle.into(), timeout, |_, cx| !table.read(cx).is_busy())
            .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            let dialog_open = window.has_active_dialog(cx);
            assert!(
                table
                    .read(cx)
                    .edit_error
                    .as_ref()
                    .is_some_and(|error| error.contains("row changed")),
                "error={:?} fields={} selected={:?} rows={} dialog={} busy={}",
                table.read(cx).edit_error,
                table.read(cx).fields.len(),
                table.read(cx).selected_row,
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
                dialog_open,
                table.read(cx).is_busy()
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
                2
            );
            // Reload the full row, then test confirmation-time busy guards.
            table.update(cx, |table, cx| table.select_row(0, window, cx));
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| {
            !table.read(cx).loading_row && !table.read(cx).fields.is_empty()
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("delete-row", cx)
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            table.update(cx, |table, _| table.exporting = true);
            window.press("enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(window.has_active_dialog(cx));
            assert!(!table.read(cx).saving);
            table.update(cx, |table, _| table.exporting = false);
            window.press("enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.wait_for(handle.into(), timeout, |_, cx| !table.read(cx).is_busy())
            .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(!window.has_active_dialog(cx));
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
            assert!(table.read(cx).fields.is_empty());
            assert_eq!(table.read(cx).query.page, 0);
        })
        .unwrap();
        async_std::task::block_on(session.run_sql("mysql", "DROP DATABASE row_controls_test"))
            .unwrap();
    }
}
