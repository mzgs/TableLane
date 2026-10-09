use crate::settings::{Connection, DatabaseKind, Settings};
use gpui_kit::{
    component::{
        ActiveTheme, Selectable, WindowExt,
        button::{Button, ButtonVariants},
        dialog::{DialogAction, DialogFooter},
        form::{Field, Form},
        input::{Input, InputState},
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use std::path::PathBuf;

pub(crate) fn open(path: Option<PathBuf>, window: &mut Window, cx: &mut App) {
    if window.has_active_dialog(cx) {
        return;
    }
    let view = cx.new(|cx| ConnectionDialog::new(path, window, cx));
    window.open_dialog(cx, move |dialog, window, _| {
        let confirm = view.clone();
        dialog
            .title("Add connection")
            .width(window.rem_size() * 44.)
            .child(view.clone())
            .on_ok(move |_, _, cx| confirm.update(cx, |view, cx| view.save(cx)))
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
        }
    }

    fn select(&mut self, kind: DatabaseKind, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected == kind {
            return;
        }
        self.selected = kind;
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

    fn save(&mut self, cx: &mut Context<Self>) -> bool {
        let result = (|| -> Result<(), String> {
            let path = self
                .path
                .as_ref()
                .ok_or("Settings saving is disabled. Fix the settings file and restart the app.")?;
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
            let mut settings = cx.global::<Settings>().clone();
            settings.connections.push(connection);
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
            .child(
                DialogFooter::new()
                    .child(
                        Button::new("cancel")
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        DialogAction::new().child(
                            Button::new("save-connection")
                                .primary()
                                .label("Add connection"),
                        ),
                    ),
            )
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
        test::TestWindowExt,
    };

    fn fill(window: &mut Window, id: &'static str, value: &str, cx: &mut App) {
        window.click(id, cx);
        window.press("secondary-a", cx);
        window.input(value, cx);
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
