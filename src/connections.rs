use crate::{
    database,
    settings::{Connection, Settings},
};
use gpui_kit::{
    component::{
        ActiveTheme, Icon, Sizable, h_flex,
        list::ListItem,
        tree::{TreeItem, TreeState, tree},
    },
    prelude::FluentBuilder,
    *,
};

struct Entry {
    config: Connection,
    item: TreeItem,
    loading: bool,
    session: Option<std::sync::Arc<database::Session>>,
    error: Option<String>,
}

pub(crate) struct Connections {
    entries: Vec<Entry>,
    tree: Entity<TreeState>,
    next_id: u64,
    #[cfg(debug_assertions)]
    debug_startup_connection: Option<SharedString>,
}

#[non_exhaustive]
#[derive(Clone)]
pub(crate) struct OpenTable {
    pub(crate) session: std::sync::Arc<database::Session>,
    pub(crate) connection_name: String,
    pub(crate) database: String,
    pub(crate) table: String,
}

impl EventEmitter<OpenTable> for Connections {}

impl Connections {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        cx.bind_keys([KeyBinding::new(
            "enter",
            gpui_kit::base::actions::Confirm { secondary: false },
            Some("Connections"),
        )]);
        let mut view = Self {
            entries: Vec::new(),
            tree: cx.new(|cx| TreeState::new(cx)),
            next_id: 0,
            #[cfg(debug_assertions)]
            debug_startup_connection: None,
        };
        cx.observe(&view.tree, |_, _, cx| cx.notify()).detach();
        view.sync(cx);
        cx.observe_global::<Settings>(|view, cx| view.sync(cx))
            .detach();
        view
    }

    #[cfg(debug_assertions)]
    pub(crate) fn open_debug_table(&mut self, cx: &mut Context<Self>) {
        let Some(item) = self
            .entries
            .iter()
            .find(|entry| entry.config.database_type == crate::settings::DatabaseKind::MariaDB)
            .map(|entry| entry.item.clone())
        else {
            return;
        };
        self.debug_startup_connection = Some(item.id.clone());
        self.tree.update(cx, |state, cx| {
            state.set_selected_item(Some(&item), cx);
        });
        self.connect(&item.id, cx);
    }

    fn sync(&mut self, cx: &mut Context<Self>) {
        let configs = cx
            .try_global::<Settings>()
            .map(|settings| settings.connections.clone())
            .unwrap_or_default();
        let mut previous = std::mem::take(&mut self.entries);
        self.entries = configs
            .into_iter()
            .map(|config| {
                if let Some(ix) = previous.iter().position(|entry| entry.config == config) {
                    previous.remove(ix)
                } else {
                    self.next_id += 1;
                    Entry {
                        item: TreeItem::new(
                            format!("connection-{}", self.next_id),
                            config.name.clone(),
                        ),
                        config,
                        loading: false,
                        session: None,
                        error: None,
                    }
                }
            })
            .collect();
        self.update_tree(cx);
    }

    fn update_tree(&self, cx: &mut Context<Self>) {
        let items = self
            .entries
            .iter()
            .map(|entry| entry.item.clone())
            .collect::<Vec<_>>();
        self.tree.update(cx, |state, cx| {
            let selected = state.selected_item().cloned();
            state.set_items(items, cx);
            state.set_selected_item(selected.as_ref(), cx);
        });
        cx.notify();
    }

    fn activate(&mut self, id: &SharedString, cx: &mut Context<Self>) {
        for entry in &self.entries {
            for database in &entry.item.children {
                if let Some(table) = database
                    .children
                    .iter()
                    .find(|table| table.id == *id && !table.is_disabled())
                    && let Some(session) = &entry.session
                {
                    cx.emit(OpenTable {
                        session: session.clone(),
                        connection_name: entry.config.name.clone(),
                        database: database.label.to_string(),
                        table: table.label.to_string(),
                    });
                    return;
                }
            }
        }
        self.connect(id, cx);
    }

    fn connect(&mut self, id: &SharedString, cx: &mut Context<Self>) {
        let Some(entry) = self.entries.iter_mut().find(|entry| entry.item.id == *id) else {
            return;
        };
        if entry.loading || entry.session.is_some() {
            return;
        }
        entry.loading = true;
        entry.error = None;
        let config = entry.config.clone();
        let id = id.clone();
        let task = cx
            .background_spawn(async move { async_std::task::block_on(database::connect(&config)) });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |view, cx| view.finish(&id, result, cx));
        })
        .detach();
        cx.notify();
    }

    fn finish(
        &mut self,
        id: &SharedString,
        result: Result<database::Session, String>,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.entries.iter_mut().find(|entry| entry.item.id == *id) else {
            return;
        };
        entry.loading = false;
        match result {
            Ok(session) => {
                entry.item.children = session
                    .databases
                    .iter()
                    .map(|(name, tables)| {
                        let database_id = format!("{id}/database/{name}");
                        TreeItem::new(database_id.clone(), name.clone())
                            .children(tables.iter().map(|table| {
                                TreeItem::new(format!("{database_id}/table/{table}"), table.clone())
                            }))
                            .children(tables.is_empty().then(|| {
                                TreeItem::new(format!("{database_id}/empty"), "No tables")
                                    .disabled(true)
                            }))
                    })
                    .collect();
                entry.item.clone().expanded(true);
                entry.session = Some(std::sync::Arc::new(session));
                entry.error = None;
            }
            Err(error) => entry.error = Some(error),
        }
        self.update_tree(cx);
        #[cfg(debug_assertions)]
        if self.debug_startup_connection.as_ref() == Some(id) {
            self.debug_startup_connection = None;
            let table = self
                .entries
                .iter()
                .find(|entry| entry.item.id == *id)
                .and_then(|entry| entry.item.children.iter().find(|db| db.label == "apps"))
                .and_then(|db| db.children.iter().find(|table| table.label == "apps"))
                .cloned();
            if let Some(table) = table {
                self.tree.update(cx, |state, cx| {
                    state.set_selected_item(Some(&table), cx);
                });
                self.activate(&table.id, cx);
            }
        }
    }
}

