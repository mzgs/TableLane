mod connection_dialog;
mod connections;
mod data_export;
mod database;
mod schema_view;
mod settings;
mod settings_view;
mod sql_editor;
mod table_view;
mod window_state;
mod workspace_state;

use gpui_kit::component::{
    ActiveTheme, Disableable, IconName, Selectable, Sizable, Theme, TitleBar, WindowExt,
    button::{Button, ButtonGroup, ButtonVariants},
    resizable::{h_resizable, resizable_panel},
    scroll::ScrollableElement,
    status_bar::StatusBar,
    tab::{Tab, TabBar},
    table::TableEvent,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use settings::Settings;
use window_state::WindowState;
use workspace_state::{SavedTab, WorkspaceFile, WorkspaceState};

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

actions!(
    base_app,
    [Quit, OpenSettings, CloseTab, NextTab, PreviousTab]
);

enum WorkspaceTab {
    Table(Entity<table_view::TableView>),
    Sql(Entity<sql_editor::SqlEditor>, usize),
    Structure(Entity<schema_view::SchemaView>),
}

impl WorkspaceTab {
    fn id(&self) -> EntityId {
        match self {
            Self::Table(table) => table.entity_id(),
            Self::Sql(editor, _) => editor.entity_id(),
            Self::Structure(view) => view.entity_id(),
        }
    }

    fn request<'a>(&'a self, cx: &'a App) -> Option<&'a connections::OpenTable> {
        match self {
            Self::Table(table) => table.read(cx).request(),
            Self::Sql(editor, _) => editor.read(cx).request(),
            Self::Structure(view) => Some(view.read(cx).request()),
        }
    }

    fn label(&self, cx: &App) -> String {
        match self {
            Self::Structure(view) => {
                let view = view.read(cx);
                let request = view.request();
                format!(
                    "{} / {} / {} · Structure{}",
                    request.connection_name,
                    request.database,
                    request.table,
                    if view.is_running() { " · Running" } else { "" }
                )
            }
            Self::Table(table) => format!(
                "{}{}",
                table.read(cx).title(),
                if table.read(cx).is_dirty(cx) {
                    " · Edited"
                } else {
                    ""
                }
            ),
            Self::Sql(editor, number) => {
                let view = editor.read(cx);
                format!(
                    "{} / {} / SQL {number}{}",
                    view.connection_name(),
                    view.database(),
                    if view.is_running() {
                        " · Running"
                    } else if view.has_draft(cx) {
                        " · Draft"
                    } else {
                        ""
                    }
                )
            }
        }
    }
}

struct AppView {
    connections: Entity<connections::Connections>,
    tabs: Vec<WorkspaceTab>,
    active_tab: usize,
    next_sql_number: usize,
    tab_scroll: ScrollHandle,
    focus: FocusHandle,
    sidebar_visible: bool,
    right_sidebar_visible: bool,
    bottom_bar_visible: bool,
    settings_path: Option<std::path::PathBuf>,
    window_path: Option<std::path::PathBuf>,
    window_state: WindowState,
    workspace: WorkspaceFile,
    workspace_task: Option<Task<()>>,
}

impl AppView {
    fn new(
        settings_path: Option<std::path::PathBuf>,
        window_path: Option<std::path::PathBuf>,
        restored: Option<WindowState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.bind_keys([
            KeyBinding::new("cmd-s", table_view::SaveRow, Some("TableWorkspace")),
            KeyBinding::new("escape", table_view::CancelRow, Some("RowEditor")),
            KeyBinding::new("cmd-w", CloseTab, Some("TableWorkspace")),
            KeyBinding::new("ctrl-w", CloseTab, Some("TableWorkspace")),
            KeyBinding::new("ctrl-tab", NextTab, Some("TableWorkspace")),
            KeyBinding::new("ctrl-shift-tab", PreviousTab, Some("TableWorkspace")),
        ]);
        let mut window_state = restored.unwrap_or_else(|| WindowState::capture(window));
        window_state.update(window);
        cx.observe_window_bounds(window, |view, window, _| view.window_state.update(window))
            .detach();
        cx.on_app_quit(|view, cx| {
            view.save_workspace(cx);
            view.save_window_state();
            async {}
        })
        .detach();
        let weak = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            weak.update(cx, |view, cx| {
                view.window_state.update(window);
                view.save_window_state();
                view.prepare_close(false, window, cx)
            })
            .unwrap_or(true)
        });
        let connections = cx.new(connections::Connections::new);
        cx.observe_in(&connections, window, |view, _, _, cx| {
            view.sync_session_health(cx);
            cx.notify();
        })
        .detach();
        cx.subscribe_in(
            &connections,
            window,
            |view, _, event: &connections::OpenTable, window, cx| {
                view.open_table(event, window, cx);
            },
        )
        .detach();
        cx.subscribe_in(
            &connections,
            window,
            |view, _, ready: &connections::SessionReady, window, cx| {
                view.attach_session(ready, window, cx);
            },
        )
        .detach();
        cx.subscribe_in(
            &connections,
            window,
            |view, _, command: &connections::ManageConnection, window, cx| {
                view.manage_connection(command, window, cx);
            },
        )
        .detach();
        let (workspace, saved) = WorkspaceFile::load(
            settings_path
                .as_ref()
                .map(|path| path.with_file_name("workspace.json")),
        );
        let mut view = Self {
            connections,
            tabs: Vec::new(),
            active_tab: 0,
            next_sql_number: 1,
            tab_scroll: ScrollHandle::new(),
            focus: cx.focus_handle(),
            sidebar_visible: true,
            right_sidebar_visible: false,
            bottom_bar_visible: true,
            settings_path,
            window_path,
            window_state,
            workspace,
            workspace_task: None,
        };
        for tab in saved.tabs {
            match tab {
                SavedTab::Sql(draft) => {
                    let number = draft.number;
                    let editor = cx.new(|cx| sql_editor::SqlEditor::restore(draft, window, cx));
                    view.observe_sql(&editor, cx);
                    view.tabs.push(WorkspaceTab::Sql(editor, number));
                    view.next_sql_number = view.next_sql_number.max(number.saturating_add(1));
                }
                SavedTab::Table(draft) => {
                    let table = cx.new(|_| table_view::TableView::restore(draft));
                    view.set_table(table, window, cx);
                }
            }
        }
        view.select_tab(saved.active, window, cx);
        view
    }

    fn capture_workspace(&self, cx: &App) -> WorkspaceState {
        let mut state = WorkspaceState::default();
        for (ix, tab) in self.tabs.iter().enumerate() {
            let saved = match tab {
                WorkspaceTab::Sql(editor, number) => {
                    let mut draft = editor.read(cx).draft(cx);
                    draft.number = *number;
                    Some(SavedTab::Sql(draft))
                }
                WorkspaceTab::Table(table) => table.read(cx).draft().map(SavedTab::Table),
                WorkspaceTab::Structure(_) => None,
            };
            if let Some(saved) = saved {
                if ix == self.active_tab {
                    state.active = state.tabs.len();
                }
                state.tabs.push(saved);
            }
        }
        state
    }

    fn save_workspace(&mut self, cx: &mut Context<Self>) -> bool {
        self.workspace_task = None;
        let state = self.capture_workspace(cx);
        let saved = self.workspace.save(&state);
        cx.notify();
        saved
    }

    fn schedule_workspace_save(&mut self, cx: &mut Context<Self>) {
        self.workspace_task = Some(cx.spawn(async move |view, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(300))
                .await;
            let _ = view.update(cx, |view, cx| {
                view.save_workspace(cx);
            });
        }));
    }

    fn observe_sql(&self, editor: &Entity<sql_editor::SqlEditor>, cx: &mut Context<Self>) {
        cx.observe(editor, |view, editor, cx| {
            view.connections
                .update(cx, |connections, cx| connections.refresh_health(cx));
            if !editor.read(cx).is_busy()
                && editor
                    .read(cx)
                    .request()
                    .is_some_and(|request| !request.session.is_connected())
            {
                editor.update(cx, |editor, cx| editor.detach(cx));
            }
            cx.notify();
        })
        .detach();
        cx.subscribe(editor, |view, _, _: &sql_editor::DraftChanged, cx| {
            view.schedule_workspace_save(cx)
        })
        .detach();
    }

    fn sync_session_health(&mut self, cx: &mut Context<Self>) {
        for tab in &self.tabs {
            if tab
                .request(cx)
                .is_some_and(|request| !request.session.is_connected())
            {
                match tab {
                    WorkspaceTab::Sql(editor, _) if !editor.read(cx).is_busy() => {
                        editor.update(cx, |editor, cx| editor.detach(cx))
                    }
                    WorkspaceTab::Table(table) if table.read(cx).can_change_schema(cx) => {
                        table.update(cx, |table, cx| table.detach(cx))
                    }
                    WorkspaceTab::Table(table) => table.update(cx, |_, cx| cx.notify()),
                    WorkspaceTab::Structure(view) => view.update(cx, |_, cx| cx.notify()),
                    _ => {}
                }
            }
        }
    }

    fn attach_session(
        &mut self,
        ready: &connections::SessionReady,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sync_session_health(cx);
        for tab in &self.tabs {
            match tab {
                WorkspaceTab::Sql(editor, _) => {
                    if editor.read(cx).request().is_none() {
                        editor.update(cx, |editor, cx| {
                            editor.attach(ready.session.clone(), ready.name.clone(), cx)
                        });
                    } else if editor.read(cx).request().is_some_and(|request| {
                        std::sync::Arc::ptr_eq(&request.session, &ready.session)
                    }) {
                        editor.update(cx, |editor, cx| editor.refresh_completions(cx));
                    }
                }
                WorkspaceTab::Table(table) if table.read(cx).request().is_none() => {
                    table.update(cx, |table, cx| {
                        table.attach(ready.session.clone(), ready.name.clone(), window, cx)
                    });
                }
                WorkspaceTab::Structure(view)
                    if view.read(cx).request().session.connection_key()
                        == ready.session.connection_key() =>
                {
                    view.update(cx, |view, cx| {
                        view.attach(ready.session.clone(), ready.name.clone(), window, cx)
                    });
                }
                _ => {}
            }
        }
    }

    fn connection_in_use(&self, session: &std::sync::Arc<database::Session>, cx: &App) -> bool {
        self.tabs.iter().any(|tab| {
            tab.request(cx)
                .is_some_and(|request| std::sync::Arc::ptr_eq(&request.session, session))
                && match tab {
                    WorkspaceTab::Table(table) => !table.read(cx).can_change_schema(cx),
                    WorkspaceTab::Sql(editor, _) => editor.read(cx).is_busy(),
                    WorkspaceTab::Structure(view) => view.read(cx).is_busy(),
                }
        })
    }

    fn manage_connection(
        &mut self,
        command: &connections::ManageConnection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.has_active_dialog(cx) || !self.connections.read(cx).is_manageable(command) {
            return;
        }
        if let Some(session) = self.connections.read(cx).managed_session(&command.id)
            && session.is_connected()
        {
            if self.connection_in_use(&session, cx) {
                self.finish_connection_change(command, window, cx);
                return;
            }
            let command = command.clone();
            let view = cx.entity().downgrade();
            window.open_alert_dialog(cx, move |dialog, _, _| {
                let view = view.clone();
                let command = command.clone();
                let session = session.clone();
                dialog.confirm()
                    .title(if command.reconnect { "Reconnect this session?" } else { "Disconnect this session?" })
                    .description("Closing the database session rolls back uncommitted SQL transactions and removes temporary tables and session variables. SQL drafts, completed SQL results, and table filters stay available.")
                    .ok_text(if command.reconnect { "Reconnect" } else { "Disconnect" })
                    .on_ok(move |_, window, cx| {
                        view.update(cx, |view, cx| {
                            if view.connections.read(cx).managed_session(&command.id).is_some_and(|current| std::sync::Arc::ptr_eq(&current, &session)) {
                                view.finish_connection_change(&command, window, cx)
                            } else { true }
                        }).unwrap_or(true)
                    })
            });
            return;
        }
        self.finish_connection_change(command, window, cx);
    }

    fn finish_connection_change(
        &mut self,
        command: &connections::ManageConnection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.connections.read(cx).is_manageable(command) {
            return false;
        }
        if let Some(session) = self.connections.read(cx).managed_session(&command.id) {
            if self.connection_in_use(&session, cx) {
                if window.has_active_dialog(cx) {
                    return false;
                }
                window.open_alert_dialog(cx, |dialog, _, _| dialog.title("Connection is in use")
                    .description("Save or cancel row edits and wait for queries, reads, schema changes, and exports on this connection to finish, then try again."));
                return false;
            }
            session.disconnect();
            cx.background_spawn(async move {
                async_std::task::block_on(session.close());
            })
            .detach();
            self.sync_session_health(cx);
        }
        self.connections
            .update(cx, |connections, cx| connections.manage(command, cx));
        self.schedule_workspace_save(cx);
        cx.notify();
        true
    }

    fn has_running_work(&self, cx: &App) -> bool {
        self.connections.read(cx).is_connecting()
            || self.tabs.iter().any(|tab| match tab {
                WorkspaceTab::Table(table) => table.read(cx).is_busy(),
                WorkspaceTab::Sql(editor, _) => editor.read(cx).is_busy(),
                WorkspaceTab::Structure(view) => view.read(cx).is_busy(),
            })
    }

    fn prepare_close(
        &mut self,
        quitting: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let saved = self.save_workspace(cx);
        if window.has_active_dialog(cx) {
            return false;
        }
        if self.has_running_work(cx) {
            window.open_alert_dialog(cx, |dialog, _, _| {
                dialog.title("Wait for running operations")
                    .description("A connection, query, row save, schema change, or export is still running. Wait for it to finish, then close again.")
            });
            return false;
        }
        let dirty = self
            .tabs
            .iter()
            .filter(|tab| matches!(tab, WorkspaceTab::Table(table) if table.read(cx).is_dirty(cx)))
            .count();
        let unsaved_workspace = !saved && !self.capture_workspace(cx).tabs.is_empty();
        if dirty == 0 && !unsaved_workspace {
            return true;
        }
        let mut description = String::new();
        if dirty > 0 {
            description = format!(
                "Unsaved row edits in {dirty} table tab(s) will be discarded. Cancel to save or cancel those edits first."
            );
        }
        if unsaved_workspace {
            description.push_str(&format!(
                " {} Closing can lose the latest SQL drafts and table workspace state.",
                self.workspace.error().cloned().unwrap_or_default()
            ));
        }
        let view = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let view = view.clone();
            dialog
                .confirm()
                .title(if dirty > 0 {
                    "Discard row edits and close?"
                } else {
                    "Close without saved workspace?"
                })
                .description(description.clone())
                .ok_text(if dirty > 0 {
                    "Discard edits and close"
                } else {
                    "Close without saving"
                })
                .ok_variant(gpui_kit::component::button::ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    view.update(cx, |view, cx| {
                        if view.has_running_work(cx) {
                            return false;
                        }
                        view.save_workspace(cx);
                        if quitting {
                            cx.quit();
                        } else {
                            window.remove_window();
                        }
                        true
                    })
                    .unwrap_or(true)
                })
        });
        false
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

    fn set_table(
        &mut self,
        table: Entity<table_view::TableView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.observe(&table, |view, table, cx| {
            view.connections
                .update(cx, |connections, cx| connections.refresh_health(cx));
            if table.read(cx).can_change_schema(cx)
                && table
                    .read(cx)
                    .request()
                    .is_some_and(|request| !request.session.is_connected())
            {
                table.update(cx, |table, cx| table.detach(cx));
            }
            cx.notify();
        })
        .detach();
        cx.subscribe(&table, |view, _, _: &table_view::QueryChanged, cx| {
            view.schedule_workspace_save(cx);
        })
        .detach();
        cx.subscribe(&table, |view, table, event: &TableEvent, cx| {
            if matches!(
                event,
                TableEvent::SelectRow(_) | TableEvent::SelectCell(_, _)
            ) && view.table().is_some_and(|active| active == &table)
            {
                view.right_sidebar_visible = true;
            }
            cx.notify();
        })
        .detach();
        self.tabs.push(WorkspaceTab::Table(table));
        self.select_tab(self.tabs.len() - 1, window, cx);
    }

    fn table(&self) -> Option<&Entity<table_view::TableView>> {
        match self.tabs.get(self.active_tab) {
            Some(WorkspaceTab::Table(table)) => Some(table),
            _ => None,
        }
    }

    fn select_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        self.active_tab = ix;
        self.schedule_workspace_save(cx);
        self.tab_scroll.scroll_to_item(ix);
        self.focus.focus(window, cx);
        match &self.tabs[ix] {
            WorkspaceTab::Table(table) => table.update(cx, |table, cx| table.focus(window, cx)),
            WorkspaceTab::Sql(editor, _) => {
                editor.update(cx, |editor, cx| editor.focus(window, cx))
            }
            WorkspaceTab::Structure(view) => view.update(cx, |view, cx| view.focus(window, cx)),
        }
        cx.notify();
    }

    fn cycle_tab(&mut self, backwards: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.tabs.is_empty() {
            let step = if backwards { self.tabs.len() - 1 } else { 1 };
            self.select_tab((self.active_tab + step) % self.tabs.len(), window, cx);
        }
    }

    fn open_table(
        &mut self,
        request: &connections::OpenTable,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let existing = self.tabs.iter().position(|tab| {
            matches!(tab, WorkspaceTab::Table(_))
                && tab.request(cx).is_some_and(|open| {
                    std::sync::Arc::ptr_eq(&open.session, &request.session)
                        && open.database == request.database
                        && open.table == request.table
                })
        });
        if let Some(ix) = existing {
            self.select_tab(ix, window, cx);
        } else {
            let table = cx.new(|cx| table_view::TableView::new(request.clone(), window, cx));
            self.set_table(table, window, cx);
        }
    }

    fn open_sql_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(request) = self
            .tabs
            .get(self.active_tab)
            .and_then(|tab| tab.request(cx))
            .cloned()
        else {
            return;
        };
        self.open_sql_draft(request, None, window, cx);
    }

    fn open_sql_draft(
        &mut self,
        request: connections::OpenTable,
        sql: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editor = cx.new(|cx| sql_editor::SqlEditor::new(request, window, cx));
        if let Some(sql) = sql {
            editor.update(cx, |editor, cx| editor.set_sql(sql, window, cx));
        }
        self.observe_sql(&editor, cx);
        self.tabs
            .push(WorkspaceTab::Sql(editor, self.next_sql_number));
        self.next_sql_number = self.next_sql_number.saturating_add(1);
        self.select_tab(self.tabs.len() - 1, window, cx);
    }

    fn open_structure(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(request) = self
            .tabs
            .get(self.active_tab)
            .and_then(|tab| tab.request(cx))
            .cloned()
        else {
            return;
        };
        if let Some(ix) = self.tabs.iter().position(|tab| {
            matches!(tab, WorkspaceTab::Structure(_))
                && tab.request(cx).is_some_and(|open| {
                    std::sync::Arc::ptr_eq(&open.session, &request.session)
                        && open.database == request.database
                        && open.table == request.table
                })
        }) {
            self.select_tab(ix, window, cx);
            return;
        }
        let structure = cx.new(|cx| schema_view::SchemaView::new(request, window, cx));
        cx.observe(&structure, |view, _, cx| {
            view.connections
                .update(cx, |connections, cx| connections.refresh_health(cx));
            cx.notify();
        })
        .detach();
        cx.subscribe_in(&structure, window, |view, structure, event, window, cx| {
            let request = structure.read(cx).request().clone();
            let tables: Vec<_> = view.tabs.iter().filter_map(|tab| match tab {
                WorkspaceTab::Table(table) if table.read(cx).request().is_some_and(|open|
                    std::sync::Arc::ptr_eq(&open.session, &request.session) && open.database == request.database && open.table == request.table) => Some(table.clone()),
                _ => None,
            }).collect();
            match event {
                schema_view::SchemaEvent::Apply(sql) => {
                    if tables.iter().any(|table| !table.read(cx).can_change_schema(cx)) {
                        structure.update(cx, |structure, cx| structure.reject_change("Save or cancel row edits and wait for table operations to finish before changing its structure.".into(), cx));
                        return;
                    }
                    for table in tables { table.update(cx, |table, cx| table.begin_schema_change(cx)); }
                    structure.update(cx, |structure, cx| structure.apply(sql.clone(), window, cx));
                }
                schema_view::SchemaEvent::Changed(changed) => {
                    for table in tables { table.update(cx, |table, cx| table.finish_schema_change(*changed, window, cx)); }
                }
                schema_view::SchemaEvent::EditSql(sql) => view.open_sql_draft(request, Some(sql.clone()), window, cx),
            }
        }).detach();
        self.tabs.push(WorkspaceTab::Structure(structure));
        self.select_tab(self.tabs.len() - 1, window, cx);
    }

    fn close_tab(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.tabs.iter().position(|tab| tab.id() == id) else {
            return;
        };
        match &self.tabs[ix] {
            WorkspaceTab::Structure(view) => {
                if view.read(cx).is_busy() {
                    return;
                }
            }
            WorkspaceTab::Table(table) => {
                if !table.update(cx, |table, cx| table.prepare_to_close(cx)) {
                    self.select_tab(ix, window, cx);
                    self.right_sidebar_visible = true;
                    return;
                }
            }
            WorkspaceTab::Sql(editor, _) => {
                if editor.read(cx).is_busy() {
                    return;
                }
                if editor.read(cx).has_draft(cx) {
                    let view = cx.entity().downgrade();
                    let label = self.tabs[ix].label(cx);
                    window.open_alert_dialog(cx, move |dialog, _, _| {
                        let view = view.clone();
                        dialog
                            .confirm()
                            .title("Discard SQL draft?")
                            .description(format!("Close {label} and discard its SQL text?"))
                            .ok_text("Discard draft")
                            .on_ok(move |_, window, cx| {
                                let _ = view.update(cx, |view, cx| view.remove_tab(id, window, cx));
                                true
                            })
                    });
                    return;
                }
            }
        }
        self.remove_tab(id, window, cx);
    }

    fn remove_tab(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.tabs.iter().position(|tab| tab.id() == id) else {
            return;
        };
        self.tabs.remove(ix);
        if ix < self.active_tab {
            self.active_tab -= 1;
        }
        self.active_tab = self.active_tab.min(self.tabs.len().saturating_sub(1));
        if !self.tabs.is_empty() {
            self.select_tab(self.active_tab, window, cx);
        } else {
            self.focus.focus(window, cx);
        }
        self.save_workspace(cx);
        cx.notify();
    }
}

