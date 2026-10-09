mod connection_dialog;
mod connections;
mod database;
mod settings;
mod settings_view;
mod table_view;
mod window_state;

use gpui_kit::component::{
    ActiveTheme, IconName, Selectable, Sizable, Theme, TitleBar,
    button::{Button, ButtonGroup, ButtonVariants},
    resizable::{h_resizable, resizable_panel},
    status_bar::StatusBar,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use settings::Settings;
use window_state::WindowState;

gpui_kit::assets::icon_assets!(
    SettingsIcons,
    [Upload, Download, PanelBottom, PanelRight, Database, Table]
);

struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<std::borrow::Cow<'static, [u8]>>> {
        if let Some(bytes) = SettingsIcons.load(path)? {
            return Ok(Some(bytes));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(SettingsIcons.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

impl Global for Settings {}

actions!(base_app, [Quit, OpenSettings]);

struct AppView {
    connections: Entity<connections::Connections>,
    table: Option<Entity<table_view::TableView>>,
    sidebar_visible: bool,
    right_sidebar_visible: bool,
    bottom_bar_visible: bool,
    settings_path: Option<std::path::PathBuf>,
    window_path: Option<std::path::PathBuf>,
    window_state: WindowState,
}

impl AppView {
    fn new(
        settings_path: Option<std::path::PathBuf>,
        window_path: Option<std::path::PathBuf>,
        restored: Option<WindowState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut window_state = restored.unwrap_or_else(|| WindowState::capture(window));
        window_state.update(window);
        cx.observe_window_bounds(window, |view, window, _| view.window_state.update(window))
            .detach();
        cx.on_app_quit(|view, _| {
            view.save_window_state();
            async {}
        })
        .detach();
        let weak = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            let _ = weak.update(cx, |view, _| {
                view.window_state.update(window);
                view.save_window_state();
            });
            true
        });
        let connections = cx.new(connections::Connections::new);
        cx.subscribe_in(
            &connections,
            window,
            |view, _, event: &connections::OpenTable, window, cx| {
                view.table =
                    Some(cx.new(|cx| table_view::TableView::new(event.clone(), window, cx)));
                cx.notify();
            },
        )
        .detach();
        Self {
            connections,
            table: None,
            sidebar_visible: true,
            right_sidebar_visible: false,
            bottom_bar_visible: true,
            settings_path,
            window_path,
            window_state,
        }
    }

    fn save_window_state(&self) {
        if let Some(path) = &self.window_path
            && let Err(error) = settings::write_json(path, &self.window_state)
        {
            eprintln!(
                "Couldn’t save window geometry to {}: {error}",
                path.display()
            );
        }
    }
}

impl Render for AppView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let workspace = h_resizable("workspace")
            .child(
                resizable_panel()
                    .visible(self.sidebar_visible)
                    .size(px(240.))
                    .size_range(px(160.)..px(400.))
                    .flex_none()
                    .child(
                        div()
                            .id("sidebar")
                            .test_support()
                            .size_full()
                            .flex()
                            .flex_col()
                            .bg(cx.theme().sidebar)
                            .p_2()
                            .child(
                                Button::new("add-connection")
                                    .icon(IconName::Plus)
                                    .label("Add connection…")
                                    .w_full()
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        connection_dialog::open(
                                            view.settings_path.clone(),
                                            window,
                                            cx,
                                        );
                                    })),
                            )
                            .child(div().flex_1().min_h_0().child(self.connections.clone())),
                    ),
            )
            .child(
                resizable_panel().child(
                    div()
                        .id("content")
                        .test_support()
                        .size_full()
                        .min_w_0()
                        .min_h_0()
                        .bg(cx.theme().background)
                        .when_some(self.table.as_ref(), |content, table| {
                            content.child(table.clone())
                        }),
                ),
            );
        let workspace = workspace.child(
            resizable_panel()
                .visible(self.right_sidebar_visible)
                .size(px(240.))
                .size_range(px(160.)..px(400.))
                .flex_none()
                .child(
                    div()
                        .id("right-sidebar")
                        .test_support()
                        .size_full()
                        .bg(cx.theme().sidebar),
                ),
        );
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(
                TitleBar::new()
                    .child(
                        Button::new("toolbar-settings")
                            .ghost()
                            .small()
                            .icon(IconName::Settings)
                            .accessibility_label("Settings…")
                            .tooltip("Settings…")
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(|_, window, cx| {
                                cx.stop_propagation();
                                window.dispatch_action(Box::new(OpenSettings), cx);
                            }),
                    )
                    .child(
                        ButtonGroup::new("layout-controls")
                            .small()
                            .outline()
                            .mr_2()
                            .child(
                                Button::new("toggle-sidebar")
                                    .icon(IconName::PanelLeft)
                                    .selected(self.sidebar_visible)
                                    .accessibility_label("Toggle left sidebar")
                                    .tooltip("Show or hide left sidebar")
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation()
                                    })
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        cx.stop_propagation();
                                        view.sidebar_visible = !view.sidebar_visible;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("toggle-bottom-bar")
                                    .icon(IconName::PanelBottom)
                                    .selected(self.bottom_bar_visible)
                                    .accessibility_label("Toggle bottom bar")
                                    .tooltip("Show or hide bottom bar")
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation()
                                    })
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        cx.stop_propagation();
                                        view.bottom_bar_visible = !view.bottom_bar_visible;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("toggle-right-sidebar")
                                    .icon(IconName::PanelRight)
                                    .selected(self.right_sidebar_visible)
                                    .accessibility_label("Toggle right sidebar")
                                    .tooltip("Show or hide right sidebar")
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation()
                                    })
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        cx.stop_propagation();
                                        view.right_sidebar_visible = !view.right_sidebar_visible;
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .child(div().flex_1().min_h_0().child(workspace))
            .when(self.bottom_bar_visible, |view| {
                view.child(
                    div()
                        .id("bottom-bar")
                        .test_support()
                        .flex_none()
                        .child(StatusBar::new().h(rems(1.5))),
                )
            })
    }
}