impl Render for Connections {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity().downgrade();
        let tree_state = self.tree.clone();
        let statuses = self
            .entries
            .iter()
            .map(|entry| {
                let text = if entry.loading {
                    "Connecting…".to_owned()
                } else if let Some(error) = &entry.error {
                    error.clone()
                } else if let Some(session) = &entry.session {
                    if session.databases.is_empty() {
                        "Connected · No databases available".into()
                    } else {
                        "Connected".into()
                    }
                } else {
                    "Double-click or press Enter to connect".into()
                };
                (
                    entry.item.id.clone(),
                    entry.session.is_some(),
                    text,
                    entry.error.is_some(),
                )
            })
            .collect::<Vec<_>>();
        let selected = self.tree.read(cx).selected_item();
        let status = selected
            .and_then(|item| statuses.iter().find(|(id, ..)| *id == item.id))
            .map(|(_, _, text, error)| (text.clone(), *error));
        div()
            .key_context("Connections")
            .flex()
            .flex_col()
            .size_full()
            .capture_action(
                cx.listener(|view, _: &gpui_kit::base::actions::Confirm, _, cx| {
                    let selected = view
                        .tree
                        .read(cx)
                        .selected_item()
                        .map(|item| item.id.clone());
                    if let Some(id) = selected {
                        view.activate(&id, cx);
                    }
                }),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(tree(&self.tree, move |_, entry, _, _, cx| {
                        use gpui_kit::assets::IconName;
                        let item = entry.item();
                        let status = statuses.iter().find(|(id, ..)| *id == item.id);
                        let connected = status.is_some_and(|(_, connected, ..)| *connected);
                        let description = status
                            .map(|(_, _, text, _)| text.clone())
                            .unwrap_or_else(|| item.label.to_string());
                        let icon = if entry.is_folder() || entry.depth() == 1 {
                            if entry.is_expanded() {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            }
                        } else if entry.depth() == 2 {
                            IconName::Table
                        } else {
                            IconName::Database
                        };
                        let id = item.id.clone();
                        let view = view.clone();
                        let tree_state = tree_state.clone();
                        ListItem::new(item.id.clone())
                            .accessibility_label(format!("{} · {}", item.label, description))
                            .w_full()
                            .h_8()
                            .px_2()
                            .pl(rems(0.5 + entry.depth() as f32))
                            .text_sm()
                            .child(
                                h_flex()
                                    .w_full()
                                    .gap_2()
                                    .child(Icon::new(icon).small())
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .child(item.label.clone()),
                                    )
                                    .when(connected, |row| {
                                        row.child(
                                            div()
                                                .id(format!("{}-connected", item.id))
                                                .test_support()
                                                .role(Role::Status)
                                                .aria_label("Connected")
                                                .flex_none()
                                                .size_2()
                                                .rounded_full()
                                                .bg(cx.theme().success),
                                        )
                                    }),
                            )
                            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                                tree_state.update(cx, |state, cx| state.focus(window, cx));
                            })
                            .on_click(move |event, _, cx| {
                                if event.click_count() == 2 || event.is_keyboard() {
                                    let _ = view.update(cx, |view, cx| view.activate(&id, cx));
                                }
                            })
                    })),
            )
            .when_some(status, |view, (text, error)| {
                view.child(
                    div()
                        .id("connection-status")
                        .test_support()
                        .role(Role::Status)
                        .aria_label(text.clone())
                        .flex_none()
                        .p_2()
                        .text_xs()
                        .text_color(if error {
                            cx.theme().danger
                        } else {
                            cx.theme().muted_foreground
                        })
                        .child(text),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::Connections;
    use crate::settings::{Connection, DatabaseKind, Settings};
    use gpui_kit::{AppContext, TestAppContext, component::Root, px, size, test::TestWindowExt};

    fn config(kind: DatabaseKind) -> Connection {
        Connection {
            name: "Local".into(),
            database_type: kind,
            host: "127.0.0.1".into(),
            port: Some(3306),
            username: "root".into(),
            password: String::new(),
            database: String::new(),
            file_path: String::new(),
        }
    }

    fn wait_for_connection(cx: &mut TestAppContext, view: &gpui_kit::Entity<Connections>) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            cx.run_until_parked();
            if cx.update(|cx| view.read(cx).entries.iter().all(|entry| !entry.loading)) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "connection never completed"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[gpui_kit::test]
    fn single_click_selects_double_click_connects_and_enter_retries(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(Settings {
                connections: vec![config(DatabaseKind::MySQL)],
                ..Settings::default()
            });
        });
        let mut view = None;
        let handle = cx.open_window(size(px(320.), px(400.)), |window, cx| {
            let entity = cx.new(Connections::new);
            view = Some(entity.clone());
            Root::new(entity, window, cx)
        });
        let view = view.unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("connection-1", cx);
            assert!(
                window
                    .find("connection-status")
                    .label()
                    .unwrap()
                    .contains("Double-click")
            );
            assert!(!view.read(cx).entries[0].loading);
            window.double_click("connection-1", cx);
            assert!(view.read(cx).entries[0].loading);
            assert_eq!(
                window.find("connection-status").label(),
                Some("Connecting…")
            );
        })
        .unwrap();
        wait_for_connection(cx, &view);
        cx.update_window(handle.into(), |_, window, cx| {
            assert_eq!(
                window.find("connection-status").label(),
                Some("Only MariaDB connections are supported yet.")
            );
            assert!(window.try_find("connection-1-connected").is_none());
            window.press("enter", cx);
            assert!(view.read(cx).entries[0].loading);
            assert!(view.read(cx).entries[0].error.is_none());
        })
        .unwrap();
        wait_for_connection(cx, &view);
        cx.update(|cx| {
            let mut settings = cx.global::<Settings>().clone();
            settings
                .connections
                .insert(0, config(DatabaseKind::MariaDB));
            cx.set_global(settings);
        });
        cx.run_until_parked();
        cx.update(|cx| {
            assert_eq!(view.read(cx).entries[1].item.id.as_str(), "connection-1");
            assert!(view.read(cx).entries[1].error.is_some());
            assert_ne!(
                view.read(cx).entries[0].item.id,
                view.read(cx).entries[1].item.id
            );
        });
    }

    #[gpui_kit::test]
    #[ignore = "requires a temporary MariaDB server and TABLELANE_TEST_MARIADB_PORT"]
    fn mariadb_double_click_loads_tree_and_connected_indicator(cx: &mut TestAppContext) {
        let mut connection = config(DatabaseKind::MariaDB);
        connection.port = Some(
            std::env::var("TABLELANE_TEST_MARIADB_PORT")
                .expect("test port")
                .parse()
                .unwrap(),
        );
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(Settings {
                connections: vec![connection],
                ..Settings::default()
            });
        });
        let mut view = None;
        let handle = cx.open_window(size(px(320.), px(500.)), |window, cx| {
            let entity = cx.new(Connections::new);
            view = Some(entity.clone());
            Root::new(entity, window, cx)
        });
        let view = view.unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.double_click("connection-1", cx)
        })
        .unwrap();
        wait_for_connection(cx, &view);
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(
                view.read(cx).entries[0].error.is_none(),
                "{:?}",
                view.read(cx).entries[0].error
            );
            window.render_frame(cx);
            assert_eq!(
                window.find("connection-1-connected").label(),
                Some("Connected")
            );
            assert_eq!(window.find("connection-status").label(), Some("Connected"));
            let empty = "connection-1/database/empty_db";
            let empty_message = "connection-1/database/empty_db/empty";
            assert!(window.try_find(empty_message).is_none());
            window.click(empty, cx);
            assert!(window.find(empty_message).visible());
            assert!(
                window
                    .find(empty_message)
                    .label()
                    .unwrap()
                    .contains("No tables")
            );
            window.click(empty, cx);
            assert!(window.try_find(empty_message).is_none());
            let database = "connection-1/database/tablelane_test";
            assert!(window.find(database).visible());
            assert!(
                window.find(database).bounds().top() > window.find("connection-1").bounds().top()
            );
            window.click(database, cx);
            assert!(
                window
                    .find("connection-1/database/tablelane_test/table/widgets")
                    .visible()
            );
            window.click("connection-1", cx);
            assert!(window.try_find(database).is_none());
            window.click("connection-1", cx);
            assert!(window.find(database).visible());
            window.click(database, cx);
            window.press("enter", cx);
            assert!(
                window
                    .find("connection-1/database/tablelane_test/table/widgets")
                    .visible()
            );
        })
        .unwrap();
    }
}
