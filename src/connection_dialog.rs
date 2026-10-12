use crate::settings::{Connection, DatabaseKind, Settings};
use gpui_kit::{
    component::{
        ActiveTheme, Disableable, Selectable, WindowExt,
        button::{Button, ButtonVariants},
        dialog::{DialogAction, DialogFooter},
        form::{Field, Form},
        input::{Input, InputEvent, InputState},
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use std::path::PathBuf;

pub(crate) fn open(path: Option<PathBuf>, window: &mut Window, cx: &mut App) {
    open_form(path, None, window, cx);
}

pub(crate) fn edit(path: Option<PathBuf>, ix: usize, window: &mut Window, cx: &mut App) {
    let Some(connection) = cx.global::<Settings>().connections.get(ix).cloned() else {
        return;
    };
    open_form(path, Some((ix, connection)), window, cx);
}

fn open_form(
    path: Option<PathBuf>,
    original: Option<(usize, Connection)>,
    window: &mut Window,
    cx: &mut App,
) {
    if window.has_active_dialog(cx) {
        return;
    }
    let editing = original.is_some();
    let view = cx.new(|cx| {
        let mut form = ConnectionDialog::new(path, window, cx);
        if let Some((_, connection)) = &original {
            form.selected = connection.database_type;
            for (input, value) in [
                (&form.name, connection.name.clone()),
                (&form.host, connection.host.clone()),
                (
                    &form.port,
                    connection
                        .port
                        .map(|port| port.to_string())
                        .unwrap_or_default(),
                ),
                (&form.username, connection.username.clone()),
                (&form.password, connection.password.clone()),
                (&form.database, connection.database.clone()),
                (&form.file_path, connection.file_path.clone()),
            ] {
                input.update(cx, |input, cx| input.set_value(value, window, cx));
            }
        }
        form.original = original;
        form
    });
    window.open_dialog(cx, move |dialog, window, _| {
        let confirm = view.clone();
        dialog
            .title(if editing {
                "Edit connection"
            } else {
                "Add connection"
            })
            .width(window.rem_size() * 44.)
            .child(view.clone())
            .on_ok(move |_, _, cx| {
                confirm.update(cx, |view, cx| {
                    if view.deleting {
                        view.delete(cx)
                    } else {
                        view.save(cx)
                    }
                })
            })
    });
}

struct ConnectionDialog {
    path: Option<PathBuf>,
    selected: DatabaseKind,
    name: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    username: Entity<InputState>,
    password: Entity<InputState>,
    database: Entity<InputState>,
    file_path: Entity<InputState>,
    error: Option<String>,
    original: Option<(usize, Connection)>,
    testing: bool,
    deleting: bool,
    status: Option<String>,
}

impl ConnectionDialog {
    fn new(path: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx));
        let host = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_value("localhost", window, cx);
            state
        });
        let port = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_value("3306", window, cx);
            state
        });
        let username = cx.new(|cx| InputState::new(window, cx));
        let password = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_masked(true, window, cx);
            state
        });
        let database = cx.new(|cx| InputState::new(window, cx));
        let file_path =
            cx.new(|cx| InputState::new(window, cx).placeholder("/path/to/database.sqlite"));
        for input in [
            &name, &host, &port, &username, &password, &database, &file_path,
        ] {
            cx.subscribe(input, |view, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) && !view.testing {
                    view.status = None;
                    cx.notify();
                }
            })
            .detach();
        }
        Self {
            path,
            selected: DatabaseKind::MySQL,
            name,
            host,
            port,
            username,
            password,
            database,
            file_path,
            error: None,
            original: None,
            testing: false,
            deleting: false,
            status: None,
        }
    }

    fn select(&mut self, kind: DatabaseKind, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected == kind {
            return;
        }
        self.selected = kind;
        self.status = None;
        self.port.update(cx, |state, cx| {
            state.set_value(
                kind.port().map(|port| port.to_string()).unwrap_or_default(),
                window,
                cx,
            );
        });
        self.error = None;
        cx.notify();
    }

    fn connection(&self, cx: &App) -> Result<Connection, String> {
        let sqlite = self.selected == DatabaseKind::SQLite;
        let connection = Connection {
            name: self.name.read(cx).value().trim().to_owned(),
            database_type: self.selected,
            host: if sqlite {
                String::new()
            } else {
                self.host.read(cx).value().trim().to_owned()
            },
            port: if sqlite {
                None
            } else {
                Some(
                    self.port
                        .read(cx)
                        .value()
                        .trim()
                        .parse::<u16>()
                        .map_err(|_| "Enter a port from 1 to 65535.")?,
                )
            },
            username: if sqlite {
                String::new()
            } else {
                self.username.read(cx).value().trim().to_owned()
            },
            password: if sqlite {
                String::new()
            } else {
                self.password.read(cx).value().to_string()
            },
            database: if sqlite {
                String::new()
            } else {
                self.database.read(cx).value().trim().to_owned()
            },
            file_path: if sqlite {
                self.file_path.read(cx).value().trim().to_owned()
            } else {
                String::new()
            },
        };
        connection.validate()?;
        Ok(connection)
    }

    fn test(&mut self, cx: &mut Context<Self>) {
        if self.testing {
            return;
        }
        let connection = match self.connection(cx) {
            Ok(connection) => connection,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        self.testing = true;
        self.error = None;
        self.status = Some("Testing connection…".into());
        let tested = connection.clone();
        let task = cx.background_spawn(async move {
            async_std::task::block_on(crate::database::connect(&connection)).map(|_| ())
        });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |view, cx| {
                view.testing = false;
                view.status = None;
                if view.connection(cx).as_ref() == Ok(&tested) {
                    match result {
                        Ok(()) => view.status = Some("Connection successful".into()),
                        Err(error) => view.error = Some(error),
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn delete(&mut self, cx: &mut Context<Self>) -> bool {
        let result = (|| -> Result<(), String> {
            let (ix, original) = self
                .original
                .as_ref()
                .ok_or("No saved connection selected.")?;
            let path = self.path.as_ref().ok_or("Settings saving is disabled.")?;
            let mut settings = cx.global::<Settings>().clone();
            if settings.connections.get(*ix) != Some(original) {
                return Err("This connection changed. Close the dialog and reopen it.".into());
            }
            settings.connections.remove(*ix);
            settings
                .save(path)
                .map_err(|error| format!("Couldn’t delete connection: {error}"))?;
            cx.set_global(settings);
            cx.refresh_windows();
            Ok(())
        })();
        match result {
            Ok(()) => true,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                false
            }
        }
    }

    fn save(&mut self, cx: &mut Context<Self>) -> bool {
        if self.testing {
            return false;
        }
        let result = (|| -> Result<(), String> {
            let path = self
                .path
                .as_ref()
                .ok_or("Settings saving is disabled. Fix the settings file and restart the app.")?;
            let connection = self.connection(cx)?;
            let mut settings = cx.global::<Settings>().clone();
            if let Some((ix, original)) = &self.original {
                if settings.connections.get(*ix) != Some(original) {
                    return Err("This connection changed. Close the dialog and reopen it.".into());
                }
                settings.connections[*ix] = connection;
            } else {
                settings.connections.push(connection);
            }
            settings.save(path).map_err(|error| {
                format!("Couldn’t save connection: {error}. Your changes have not been saved.")
            })?;
            cx.set_global(settings);
            cx.refresh_windows();
            Ok(())
        })();
        match result {
            Ok(()) => true,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                false
            }
        }
    }
}

impl Render for ConnectionDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.deleting {
            let name = self
                .original
                .as_ref()
                .map(|(_, connection)| connection.name.as_str())
                .unwrap_or_default();
            return v_flex()
                .gap_3()
                .child(format!("Delete “{name}”?"))
                .child("Remove this saved connection. Database data and open tabs are kept.")
                .children(self.error.clone().map(|error| {
                    div()
                        .id("connection-error")
                        .test_support()
                        .role(Role::Status)
                        .aria_label(error.clone())
                        .text_color(cx.theme().danger)
                        .child(error)
                }))
                .child(
                    DialogFooter::new()
                        .child(
                            Button::new("cancel-delete-connection")
                                .label("Cancel")
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.deleting = false;
                                    view.error = None;
                                    cx.notify();
                                })),
                        )
                        .child(
                            DialogAction::new().child(
                                Button::new("confirm-delete-connection")
                                    .danger()
                                    .label("Delete"),
                            ),
                        ),
                )
                .into_any_element();
        }
        let sqlite = self.selected == DatabaseKind::SQLite;
        v_flex()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_stretch()
                    .w_full()
                    .gap_4()
                    .child(
                        v_flex()
                            .id("database-list")
                            .test_support()
                            .w_40()
                            .flex_none()
                            .p_2()
                            .gap_1()
                            .bg(cx.theme().sidebar)
                            .rounded(cx.theme().radius)
                            .children(DatabaseKind::ALL.map(|kind| {
                                Button::new(kind.label())
                                    .ghost()
                                    .label(kind.label())
                                    .justify_start()
                                    .selected(self.selected == kind)
                                    .toggled(self.selected == kind)
                                    .on_click(cx.listener(move |view, _, window, cx| {
                                        view.select(kind, window, cx)
                                    }))
                            })),
                    )
                    .child(
                        Form::new()
                            .layout(Axis::Horizontal)
                            .flex_1()
                            .min_w_0()
                            .child(
                                Field::new()
                                    .label("Connection name")
                                    .child(Input::new(&self.name).id("connection-name")),
                            )
                            .when(sqlite, |form| {
                                form.child(
                                    Field::new().label("File path").child(
                                        Input::new(&self.file_path).id("connection-file-path"),
                                    ),
                                )
                            })
                            .when(!sqlite, |form| {
                                form.child(
                                    Field::new()
                                        .label("Host")
                                        .child(Input::new(&self.host).id("connection-host")),
                                )
                                .child(
                                    Field::new()
                                        .label("Port")
                                        .child(Input::new(&self.port).id("connection-port")),
                                )
                                .child(
                                    Field::new().label("Username").child(
                                        Input::new(&self.username).id("connection-username"),
                                    ),
                                )
                                .child(
                                    Field::new().label("Password").child(
                                        Input::new(&self.password).id("connection-password"),
                                    ),
                                )
                                .child(
                                    Field::new().label("Database (optional)").child(
                                        Input::new(&self.database).id("connection-database"),
                                    ),
                                )
                            }),
                    ),
            )
            .child(
                div()
                    .id("connection-error")
                    .test_support()
                    .role(Role::Status)
                    .aria_label(self.error.clone().unwrap_or_default())
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .children(self.error.clone()),
            )
            .children(self.status.clone().map(|status| {
                div()
                    .id("connection-test-status")
                    .test_support()
                    .role(Role::Status)
                    .aria_label(status.clone())
                    .text_sm()
                    .child(status)
            }))
            .child(
                DialogFooter::new()
                    .child(
                        Button::new("test-connection")
                            .label("Test connection")
                            .loading(self.testing)
                            .disabled(self.testing)
                            .on_click(cx.listener(|view, _, _, cx| view.test(cx))),
                    )
                    .when(self.original.is_some(), |footer| {
                        footer.child(
                            Button::new("delete-connection")
                                .danger()
                                .label("Delete…")
                                .disabled(self.testing)
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.deleting = true;
                                    view.error = None;
                                    cx.notify();
                                })),
                        )
                    })
                    .child(
                        Button::new("cancel")
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        DialogAction::new().child(
                            Button::new("save-connection")
                                .primary()
                                .disabled(self.testing)
                                .label(if self.original.is_some() {
                                    "Save connection"
                                } else {
                                    "Add connection"
                                }),
                        ),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        AppView,
        settings::{DatabaseKind, Settings},
    };
    use gpui_kit::{
        App, AppContext, TestAppContext, Window,
        component::{Root, WindowExt},
        px, size,
        test::{TestAppContextExt, TestWindowExt},
    };

    fn fill(window: &mut Window, id: &'static str, value: &str, cx: &mut App) {
        window.click(id, cx);
        window.press("secondary-a", cx);
        window.input(value, cx);
    }

    #[gpui_kit::test]
    #[ignore = "requires an isolated MariaDB server and TABLELANE_TEST_MARIADB_PORT"]
    async fn mariadb_connection_test_preserves_settings_and_invalidates_result(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(Settings::default());
        });
        let handle = cx.open_window(size(px(1100.), px(800.)), |window, cx| {
            Root::new(
                cx.new(|cx| AppView::new(None, None, None, window, cx)),
                window,
                cx,
            )
        });
        cx.update_window(handle.into(), |_, window, cx| {
            super::open(None, window, cx);
            window.click("MariaDB", cx);
            fill(window, "connection-name", "Temporary test", cx);
            fill(window, "connection-host", "127.0.0.1", cx);
            fill(
                window,
                "connection-port",
                &std::env::var("TABLELANE_TEST_MARIADB_PORT").unwrap(),
                cx,
            );
            fill(window, "connection-username", "root", cx);
            window.click("test-connection", cx);
        })
        .unwrap();
        cx.wait_for(
            handle.into(),
            std::time::Duration::from_secs(15),
            |window, _| {
                window
                    .try_find("connection-test-status")
                    .is_some_and(|node| node.label() == Some("Connection successful"))
            },
        )
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(cx.global::<Settings>().connections.is_empty());
            fill(window, "connection-name", "Changed", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(window.try_find("connection-test-status").is_none());
            window.click("cancel", cx);
        })
        .unwrap();
    }

    #[gpui_kit::test]
    async fn saved_connections_edit_test_delete_and_preserve_failed_changes(
        cx: &mut TestAppContext,
    ) {
        let directory = std::env::temp_dir().join(format!(
            "tablelane-edit-connections-test-{}",
            std::process::id()
        ));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("settings.json");
        let initial = Settings {
            connections: vec![crate::settings::Connection {
                name: "Saved".into(),
                database_type: DatabaseKind::MariaDB,
                host: "localhost".into(),
                port: Some(3306),
                username: "original".into(),
                password: "secret".into(),
                database: String::new(),
                file_path: String::new(),
            }],
            ..Settings::default()
        };
        initial.save(&path).unwrap();
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(initial.clone());
        });
        let handle = cx.open_window(size(px(1100.), px(800.)), |window, cx| {
            Root::new(
                cx.new(|cx| AppView::new(Some(path.clone()), None, None, window, cx)),
                window,
                cx,
            )
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("edit-connection", cx);
            assert!(!window.has_active_dialog(cx));
            window.click("connection-1", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("edit-connection", cx);
            assert_eq!(window.find("connection-name").value(), Some("Saved"));
            assert_eq!(window.find("connection-username").value(), Some("original"));
            fill(window, "connection-name", "Renamed", cx);
            window.click("save-connection", cx);
        })
        .unwrap();
        cx.run_until_parked();
        let renamed = Settings::read(&path).unwrap();
        assert_eq!(renamed.connections.len(), 1);
        assert_eq!(renamed.connections[0].name, "Renamed");
        assert_eq!(renamed.connections[0].password, "secret");
        cx.update_window(handle.into(), |_, window, cx| {
            super::edit(Some(path.clone()), 0, window, cx);
            let mut changed = renamed.clone();
            changed.connections[0].username = "concurrent change".into();
            cx.set_global(changed);
            fill(window, "connection-name", "Stale draft", cx);
            window.click("save-connection", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(
                window
                    .find("connection-error")
                    .label()
                    .unwrap()
                    .contains("connection changed")
            );
            assert_eq!(
                cx.global::<Settings>().connections[0].username,
                "concurrent change"
            );
            window.click("cancel", cx);
            cx.set_global(renamed.clone());
        })
        .unwrap();
        assert_eq!(Settings::read(&path).unwrap(), renamed);
        cx.update_window(handle.into(), |_, window, cx| {
            super::edit(Some(path.clone()), 0, window, cx);
            window.click("SQLite", cx);
            fill(
                window,
                "connection-file-path",
                "/tmp/unsupported.sqlite",
                cx,
            );
            window.click("test-connection", cx);
        })
        .unwrap();
        cx.wait_for(
            handle.into(),
            std::time::Duration::from_secs(5),
            |window, _| {
                window.try_find("connection-error").is_some_and(|node| {
                    node.label()
                        .is_some_and(|label| label.contains("Only MariaDB"))
                })
            },
        )
        .await;
        assert_eq!(Settings::read(&path).unwrap(), renamed);
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("cancel", cx);
            super::edit(Some(path.clone()), 0, window, cx);
            window.click("delete-connection", cx);
            window.click("cancel-delete-connection", cx);
            assert_eq!(window.find("connection-name").value(), Some("Renamed"));
            window.click("delete-connection", cx);
            // An unwritable destination must preserve both disk and in-memory settings.
            std::fs::create_dir(path.with_extension(format!("{}.tmp", std::process::id())))
                .unwrap();
            window.click("confirm-delete-connection", cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(Settings::read(&path).unwrap(), renamed);
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(
                window
                    .find("connection-error")
                    .label()
                    .unwrap()
                    .contains("Couldn’t delete")
            );
            assert_eq!(cx.global::<Settings>(), &renamed);
            std::fs::remove_dir(path.with_extension(format!("{}.tmp", std::process::id())))
                .unwrap();
            window.click("confirm-delete-connection", cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert!(Settings::read(&path).unwrap().connections.is_empty());
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(!window.has_active_dialog(cx))
        })
        .unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[gpui_kit::test]
    fn connections_validate_save_reload_cancel_and_preserve_settings(cx: &mut TestAppContext) {
        let directory =
            std::env::temp_dir().join(format!("tablelane-connections-test-{}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("settings.json");
        let initial = Settings {
            display_name: "Ada".into(),
            ..Settings::default()
        };
        initial.save(&path).unwrap();
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(initial.clone());
        });
        let handle = cx.open_window(size(px(1000.), px(700.)), |window, cx| {
            let view = cx.new(|cx| AppView::new(Some(path.clone()), None, None, window, cx));
            Root::new(view, window, cx)
        });
        for (ix, kind) in DatabaseKind::ALL.into_iter().enumerate() {
            cx.update_window(handle.into(), |_, window, cx| {
                window.click("add-connection", cx);
                window.click(kind.label(), cx);
                fill(window, "connection-name", kind.label(), cx);
                if kind == DatabaseKind::SQLite {
                    assert!(window.try_find("connection-host").is_none());
                    fill(window, "connection-file-path", "/tmp/example.sqlite", cx);
                } else {
                    assert!(window.try_find("connection-file-path").is_none());
                    assert_eq!(
                        window.find("connection-port").value(),
                        Some(kind.port().unwrap().to_string().as_str())
                    );
                    fill(window, "connection-username", "user", cx);
                    fill(window, "connection-password", " secret ", cx);
                    assert_eq!(window.find("connection-password").value(), None);
                    fill(window, "connection-database", "example", cx);
                }
                window.click("save-connection", cx);
            })
            .unwrap();
            cx.run_until_parked();
            let saved = Settings::load(&path).unwrap();
            let stored: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert!(stored["connections"][ix]["password"]["aes256_gcm_v1"].is_string());
            assert!(!std::fs::read_to_string(&path).unwrap().contains(" secret "));
            assert_eq!(saved.display_name, "Ada");
            assert_eq!(saved.connections.len(), ix + 1);
            assert_eq!(saved.connections[ix].database_type, kind);
            if kind != DatabaseKind::SQLite {
                assert_eq!(saved.connections[ix].password, " secret ");
            }
            cx.update_window(handle.into(), |_, window, cx| {
                assert!(!window.has_active_dialog(cx));
                assert_eq!(cx.global::<Settings>(), &saved);
            })
            .unwrap();
        }
        let original = std::fs::read(&path).unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("add-connection", cx);
            window.click("save-connection", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(
                window
                    .find("connection-error")
                    .label()
                    .unwrap()
                    .contains("name")
            );
            fill(window, "connection-name", "Another", cx);
            fill(window, "connection-port", "0", cx);
            window.click("save-connection", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(
                window
                    .find("connection-error")
                    .label()
                    .unwrap()
                    .contains("port")
            );
            fill(window, "connection-port", "65536", cx);
            window.click("save-connection", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(
                window
                    .find("connection-error")
                    .label()
                    .unwrap()
                    .contains("port")
            );
            window.click("SQLite", cx);
            window.click("save-connection", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(
                window
                    .find("connection-error")
                    .label()
                    .unwrap()
                    .contains("file path")
            );
            fill(window, "connection-file-path", "/tmp/another.sqlite", cx);
            std::fs::create_dir(path.with_extension(format!("{}.tmp", std::process::id())))
                .unwrap();
            window.click("save-connection", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(
                window
                    .find("connection-error")
                    .label()
                    .unwrap()
                    .contains("Couldn’t save")
            );
            assert_eq!(window.find("connection-name").value(), Some("Another"));
            assert_eq!(std::fs::read(&path).unwrap(), original);
            assert_eq!(cx.global::<Settings>().connections.len(), 5);
            std::fs::remove_dir(path.with_extension(format!("{}.tmp", std::process::id())))
                .unwrap();
            window.click("cancel", cx);
            window.click("add-connection", cx);
            assert_eq!(window.find("connection-name").value(), Some(""));
            window.click("cancel", cx);
            crate::settings_view::open(Some(path.clone()), None, window, cx);
            window.click("save", cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(
            Settings::read(&path).unwrap(),
            serde_json::from_slice::<Settings>(&original).unwrap()
        );
        cx.update_window(handle.into(), |_, window, cx| {
            super::open(None, window, cx);
            window.click("save-connection", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, _| {
            assert!(
                window
                    .find("connection-error")
                    .label()
                    .unwrap()
                    .contains("disabled")
            );
        })
        .unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }
}