fn main() {
    let loaded = Settings::path().and_then(|path| {
        Settings::load(&path)
            .map(|settings| (path.clone(), settings))
            .map_err(|error| {
                std::io::Error::new(error.kind(), format!("{}: {error}", path.display()))
            })
    });
    let (settings_path, settings, startup_error) = match loaded {
        Ok((path, settings)) => (Some(path), settings, None),
        Err(error) => (
            None,
            Settings::default(),
            Some(format!(
                "Couldn’t load settings: {error}. Existing files are preserved and saving is disabled. Fix the file or folder permissions, then restart the app."
            )),
        ),
    };
    let (window_path, restored) = match WindowState::load() {
        Ok(loaded) => loaded,
        Err(error) => {
            eprintln!(
                "Couldn’t load window geometry: {error}. Existing file is preserved; geometry saving is disabled."
            );
            (None, None)
        }
    };
    let window_handle = std::rc::Rc::new(std::cell::Cell::new(None::<AnyWindowHandle>));
    let application = gpui_kit::application().with_assets(Assets);
    application.on_reopen({
        let window_handle = window_handle.clone();
        move |cx| {
            if let Some(handle) = window_handle.get() {
                cx.defer(move |cx| {
                    if let Err(error) = cx.update_window(handle, |_, window, cx| {
                        window.activate_window();
                        cx.activate(true);
                    }) {
                        eprintln!("Couldn’t reactivate the app window: {error}");
                    }
                });
            }
        }
    });
    application.run(move |cx| {
        gpui_kit::init(cx);
        cx.set_global(settings);
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let options = WindowOptions {
            window_bounds: Some(WindowState::window_bounds(restored.as_ref(), cx)),
            ..TitleBar::window_options()
        };
        let (handle, _view) = gpui_kit::open_window(options, cx, |window, cx| {
            Theme::sync_system_appearance(Some(window), cx);
            window
                .observe_window_appearance(|window, cx| {
                    Theme::sync_system_appearance(Some(window), cx);
                })
                .detach();
            cx.new(|cx| AppView::new(settings_path.clone(), window_path, restored, window, cx))
        })
        .expect("Failed to open app window");
        window_handle.set(Some(handle));
        #[cfg(debug_assertions)]
        _view.update(cx, |view, cx| {
            view.connections
                .update(cx, |connections, cx| connections.open_debug_table(cx));
        });
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-,", OpenSettings, None),
            #[cfg(not(target_os = "macos"))]
            KeyBinding::new("ctrl-q", Quit, None),
            #[cfg(not(target_os = "macos"))]
            KeyBinding::new("ctrl-,", OpenSettings, None),
        ]);
        cx.on_action(|_: &Quit, cx| cx.quit());
        settings_view::register_action(handle, settings_path, startup_error.clone(), cx);
        cx.set_menus(vec![Menu {
            name: env!("CARGO_PKG_NAME").into(),
            disabled: false,
            items: vec![
                MenuItem::action("Settings…", OpenSettings),
                MenuItem::separator(),
                MenuItem::action("Quit", Quit),
            ],
        }]);
        if let Some(error) = startup_error {
            let _ = cx.update_window(handle, |_, window, cx| {
                settings_view::show_error(error, window, cx);
            });
        }
        cx.activate(true);
    });
}