impl Render for AppView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let sql_active = matches!(
            self.tabs.get(self.active_tab),
            Some(WorkspaceTab::Sql(..) | WorkspaceTab::Structure(..))
        );
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
                            .child(
                                Button::new("edit-connection")
                                    .ghost()
                                    .label("Edit connection…")
                                    .w_full()
                                    .disabled(
                                        self.settings_path.is_none()
                                            || self
                                                .connections
                                                .read(cx)
                                                .selected_connection(cx)
                                                .is_none(),
                                    )
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        if let Some(ix) =
                                            view.connections.read(cx).selected_connection(cx)
                                        {
                                            connection_dialog::edit(
                                                view.settings_path.clone(),
                                                ix,
                                                window,
                                                cx,
                                            );
                                        }
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
                        .flex()
                        .flex_col()
                        .when_some(self.workspace.error(), |content, error| {
                            content.child(
                                div()
                                    .px_3()
                                    .py_2()
                                    .flex()
                                    .flex_wrap()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .id("workspace-error")
                                            .test_support()
                                            .role(Role::Status)
                                            .aria_label(error.clone())
                                            .flex_1()
                                            .min_w_0()
                                            .text_sm()
                                            .text_color(cx.theme().danger)
                                            .child(error.clone()),
                                    )
                                    .child(
                                        Button::new("retry-workspace-save")
                                            .small()
                                            .label("Retry autosave")
                                            .disabled(self.workspace.is_readonly())
                                            .on_click(cx.listener(|view, _, _, cx| {
                                                view.save_workspace(cx);
                                            })),
                                    ),
                            )
                        })
                        .when(!self.tabs.is_empty(), |content| {
                            content.child(
                                TabBar::new("workspace-tabs")
                                    .small()
                                    .flex_shrink_0()
                                    .menu(true)
                                    .max_width(rems(20.).to_pixels(window.rem_size()))
                                    .track_scroll(&self.tab_scroll)
                                    .selected_index(self.active_tab)
                                    .children(self.tabs.iter().map(|tab| {
                                        let id = tab.id();
                                        let label = tab.label(cx);
                                        let running = match tab {
                                            WorkspaceTab::Sql(editor, _) => {
                                                editor.read(cx).is_running()
                                            }
                                            WorkspaceTab::Structure(view) => {
                                                view.read(cx).is_running()
                                            }
                                            _ => false,
                                        };
                                        Tab::new().label(label.clone()).suffix(
                                            Button::new(("close-tab", id))
                                                .ghost()
                                                .xsmall()
                                                .icon(IconName::Close)
                                                .accessibility_label(format!("Close {label}"))
                                                .tooltip(if running {
                                                    "Wait for the query to finish"
                                                } else {
                                                    "Close tab"
                                                })
                                                .disabled(running)
                                                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                                    cx.stop_propagation()
                                                })
                                                .on_click(cx.listener(
                                                    move |view, _, window, cx| {
                                                        cx.stop_propagation();
                                                        view.close_tab(id, window, cx);
                                                    },
                                                )),
                                        )
                                    }))
                                    .on_click(cx.listener(|view, ix, window, cx| {
                                        view.select_tab(*ix, window, cx)
                                    })),
                            )
                        })
                        .when_some(self.tabs.get(self.active_tab), |content, tab| {
                            let child = match tab {
                                WorkspaceTab::Table(table) => table.clone().into_any_element(),
                                WorkspaceTab::Sql(editor, _) => editor.clone().into_any_element(),
                                WorkspaceTab::Structure(view) => view.clone().into_any_element(),
                            };
                            content.child(
                                div()
                                    .id(("tab-content", tab.id()))
                                    .flex_1()
                                    .min_h_0()
                                    .min_w_0()
                                    .child(child),
                            )
                        }),
                ),
            );
        let workspace = workspace.child(
            resizable_panel()
                .visible(self.right_sidebar_visible && !sql_active)
                .size(px(240.))
                .size_range(px(160.)..px(400.))
                .flex_none()
                .child(
                    div()
                        .id("right-sidebar")
                        .test_support()
                        .key_context("RowEditor")
                        .size_full()
                        .bg(cx.theme().sidebar)
                        .child(
                            div()
                                .size_full()
                                .when_some(self.table(), |sidebar, table| {
                                    sidebar.child(
                                        table.update(cx, |table, cx| table.render_sidebar(cx)),
                                    )
                                })
                                .overflow_y_scrollbar(),
                        ),
                ),
        );
        div()
            .flex()
            .flex_col()
            .size_full()
            .track_focus(&self.focus)
            .key_context("TableWorkspace")
            .on_action(cx.listener(|view, _: &CloseTab, window, cx| {
                if let Some(tab) = view.tabs.get(view.active_tab) {
                    view.close_tab(tab.id(), window, cx);
                }
            }))
            .on_action(
                cx.listener(|view, _: &NextTab, window, cx| view.cycle_tab(false, window, cx)),
            )
            .on_action(
                cx.listener(|view, _: &PreviousTab, window, cx| view.cycle_tab(true, window, cx)),
            )
            .on_action(cx.listener(|view, _: &table_view::SaveRow, window, cx| {
                if let Some(table) = view.table() {
                    table.update(cx, |table, cx| table.save(window, cx));
                }
            }))
            .on_action(cx.listener(|view, _: &table_view::CancelRow, window, cx| {
                if let Some(table) = view.table() {
                    table.update(cx, |table, cx| table.cancel(window, cx));
                }
            }))
            .on_action(
                cx.listener(|view, _: &gpui_kit::component::input::Escape, window, cx| {
                    if let Some(table) = view.table() {
                        table.update(cx, |table, cx| table.cancel(window, cx));
                    }
                }),
            )
            .child(
                TitleBar::new().h_12().child(
                    div()
                        .flex()
                        .items_center()
                        .w_full()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("toolbar-structure")
                                .ghost()
                                .label("Structure")
                                .disabled(
                                    self.tabs
                                        .get(self.active_tab)
                                        .and_then(|tab| tab.request(cx))
                                        .is_none(),
                                )
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .on_click(cx.listener(|view, _, window, cx| {
                                    cx.stop_propagation();
                                    view.open_structure(window, cx);
                                })),
                        )
                        .child(
                            Button::new("toolbar-sql-editor")
                                .ghost()
                                .large()
                                .icon(IconName::SquareTerminal)
                                .accessibility_label("Open SQL editor")
                                .tooltip("Open SQL editor")
                                .disabled(
                                    self.tabs
                                        .get(self.active_tab)
                                        .and_then(|tab| tab.request(cx))
                                        .is_none(),
                                )
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .on_click(cx.listener(|view, _, window, cx| {
                                    cx.stop_propagation();
                                    view.open_sql_editor(window, cx);
                                })),
                        )
                        .child(
                            Button::new("toolbar-settings")
                                .ghost()
                                .large()
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
                                .large()
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
                                        .selected(self.right_sidebar_visible && !sql_active)
                                        .disabled(sql_active)
                                        .accessibility_label("Toggle right sidebar")
                                        .tooltip("Show or hide right sidebar")
                                        .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                            cx.stop_propagation()
                                        })
                                        .on_click(cx.listener(|view, _, _, cx| {
                                            cx.stop_propagation();
                                            view.right_sidebar_visible =
                                                !view.right_sidebar_visible;
                                            cx.notify();
                                        })),
                                ),
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
            titlebar: Some(TitlebarOptions {
                // Center the native 14 px controls in the 48 px toolbar.
                traffic_light_position: Some(point(px(9.), px(17.))),
                ..TitleBar::title_bar_options()
            }),
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
            if view.tabs.is_empty() {
                view.connections
                    .update(cx, |connections, cx| connections.open_debug_table(cx));
            }
        });
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-,", OpenSettings, None),
            #[cfg(not(target_os = "macos"))]
            KeyBinding::new("ctrl-q", Quit, None),
            #[cfg(not(target_os = "macos"))]
            KeyBinding::new("ctrl-,", OpenSettings, None),
        ]);
        let quit_view = _view.downgrade();
        cx.on_action(move |_: &Quit, cx| {
            let quit_view = quit_view.clone();
            cx.defer(move |cx| {
                let _ = cx.update_window(handle, |_, window, cx| {
                    let _ = quit_view.update(cx, |view, cx| {
                        if view.prepare_close(true, window, cx) {
                            cx.quit();
                        }
                    });
                });
            });
        });
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

    #[gpui_kit::test]
    fn sql_workspace_recovers_offline_autosaves_edits_and_keeps_failed_saves(
        cx: &mut TestAppContext,
    ) {
        use crate::{
            settings::{Connection, DatabaseKind, Settings},
            workspace_state::{SavedTab, SqlDraft, WorkspaceFile, WorkspaceState},
        };
        let directory =
            std::env::temp_dir().join(format!("tablelane-workspace-ui-{}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("workspace.json");
        let config = Connection {
            name: "Local".into(),
            database_type: DatabaseKind::MariaDB,
            host: "127.0.0.1".into(),
            port: Some(3306),
            username: "root".into(),
            password: "never-write-this-password".into(),
            database: String::new(),
            file_path: String::new(),
        };
        let mut state = WorkspaceState::default();
        state.tabs = vec![
            SavedTab::Sql(SqlDraft {
                number: 2,
                connection_key: config.key(),
                connection_name: config.name.clone(),
                database: "offline".into(),
                table: "widgets".into(),
                sql: "SELECT '世界';\n-- draft\n".into(),
            }),
            SavedTab::Sql(SqlDraft {
                number: 7,
                connection_key: config.key(),
                connection_name: config.name.clone(),
                database: "offline".into(),
                table: "widgets".into(),
                sql: "UPDATE widgets SET name = 'unfinished'".into(),
            }),
        ];
        state.active = 1;
        assert!(WorkspaceFile::load(Some(path.clone())).0.save(&state));
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(Settings {
                connections: vec![config.clone()],
                ..Settings::default()
            });
        });
        let mut app = None;
        let handle = cx.open_window(size(px(1100.), px(700.)), |window, cx| {
            let view = cx.new(|cx| {
                AppView::new(
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
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(app.read(cx).tabs.len(), 2);
            assert_eq!(app.read(cx).active_tab, 1);
            assert_eq!(app.read(cx).next_sql_number, 8);
            assert_eq!(
                window.within("workspace-tabs").find(1_usize).label(),
                Some("Local / offline / SQL 7 · Draft")
            );
            assert!(window.find("sql-offline").visible());
            window.click("run-sql", cx);
            window.press("cmd-enter", cx);
            let super::WorkspaceTab::Sql(editor, _) = &app.read(cx).tabs[1] else {
                panic!("SQL tab missing")
            };
            assert!(!editor.read(cx).is_running());
            assert!(editor.read(cx).request().is_none());
            window.press("ctrl-shift-tab", cx);
            assert_eq!(app.read(cx).active_tab, 0);
            window.press("secondary-a", cx);
            window.input("SELECT '保存';\n-- still a draft", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.background_executor
            .advance_clock(std::time::Duration::from_millis(299));
        cx.run_until_parked();
        assert_eq!(WorkspaceFile::load(Some(path.clone())).1, state);
        cx.background_executor
            .advance_clock(std::time::Duration::from_millis(1));
        cx.run_until_parked();
        let saved = WorkspaceFile::load(Some(path.clone())).1;
        assert_eq!(saved.active, 0);
        assert_eq!(saved.tabs[0].sql().sql, "SELECT '保存';\n-- still a draft");
        assert_eq!(saved.tabs[1].sql().sql, state.tabs[1].sql().sql);
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains(&config.password)
        );
        cx.update_window(handle.into(), |_, window, cx| {
            window.press("cmd-w", cx);
            window.press("escape", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            assert_eq!(app.read(cx).tabs.len(), 2);
            window.press("cmd-w", cx);
            window.within("dialog").click("ok", cx);
        })
        .unwrap();
        cx.run_until_parked();
        let saved = WorkspaceFile::load(Some(path.clone())).1;
        assert_eq!(saved.tabs.len(), 1);
        assert_eq!(saved.tabs[0].sql().number, 7);
        let original = std::fs::read(&path).unwrap();
        let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
        std::fs::create_dir(&temporary).unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.press("secondary-a", cx);
            window.input("SELECT 'latest unsaved'", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.background_executor
            .advance_clock(std::time::Duration::from_millis(300));
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window
                    .find("workspace-error")
                    .label()
                    .unwrap()
                    .contains("Couldn’t save workspace")
            );
        })
        .unwrap();
        let mut visual = gpui_kit::VisualTestContext::from_window(handle.into(), cx);
        assert!(!visual.simulate_close());
        cx.update_window(handle.into(), |_, window, cx| {
            window.press("escape", cx);
        })
        .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), original);
        std::fs::remove_dir(&temporary).unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("retry-workspace-save", cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(
            WorkspaceFile::load(Some(path.clone())).1.tabs[0].sql().sql,
            "SELECT 'latest unsaved'"
        );
        // A fresh application view restores the file, with no connection or SQL execution.
        let restored = cx.open_window(size(px(1100.), px(700.)), |window, cx| {
            let view = cx.new(|cx| {
                AppView::new(
                    Some(directory.join("settings.json")),
                    None,
                    None,
                    window,
                    cx,
                )
            });
            assert_eq!(view.read(cx).tabs.len(), 1);
            assert_eq!(
                view.read(cx).capture_workspace(cx).tabs[0].sql().sql,
                "SELECT 'latest unsaved'"
            );
            Root::new(view, window, cx)
        });
        cx.run_until_parked();
        cx.update_window(restored.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("sql-offline").visible());
        })
        .unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

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
                "toolbar-sql-editor",
            ] {
                window.double_click(id, cx);
                assert_eq!(double_clicks.get(), 0, "{id} leaked a double click");
            }
            assert!(window.find("sidebar").visible());
            assert!(window.find("bottom-bar").visible());
            assert!(window.try_find("right-sidebar").is_none());
            let title_bar = window.find("title-bar").bounds();
            let sql_button = window.find("toolbar-sql-editor").bounds();
            let settings_button = window.find("toolbar-settings").bounds();
            window.click_at(
                "title-bar",
                point(
                    (sql_button.right() + settings_button.left()) / 2. - title_bar.left(),
                    title_bar.size.height / 2.,
                ),
                cx,
            );
            assert_eq!(double_clicks.get(), 1);
        })
        .unwrap();
    }
}