#[cfg(test)]
mod tests {
    use super::AppView;
    use gpui_kit::{
        AppContext, TestAppContext,
        component::{Root, WindowExt},
        point, px, size,
        test::TestWindowExt,
    };

    #[cfg(debug_assertions)]
    #[gpui_kit::test]
    #[ignore = "requires a temporary MariaDB server with apps.apps and TABLELANE_TEST_MARIADB_PORT"]
    fn debug_startup_opens_apps_table(cx: &mut TestAppContext) {
        use crate::settings::{Connection, DatabaseKind, Settings};
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(Settings {
                connections: vec![Connection {
                    name: "Local".into(),
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
                }],
                ..Settings::default()
            });
        });
        let mut view = None;
        let handle = cx.open_window(size(px(1000.), px(600.)), |window, cx| {
            let app = cx.new(|cx| AppView::new(None, None, None, window, cx));
            view = Some(app.clone());
            Root::new(app, window, cx)
        });
        cx.update(|cx| {
            view.unwrap().update(cx, |view, cx| {
                view.connections
                    .update(cx, |connections, cx| connections.open_debug_table(cx));
            });
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            cx.run_until_parked();
            let ready = cx
                .update_window(handle.into(), |_, window, cx| {
                    window.render_frame(cx);
                    window.try_find("table").is_some()
                })
                .unwrap();
            if ready {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "debug table never loaded"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(
                window.find("table-title").label(),
                Some("Local / apps / apps")
            );
            assert!(
                window
                    .find("connection-1/database/apps/table/apps")
                    .visible()
            );
        })
        .unwrap();
    }

    #[gpui_kit::test]
    #[ignore = "requires a temporary MariaDB server and TABLELANE_TEST_MARIADB_PORT"]
    fn mariadb_table_double_click_and_enter_open_content(cx: &mut TestAppContext) {
        use crate::settings::{Connection, DatabaseKind, Settings};
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(Settings {
                connections: vec![Connection {
                    name: "Local".into(),
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
                }],
                ..Settings::default()
            });
        });
        let handle = cx.open_window(size(px(1000.), px(600.)), |window, cx| {
            let view = cx.new(|cx| AppView::new(None, None, None, window, cx));
            Root::new(view, window, cx)
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.double_click("connection-1", cx)
        })
        .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            cx.run_until_parked();
            let ready = cx
                .update_window(handle.into(), |_, window, cx| {
                    window.render_frame(cx);
                    window.try_find("connection-1-connected").is_some()
                })
                .unwrap();
            if ready {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "connection never completed"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let table = "connection-1/database/tablelane_test/table/widgets";
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("connection-1/database/tablelane_test", cx);
            window.click(table, cx);
            assert!(window.try_find("table-title").is_none());
            window.double_click(table, cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(
                window.find("table-title").label(),
                Some("Local / tablelane_test / widgets")
            );
        })
        .unwrap();
        loop {
            cx.run_until_parked();
            let ready = cx
                .update_window(handle.into(), |_, window, cx| {
                    window.render_frame(cx);
                    window.try_find("table").is_some()
                })
                .unwrap();
            if ready {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "table never loaded");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        cx.update_window(handle.into(), |_, window, cx| {
            let content = window.find("content").bounds();
            let grid = window.find("table").bounds();
            assert!(grid.top() > content.top());
            assert_eq!(grid.left(), content.left() + window.rem_size() * 0.75);
            assert_eq!(grid.right(), content.right());
            assert!(grid.bottom() <= content.bottom());
            assert!(window.try_find("table-status").is_none());
            window.click(table, cx);
            window.press("enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(
                window.find("table-title").label(),
                Some("Local / tablelane_test / widgets")
            );
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn add_connection_selects_databases_and_dismisses_dialog(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let handle = cx.open_window(size(px(800.), px(600.)), |window, cx| {
            let view = cx.new(|cx| AppView::new(None, None, None, window, cx));
            Root::new(view, window, cx)
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            assert!(!window.has_active_dialog(cx));
            let button = window.find("add-connection").bounds();
            let sidebar = window.find("sidebar").bounds();
            assert!(button.top() >= sidebar.top());
            assert!(button.bottom() < sidebar.center().y);
            assert!(button.left() >= sidebar.left() && button.right() <= sidebar.right());
            window.click("add-connection", cx);
            assert!(window.has_active_dialog(cx));
            assert!(window.find("dialog").visible());
            assert_eq!(window.find("MySQL").checked(), Some(true));
            let list = window.find("database-list").bounds();
            assert!(list.size.width < window.find("dialog").bounds().size.width / 2.);
            for database in ["MariaDB", "MongoDB", "SQLite", "PostgreSQL", "MySQL"] {
                assert!(window.find(database).visible());
                window.click(database, cx);
                assert_eq!(window.find(database).checked(), Some(true));
            }
            assert_eq!(window.find("PostgreSQL").checked(), Some(false));
            window.press("escape", cx);
            assert!(!window.has_active_dialog(cx));
            for _ in 0..5 {
                if window.find("add-connection").focused() == Some(true) {
                    break;
                }
                window.focus_next(cx);
                window.render_frame(cx);
            }
            assert_eq!(window.find("add-connection").focused(), Some(true));
            window.press("enter", cx);
            assert!(window.has_active_dialog(cx));
            assert_eq!(window.find("MySQL").checked(), Some(true));
            for _ in 0..7 {
                if window.find("SQLite").focused() == Some(true) {
                    break;
                }
                window.focus_next(cx);
                window.render_frame(cx);
            }
            assert_eq!(window.find("SQLite").focused(), Some(true));
            window.press("space", cx);
            assert_eq!(window.find("SQLite").checked(), Some(true));
            assert_eq!(window.find("MySQL").checked(), Some(false));
            window.within("dialog").click("close", cx);
            assert!(!window.has_active_dialog(cx));
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn sidebar_resizes_by_dragging_its_divider(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let handle = cx.open_window(size(px(800.), px(600.)), |window, cx| {
            let view = cx.new(|cx| AppView::new(None, None, None, window, cx));
            Root::new(view, window, cx)
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            let sidebar = window.find("sidebar").bounds();
            let toolbar = window.find("title-bar").bounds();
            assert_eq!(toolbar.top(), px(0.));
            assert_eq!(toolbar.size.width, px(800.));
            assert_eq!(sidebar.top(), toolbar.bottom());
            assert_eq!(sidebar.left(), px(0.));
            assert!((sidebar.size.width - px(240.)).abs() <= px(1.));
            let divider = point(sidebar.right(), sidebar.center().y);
            window.drag(divider, divider + point(px(80.), px(0.)), cx);
            let resized = window.find("sidebar").bounds();
            assert!(resized.size.width > sidebar.size.width + px(60.));
            assert!(window.find("content").bounds().left() >= resized.right());
            let divider = point(resized.right(), resized.center().y);
            window.drag(divider, divider + point(px(500.), px(0.)), cx);
            assert!(window.find("sidebar").bounds().size.width <= px(400.));
            let sidebar = window.find("sidebar").bounds();
            let divider = point(sidebar.right(), sidebar.center().y);
            window.drag(divider, point(px(0.), divider.y), cx);
            assert!(window.find("sidebar").bounds().size.width >= px(160.));
            window.click("toggle-sidebar", cx);
            assert!(window.try_find("sidebar").is_none());
            assert_eq!(window.find("content").bounds().left(), px(0.));
            window.click("toggle-sidebar", cx);
            assert!(window.find("sidebar").visible());
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn layout_controls_toggle_bars_and_right_sidebar_resizes(cx: &mut TestAppContext) {
        use gpui_kit::AssetSource;
        for path in ["icons/panel-bottom.svg", "icons/panel-right.svg"] {
            assert!(!super::Assets.load(path).unwrap().unwrap().is_empty());
        }
        cx.update(gpui_kit::init);
        let handle = cx.open_window(size(px(1000.), px(600.)), |window, cx| {
            let view = cx.new(|cx| AppView::new(None, None, None, window, cx));
            Root::new(view, window, cx)
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            assert!(window.try_find("right-sidebar").is_none());
            let bottom = window.find("bottom-bar").bounds();
            assert_eq!(bottom.bottom(), px(600.));
            assert_eq!(bottom.size.width, px(1000.));
            assert_eq!(window.find("content").bounds().bottom(), bottom.top());
            let left = window.find("toggle-sidebar").bounds();
            let bottom_toggle = window.find("toggle-bottom-bar").bounds();
            let right = window.find("toggle-right-sidebar").bounds();
            assert_eq!(left.top(), right.top());
            assert_eq!(left.right(), bottom_toggle.left());
            assert_eq!(bottom_toggle.right(), right.left());
            assert!(right.right() > px(950.));

            window.click("toggle-right-sidebar", cx);
            let sidebar = window.find("right-sidebar").bounds();
            assert!((sidebar.size.width - px(240.)).abs() <= px(1.));
            assert_eq!(sidebar.right(), px(1000.));
            assert_eq!(sidebar.bottom(), bottom.top());
            let divider = point(sidebar.left(), sidebar.center().y);
            window.drag(divider, divider - point(px(80.), px(0.)), cx);
            let resized = window.find("right-sidebar").bounds();
            assert!(resized.size.width > sidebar.size.width + px(60.));
            assert!(window.find("content").bounds().right() <= resized.left());
            let divider = point(resized.left(), resized.center().y);
            window.drag(divider, divider - point(px(500.), px(0.)), cx);
            assert!(window.find("right-sidebar").bounds().size.width <= px(400.));
            let sidebar = window.find("right-sidebar").bounds();
            let divider = point(sidebar.left(), sidebar.center().y);
            window.drag(divider, point(px(1000.), divider.y), cx);
            assert!(window.find("right-sidebar").bounds().size.width >= px(160.));
            let width = window.find("right-sidebar").bounds().size.width;
            window.click("toggle-right-sidebar", cx);
            assert!(window.try_find("right-sidebar").is_none());
            assert_eq!(window.find("content").bounds().right(), px(1000.));
            for _ in 0..4 {
                if window.find("toggle-right-sidebar").focused() == Some(true) {
                    break;
                }
                window.focus_next(cx);
                window.render_frame(cx);
            }
            assert_eq!(window.find("toggle-right-sidebar").focused(), Some(true));
            window.press("enter", cx);
            assert!(
                window.try_find("right-sidebar").is_some(),
                "keyboard did not reopen sidebar"
            );
            assert_eq!(window.find("right-sidebar").bounds().size.width, width);

            window.click("toggle-bottom-bar", cx);
            assert!(window.try_find("bottom-bar").is_none());
            assert_eq!(window.find("content").bounds().bottom(), px(600.));
            window.focus_prev(cx);
            window.render_frame(cx);
            assert_eq!(window.find("toggle-bottom-bar").focused(), Some(true));
            window.press("space", cx);
            assert_eq!(window.find("bottom-bar").bounds(), bottom);
            assert!(window.find("sidebar").visible());
            assert!(window.find("right-sidebar").visible());
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn toolbar_double_clicks_do_not_bubble_to_window_chrome(cx: &mut TestAppContext) {
        use gpui_kit::{
            InteractiveElement, ParentElement, StatefulInteractiveElement, Styled, TestSupportExt,
            div,
        };
        use std::{cell::Cell, rc::Rc};

        struct Chrome {
            view: gpui_kit::Entity<AppView>,
            double_clicks: Rc<Cell<usize>>,
        }
        impl gpui_kit::Render for Chrome {
            fn render(
                &mut self,
                _: &mut gpui_kit::Window,
                _: &mut gpui_kit::Context<Self>,
            ) -> impl gpui_kit::IntoElement {
                let double_clicks = self.double_clicks.clone();
                div()
                    .id("window-chrome")
                    .test_support()
                    .size_full()
                    .on_click(move |_, _, _| {
                        double_clicks.set(double_clicks.get() + 1);
                    })
                    .child(self.view.clone())
            }
        }
        cx.update(gpui_kit::init);
        let double_clicks = Rc::new(Cell::new(0));
        let handle = cx.open_window(size(px(1000.), px(600.)), |window, cx| {
            let view = cx.new(|cx| AppView::new(None, None, None, window, cx));
            let chrome = cx.new(|_| Chrome {
                view,
                double_clicks: double_clicks.clone(),
            });
            Root::new(chrome, window, cx)
        });
        cx.update_window(handle.into(), |_, window, cx| {
            for id in [
                "toggle-sidebar",
                "toggle-bottom-bar",
                "toggle-right-sidebar",
                "toolbar-settings",
            ] {
                window.double_click(id, cx);
                assert_eq!(double_clicks.get(), 0, "{id} leaked a double click");
            }
            assert!(window.find("sidebar").visible());
            assert!(window.find("bottom-bar").visible());
            assert!(window.try_find("right-sidebar").is_none());
            window.click("title-bar", cx);
            assert_eq!(double_clicks.get(), 1);
        })
        .unwrap();
    }
}
