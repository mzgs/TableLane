use crate::{
    connections::OpenTable,
    database::{TableRows, TableStructure, quote_identifier},
};
use gpui_kit::{
    component::{
        ActiveTheme, Disableable, Sizable, WindowExt,
        button::{Button, ButtonVariants},
        checkbox::Checkbox,
        dialog::{DialogAction, DialogFooter},
        form::{Field, Form},
        input::{Editor, EditorState, Input, InputEvent, InputState},
        tab::{Tab, TabBar},
        table::{Column, DataTable, TableDelegate, TableState},
    },
    prelude::FluentBuilder,
    *,
};

struct Metadata {
    columns: Vec<Column>,
    data: TableRows,
}
impl TableDelegate for Metadata {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }
    fn rows_count(&self, _: &App) -> usize {
        self.data.rows.len()
    }
    fn column(&self, ix: usize, _: &App) -> Column {
        self.columns[ix].clone()
    }
    fn render_td(
        &mut self,
        row: usize,
        column: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let value = &self.data.rows[row][column];
        div()
            .px_2()
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
            .p_3()
            .text_color(cx.theme().muted_foreground)
            .child("No entries in this section")
    }
}

pub(crate) enum SchemaEvent {
    Apply(String),
    Changed(bool),
    EditSql(String),
}
impl EventEmitter<SchemaEvent> for SchemaView {}

pub(crate) struct SchemaView {
    request: OpenTable,
    tables: Vec<Entity<TableState<Metadata>>>,
    ddl: Entity<EditorState>,
    section: usize,
    loading: bool,
    running: bool,
    status: String,
    failed: bool,
    no_backslash_escapes: bool,
    focus: FocusHandle,
    change_form: Option<Entity<ChangeForm>>,
}
impl SchemaView {
    pub(crate) fn new(request: OpenTable, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            request,
            tables: Vec::new(),
            ddl: cx.new(|cx| EditorState::new(window, cx).language("sql")),
            section: 0,
            loading: false,
            running: false,
            status: String::new(),
            failed: false,
            no_backslash_escapes: false,
            focus: cx.focus_handle().tab_stop(true),
            change_form: None,
        };
        view.refresh(window, cx);
        view
    }
    pub(crate) fn request(&self) -> &OpenTable {
        &self.request
    }

    pub(crate) fn attach(
        &mut self,
        session: std::sync::Arc<crate::database::Session>,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.request.session.is_connected()
            || self.is_busy()
            || !session
                .databases()
                .get(&self.request.database)
                .is_some_and(|tables| tables.contains(&self.request.table))
        {
            return;
        }
        self.request.session = session;
        self.request.connection_name = name;
        self.refresh(window, cx);
    }
    pub(crate) fn is_busy(&self) -> bool {
        self.loading || self.running
    }

    pub(crate) fn is_running(&self) -> bool {
        self.running
    }
    pub(crate) fn focus(&self, window: &mut Window, cx: &mut App) {
        self.focus.focus(window, cx);
    }
    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading || self.running || !self.request.session.is_connected() {
            return;
        }
        self.loading = true;
        self.failed = false;
        self.status = "Loading table structure…".into();
        let request = self.request.clone();
        let task = cx.background_spawn(async move {
            async_std::task::block_on(
                request
                    .session
                    .read_structure(&request.database, &request.table),
            )
        });
        cx.spawn_in(window, async move |view, cx| {
            let result = task.await;
            let _ = view.update_in(cx, |view, window, cx| {
                view.loading = false;
                match result {
                    Ok(data) => view.finish(data, window, cx),
                    Err(error) => {
                        view.status = error;
                        view.failed = true;
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn finish(&mut self, data: TableStructure, window: &mut Window, cx: &mut Context<Self>) {
        self.no_backslash_escapes = data.no_backslash_escapes;
        self.ddl
            .update(cx, |editor, cx| editor.set_value(data.ddl, window, cx));
        self.tables = [data.columns, data.indexes, data.foreign_keys]
            .into_iter()
            .map(|data| {
                let columns = data
                    .schema
                    .columns
                    .iter()
                    .map(|name| {
                        Column::new(name.clone(), name.clone())
                            .width(rems(12.).to_pixels(window.rem_size()))
                            .movable(false)
                    })
                    .collect();
                let table = cx.new(|cx| {
                    TableState::new(Metadata { columns, data }, window, cx)
                        .col_movable(false)
                        .row_header(false)
                });
                cx.observe(&table, |_, _, cx| cx.notify()).detach();
                table
            })
            .collect();
        self.status = "Structure loaded".into();
        self.failed = false;
    }
    fn selected<'a>(&self, cx: &'a App) -> Option<&'a Vec<Option<String>>> {
        let table = self.tables.get(self.section)?.read(cx);
        table
            .selected_row()
            .and_then(|ix| table.delegate().data.rows.get(ix))
    }
    fn open_change(&mut self, kind: ChangeKind, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading
            || self.running
            || !self.request.session.is_connected()
            || self.tables.is_empty()
            || window.has_active_dialog(cx)
        {
            return;
        }
        let row = self.selected(cx);
        let target = match kind {
            ChangeKind::ModifyColumn | ChangeKind::DropColumn | ChangeKind::DropIndex => {
                let Some(name) = row.and_then(|row| row[0].clone()) else {
                    return;
                };
                name
            }
            _ => String::new(),
        };
        let column = if matches!(kind, ChangeKind::ModifyColumn) {
            let row = row.unwrap();
            if row[5]
                .as_deref()
                .is_some_and(|extra| extra.contains("GENERATED"))
            {
                self.status =
                    "Use Edit SQL for generated columns so their expression is preserved.".into();
                self.failed = true;
                cx.notify();
                return;
            }
            Some(row.clone())
        } else {
            None
        };
        let definition = if let Some(column) = &column {
            column[1].clone().unwrap_or_default()
        } else if matches!(kind, ChangeKind::AddIndex) {
            self.tables[0]
                .read(cx)
                .delegate()
                .data
                .rows
                .first()
                .and_then(|row| row[0].as_deref())
                .map(quote_identifier)
                .unwrap_or_default()
        } else {
            String::new()
        };
        let owner = cx.entity().downgrade();
        let form = cx.new(|cx| ChangeForm::new(kind, target, definition, column, self, window, cx));
        self.change_form = Some(form.clone());
        let title = format!("{} · {}", kind.label(), self.request.table);
        window.open_dialog(cx, move |dialog, window, cx| {
            let busy = form.read(cx).busy;
            let cancel = form.clone();
            let form = form.clone();
            let submit = form.clone();
            let owner = owner.clone();
            dialog
                .title(title.clone())
                .close_button(!busy)
                .keyboard(!busy)
                .overlay_closable(false)
                .on_cancel(move |_, _, cx| !cancel.read(cx).busy)
                .width(window.rem_size() * 44.)
                .child(form)
                .on_ok(move |_, _, cx| {
                    submit.update(cx, |form, cx| match form.sql(cx) {
                        Ok(sql) => {
                            if !form.busy {
                                let _ = owner.update(cx, |_, cx| cx.emit(SchemaEvent::Apply(sql)));
                            }
                            false
                        }
                        Err(error) => {
                            form.error = Some(error);
                            cx.notify();
                            false
                        }
                    })
                })
        });
    }
    pub(crate) fn reject_change(&mut self, error: String, cx: &mut Context<Self>) {
        if let Some(form) = &self.change_form {
            form.update(cx, |form, cx| {
                form.error = Some(error);
                form.busy = false;
                cx.notify();
            });
        }
    }
    pub(crate) fn apply(&mut self, sql: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.running || self.loading {
            return;
        }
        self.running = true;
        if let Some(form) = &self.change_form {
            form.update(cx, |form, cx| {
                form.busy = true;
                cx.notify();
            });
        }
        self.failed = false;
        self.status = "Applying schema change…".into();
        let request = self.request.clone();
        let task = cx.background_spawn(async move {
            async_std::task::block_on(async {
                request.session.run_sql(&request.database, &sql).await?;
                Ok::<_, String>(
                    request
                        .session
                        .read_structure(&request.database, &request.table)
                        .await,
                )
            })
        });
        cx.spawn_in(window, async move |view, cx| {
            let result = task.await;
            let _ = view.update_in(cx, |view, window, cx| {
                view.running = false;
                match result {
                    Ok(reloaded) => {
                        if view.change_form.take().is_some() {
                            window.close_dialog(cx);
                        }
                        match reloaded {
                            Ok(data) => {
                                view.finish(data, window, cx);
                                view.status = "Schema change applied".into();
                            }
                            Err(error) => {
                                view.tables.clear();
                                view.status =
                                    format!("Schema change applied, but refresh failed: {error}");
                                view.failed = true;
                            }
                        }
                        cx.emit(SchemaEvent::Changed(true));
                    }
                    Err(error) => {
                        view.status = error.clone();
                        view.failed = true;
                        view.reject_change(error, cx);
                        cx.emit(SchemaEvent::Changed(false));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}
impl Render for SchemaView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.loading || self.running || !self.request.session.is_connected();
        let selected = self.selected(cx).is_some();
        div().size_full().flex().flex_col().min_w_0().min_h_0().track_focus(&self.focus)
            .child(div().px_3().py_2().text_lg().font_weight(FontWeight::SEMIBOLD)
                .child(format!("{} / {} / {} · Structure", self.request.connection_name, self.request.database, self.request.table)))
            .child(TabBar::new("structure-sections").small().selected_index(self.section)
                .children(["Columns", "Indexes", "Foreign keys", "CREATE statement"].map(|name| Tab::new().label(name)))
                .on_click(cx.listener(|view, ix, _, cx| { view.section = *ix; cx.notify(); })))
            .child(div().flex().flex_wrap().gap_2().px_3().py_2()
                .when(self.section == 0, |bar| bar
                    .child(Button::new("add-column").small().label("Add column…").disabled(busy || self.tables.is_empty()).on_click(cx.listener(|view, _, window, cx| view.open_change(ChangeKind::AddColumn, window, cx))))
                    .child(Button::new("modify-column").small().label("Modify column…").disabled(busy || !selected).on_click(cx.listener(|view, _, window, cx| view.open_change(ChangeKind::ModifyColumn, window, cx))))
                    .child(Button::new("drop-column").small().label("Drop column…").disabled(busy || !selected).on_click(cx.listener(|view, _, window, cx| view.open_change(ChangeKind::DropColumn, window, cx)))))
                .when(self.section == 1, |bar| bar
                    .child(Button::new("add-index").small().label("Add index…").disabled(busy || self.tables.is_empty()).on_click(cx.listener(|view, _, window, cx| view.open_change(ChangeKind::AddIndex, window, cx))))
                    .child(Button::new("drop-index").small().label("Drop index…").disabled(busy || !selected).on_click(cx.listener(|view, _, window, cx| view.open_change(ChangeKind::DropIndex, window, cx)))))
                .child(Button::new("structure-edit-sql").small().outline().label("Edit SQL…").disabled(busy).on_click(cx.listener(|view, _, _, cx| {
                    cx.emit(SchemaEvent::EditSql(format!("-- Enter one schema change, then Run. DDL cannot be rolled back.\nALTER TABLE {}.{}\n    ", quote_identifier(&view.request.database), quote_identifier(&view.request.table))));
                })))
                .child(Button::new("refresh-structure").small().ghost().label("Refresh").disabled(busy).on_click(cx.listener(|view, _, window, cx| view.refresh(window, cx)))))
            .child(div().id("structure-status").test_support().role(Role::Status).aria_label(if self.request.session.is_connected() { self.status.clone() } else { "Offline. Reconnect in the sidebar to change structure.".into() }).px_3().pb_2().text_sm()
                .text_color(if self.failed { cx.theme().danger } else { cx.theme().muted_foreground }).child(if self.request.session.is_connected() { self.status.clone() } else { "Offline. Reconnect in the sidebar to change structure.".into() }))
            .child(div().flex_1().min_h_0().min_w_0()
                .when(self.section == 3, |body| body.child(Editor::new(&self.ddl).aria_label("CREATE statement").readonly(true).size_full()))
                .when_some(self.tables.get(self.section), |body, table| body.child(DataTable::new(table).small().stripe(true).bordered(false))))
    }
}

#[derive(Clone, Copy)]
enum ChangeKind {
    AddColumn,
    ModifyColumn,
    DropColumn,
    AddIndex,
    DropIndex,
}
impl ChangeKind {
    fn label(self) -> &'static str {
        match self {
            Self::AddColumn => "Add column",
            Self::ModifyColumn => "Modify column",
            Self::DropColumn => "Drop column",
            Self::AddIndex => "Add index",
            Self::DropIndex => "Drop index",
        }
    }
    fn drops(self) -> bool {
        matches!(self, Self::DropColumn | Self::DropIndex)
    }
}
fn change_sql(
    kind: ChangeKind,
    database: &str,
    table: &str,
    target: &str,
    name: &str,
    definition: &str,
    unique: bool,
) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() || name.contains('\0') {
        return Err("Enter a name without NUL characters.".into());
    }
    let definition = definition.trim();
    if matches!(
        kind,
        ChangeKind::AddColumn | ChangeKind::ModifyColumn | ChangeKind::AddIndex
    ) && definition.is_empty()
    {
        return Err("Enter a column definition or index columns.".into());
    }
    let quoted = quote_identifier(name);
    let clause = match kind {
        ChangeKind::AddColumn => format!("ADD COLUMN {quoted} {definition}"),
        ChangeKind::ModifyColumn => format!(
            "CHANGE COLUMN {} {quoted} {definition}",
            quote_identifier(target)
        ),
        ChangeKind::DropColumn => format!("DROP COLUMN {}", quote_identifier(target)),
        ChangeKind::AddIndex => format!(
            "ADD {}INDEX {quoted} ({definition})",
            if unique { "UNIQUE " } else { "" }
        ),
        ChangeKind::DropIndex if target == "PRIMARY" => "DROP PRIMARY KEY".into(),
        ChangeKind::DropIndex => format!("DROP INDEX {}", quote_identifier(target)),
    };
    Ok(format!(
        "ALTER TABLE {}.{}\n    {clause};",
        quote_identifier(database),
        quote_identifier(table)
    ))
}
fn column_definition(row: &[Option<String>], no_backslash_escapes: bool) -> String {
    let mut definition = row[1].clone().unwrap_or_default();
    if let Some(collation) = &row[6] {
        definition.push_str(&format!(" COLLATE {}", quote_identifier(collation)));
    }
    definition.push_str(if row[2].as_deref() == Some("YES") {
        " NULL"
    } else {
        " NOT NULL"
    });
    if let Some(default) = &row[3] {
        definition.push_str(&format!(" DEFAULT {default}"));
    }
    if let Some(extra) = &row[5] {
        definition.push(' ');
        definition.push_str(extra);
    }
    if let Some(comment) = &row[7] {
        let comment = if no_backslash_escapes {
            comment.clone()
        } else {
            comment.replace('\\', "\\\\")
        };
        definition.push_str(&format!(" COMMENT '{}'", comment.replace('\'', "''")));
    }
    definition.trim().to_owned()
}
// MariaDB metadata escapes default expressions with backslashes even when the session disables them.
fn metadata_default(value: &str, no_backslash_escapes: bool) -> String {
    if !no_backslash_escapes {
        return value.to_owned();
    }
    let mut result = String::new();
    let mut quoted = false;
    let mut chars = value.chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            '`' if !quoted => {
                result.push(character);
                while let Some(c) = chars.next() {
                    result.push(c);
                    if c == '`' {
                        if chars.peek() == Some(&'`') {
                            result.push(chars.next().unwrap());
                        } else {
                            break;
                        }
                    }
                }
            }
            '\'' => {
                result.push('\'');
                if quoted && chars.peek() == Some(&'\'') {
                    result.push(chars.next().unwrap());
                } else {
                    quoted = !quoted;
                }
            }
            '\\' if quoted => {
                let escaped = match chars.next() {
                    Some('0') => '\0',
                    Some('b') => '\u{0008}',
                    Some('n') => '\n',
                    Some('r') => '\r',
                    Some('t') => '\t',
                    Some('Z') => '\u{001a}',
                    Some(c) => c,
                    None => '\\',
                };
                if escaped == '\'' {
                    result.push('\'');
                }
                result.push(escaped);
            }
            other => result.push(other),
        }
    }
    result
}
struct ChangeForm {
    kind: ChangeKind,
    target: String,
    request: OpenTable,
    name: Entity<InputState>,
    definition: Entity<InputState>,
    preview: Entity<EditorState>,
    unique: bool,
    busy: bool,
    nullable: bool,
    default_value: Entity<InputState>,
    comment: Entity<InputState>,
    original_column: Option<Vec<Option<String>>>,
    no_backslash_escapes: bool,
    error: Option<String>,
}
impl ChangeForm {
    fn new(
        kind: ChangeKind,
        target: String,
        definition: String,
        original_column: Option<Vec<Option<String>>>,
        structure: &SchemaView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let request = structure.request.clone();
        let no_backslash_escapes = structure.no_backslash_escapes;
        let name = cx.new(|cx| {
            let mut input = InputState::new(window, cx);
            input.set_value(target.clone(), window, cx);
            input
        });
        let definition = cx.new(|cx| {
            let mut input =
                InputState::new(window, cx).placeholder(if matches!(kind, ChangeKind::AddIndex) {
                    "`column`, `other_column`"
                } else {
                    "varchar(255)"
                });
            input.set_value(definition, window, cx);
            input
        });
        let default_value = cx.new(|cx| {
            let mut input = InputState::new(window, cx)
                .placeholder("No default; use NULL or an SQL expression");
            input.set_value(
                original_column
                    .as_ref()
                    .and_then(|row| row[3].as_deref())
                    .map(|value| metadata_default(value, no_backslash_escapes))
                    .unwrap_or_default(),
                window,
                cx,
            );
            input
        });
        let comment = cx.new(|cx| {
            let mut input = InputState::new(window, cx);
            input.set_value(
                original_column
                    .as_ref()
                    .and_then(|row| row[7].clone())
                    .unwrap_or_default(),
                window,
                cx,
            );
            input
        });
        let nullable = original_column
            .as_ref()
            .is_none_or(|row| row[2].as_deref() == Some("YES"));
        let preview = cx.new(|cx| EditorState::new(window, cx).language("sql"));
        for input in [&name, &definition, &default_value, &comment] {
            cx.subscribe_in(input, window, |form, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    form.update_preview(window, cx);
                }
            })
            .detach();
        }
        let mut form = Self {
            kind,
            target,
            request,
            name,
            definition,
            preview,
            unique: false,
            busy: false,
            nullable,
            default_value,
            comment,
            original_column,
            no_backslash_escapes,
            error: None,
        };
        form.update_preview(window, cx);
        form
    }
    fn sql(&self, cx: &App) -> Result<String, String> {
        let mut definition = self.definition.read(cx).value().to_string();
        if matches!(self.kind, ChangeKind::AddColumn | ChangeKind::ModifyColumn) {
            if definition.trim().is_empty() {
                return Err("Enter a column type.".into());
            }
            let mut row = self
                .original_column
                .clone()
                .unwrap_or_else(|| vec![None; 9]);
            row[1] = Some(definition);
            row[2] = Some(if self.nullable { "YES" } else { "NO" }.into());
            let default = self.default_value.read(cx).value().trim().to_owned();
            row[3] = (!default.is_empty()).then_some(default);
            row[7] = Some(self.comment.read(cx).value().to_string());
            definition = column_definition(&row, self.no_backslash_escapes);
        }
        change_sql(
            self.kind,
            &self.request.database,
            &self.request.table,
            &self.target,
            &self.name.read(cx).value(),
            &definition,
            self.unique,
        )
    }
    fn update_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let sql = self.sql(cx).unwrap_or_default();
        self.preview
            .update(cx, |preview, cx| preview.set_value(sql, window, cx));
        self.error = None;
        cx.notify();
    }
}
impl Render for ChangeForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().flex().flex_col().gap_3()
            .child(Form::new()
                .child(Field::new().label("Name").child(Input::new(&self.name).id("schema-name").disabled(self.kind.drops() || self.busy)))
                .when(!self.kind.drops(), |form| form.child(Field::new().label(if matches!(self.kind, ChangeKind::AddIndex) { "Columns (SQL)" } else { "Type" })
                    .description(if matches!(self.kind, ChangeKind::AddIndex) { "Quote column names with backticks; separate columns with commas." } else { "MariaDB type, for example int or varchar(255). Existing extra attributes are preserved." })
                    .child(Input::new(&self.definition).id("schema-definition").disabled(self.busy))))
                .when(matches!(self.kind, ChangeKind::AddColumn | ChangeKind::ModifyColumn), |form| form
                    .child(Field::new().label_indent(false).child(Checkbox::new("schema-nullable").label("Allow NULL").checked(self.nullable).disabled(self.busy)
                        .on_change(cx.listener(|form, checked, window, cx| { form.nullable = *checked; form.update_preview(window, cx); }))))
                    .child(Field::new().label("Default (SQL expression)").child(Input::new(&self.default_value).id("schema-default").disabled(self.busy)))
                    .child(Field::new().label("Comment").child(Input::new(&self.comment).id("schema-comment").disabled(self.busy))))
                .when(matches!(self.kind, ChangeKind::AddIndex), |form| form.child(Field::new().label_indent(false).child(Checkbox::new("schema-unique").label("Unique index").checked(self.unique).disabled(self.busy)
                    .on_change(cx.listener(|form, checked, window, cx| { form.unique = *checked; form.update_preview(window, cx); }))))))
            .child(div().text_sm().child("Review the SQL before applying. Schema changes commit immediately and cannot be rolled back. Dropping a column deletes its data."))
            .child(Editor::new(&self.preview).aria_label("Schema change SQL").readonly(true).h(rems(8.)))
            .when_some(self.error.as_ref(), |form, error| form.child(div().id("schema-form-error").test_support().role(Role::Status).aria_label(error.clone()).text_color(cx.theme().danger).child(error.clone())))
            .child(DialogFooter::new().child(Button::new("cancel-schema-change").label("Cancel").disabled(self.busy).on_click(|_, window, cx| window.close_dialog(cx)))
                .child(DialogAction::new().child(Button::new("apply-schema-change").disabled(self.busy).loading(self.busy).label(if self.kind.drops() { "Drop" } else { "Apply change" }).when(self.kind.drops(), |button| button.danger()))))
    }
}

#[cfg(test)]
mod tests {
    use super::{ChangeKind, change_sql, column_definition, metadata_default};
    use crate::{
        WorkspaceTab,
        connections::OpenTable,
        settings::{Connection, DatabaseKind},
    };
    use gpui_kit::{
        AppContext, TestAppContext,
        component::{Root, Size, WindowExt},
        point, px, size,
        test::{TestAppContextExt, TestWindowExt},
    };

    #[test]
    fn schema_changes_quote_identifiers_validate_inputs_and_preserve_attributes() {
        assert_eq!(
            change_sql(
                ChangeKind::ModifyColumn,
                "d`b",
                "t`b",
                "o`ld",
                "n`ew",
                "int NOT NULL",
                false
            )
            .unwrap(),
            "ALTER TABLE `d``b`.`t``b`\n    CHANGE COLUMN `o``ld` `n``ew` int NOT NULL;"
        );
        assert_eq!(
            change_sql(
                ChangeKind::DropIndex,
                "db",
                "table",
                "PRIMARY",
                "PRIMARY",
                "",
                false
            )
            .unwrap(),
            "ALTER TABLE `db`.`table`\n    DROP PRIMARY KEY;"
        );
        assert!(change_sql(ChangeKind::AddColumn, "db", "table", "", "", "int", false).is_err());
        assert!(change_sql(ChangeKind::AddColumn, "db", "table", "", "x", " ", false).is_err());
        assert_eq!(metadata_default("'a\\\\b''c'", true), "'a\\b''c'");
        assert_eq!(
            metadata_default("concat('a\\\\b', 'x')", true),
            "concat('a\\b', 'x')"
        );
        assert_eq!(metadata_default("'hello'", false), "'hello'");
        let row = vec![
            Some("name".into()),
            Some("varchar(100)".into()),
            Some("NO".into()),
            Some("'hello'".into()),
            None,
            Some("INVISIBLE".into()),
            Some("utf8mb4_unicode_ci".into()),
            Some("it's \\ text".into()),
            None,
        ];
        assert_eq!(
            column_definition(&row, false),
            "varchar(100) COLLATE `utf8mb4_unicode_ci` NOT NULL DEFAULT 'hello' INVISIBLE COMMENT 'it''s \\\\ text'"
        );
        assert_eq!(
            column_definition(&row, true),
            "varchar(100) COLLATE `utf8mb4_unicode_ci` NOT NULL DEFAULT 'hello' INVISIBLE COMMENT 'it''s \\ text'"
        );
    }

    #[gpui_kit::test]
    #[ignore = "requires an isolated MariaDB server and TABLELANE_TEST_MARIADB_PORT"]
    async fn mariadb_structure_forms_apply_changes_refresh_data_and_preserve_attributes(
        cx: &mut TestAppContext,
    ) {
        let session = std::sync::Arc::new(
            async_std::task::block_on(crate::database::connect(&Connection {
                name: "Test".into(),
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
            for sql in [
                "DROP DATABASE IF EXISTS structure_forms_test",
                "CREATE DATABASE structure_forms_test",
                "CREATE TABLE structure_forms_test.parent (id INT PRIMARY KEY)",
                "CREATE TABLE structure_forms_test.widgets (id INT PRIMARY KEY AUTO_INCREMENT, parent_id INT, name VARCHAR(100) NOT NULL DEFAULT 'hello' COMMENT 'keep me', computed INT GENERATED ALWAYS AS (id + 1) VIRTUAL, INDEX ix_name (name(10)), CONSTRAINT fk_parent FOREIGN KEY (parent_id) REFERENCES parent(id) ON DELETE SET NULL)",
                "INSERT INTO structure_forms_test.widgets (name) VALUES ('Original')",
            ] {
                session.run_sql("tablelane_test", sql).await.unwrap();
            }
        });
        let metadata =
            async_std::task::block_on(session.read_structure("structure_forms_test", "widgets"))
                .unwrap();
        assert_eq!(metadata.columns.rows.len(), 4);
        assert!(metadata.ddl.contains("fk_parent"));
        assert!(
            metadata
                .indexes
                .rows
                .iter()
                .any(|row| row[0].as_deref() == Some("ix_name") && row[4].as_deref() == Some("10"))
        );
        assert_eq!(
            metadata.foreign_keys.rows[0][6].as_deref(),
            Some("SET NULL")
        );
        assert!(
            async_std::task::block_on(session.read_structure("structure_forms_test", "missing"))
                .is_err()
        );
        cx.update(gpui_kit::init);
        let mut app = None;
        let request = OpenTable {
            session: session.clone(),
            connection_name: "Test".into(),
            database: "structure_forms_test".into(),
            table: "widgets".into(),
        };
        let handle = cx.open_window(size(px(1200.), px(800.)), |window, cx| {
            let view = cx.new(|cx| crate::AppView::new(None, None, None, window, cx));
            view.update(cx, |view, cx| view.open_table(&request, window, cx));
            app = Some(view.clone());
            Root::new(view, window, cx)
        });
        let app = app.unwrap();
        let timeout = std::time::Duration::from_secs(15);
        cx.wait_for(handle.into(), timeout, |window, _| {
            window.try_find("table").is_some()
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("toolbar-structure", cx)
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| matches!(&app.read(cx).tabs[1], WorkspaceTab::Structure(view) if !view.read(cx).loading)).await;
        let structure = cx.update(|cx| match &app.read(cx).tabs[1] {
            WorkspaceTab::Structure(view) => view.clone(),
            _ => panic!("structure tab missing"),
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("toolbar-structure", cx);
            assert_eq!(app.read(cx).tabs.len(), 2);
            window.click("add-column", cx);
            window.click("apply-schema-change", cx);
            window.render_frame(cx);
            window.render_frame(cx);
            assert!(window.has_active_dialog(cx));
            assert!(
                window
                    .find("schema-form-error")
                    .label()
                    .unwrap()
                    .contains("type")
            );
            for (id, value) in [
                ("schema-name", "notes`text"),
                ("schema-definition", "varchar(200)"),
                ("schema-default", "'test'"),
                ("schema-comment", "it's \\ useful"),
            ] {
                window.click(id, cx);
                window.press("secondary-a", cx);
                window.input(value, cx);
            }
            window.click("apply-schema-change", cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| {
            !structure.read(cx).is_running()
        })
        .await;
        cx.wait_for(handle.into(), timeout, |_, cx| {
            app.read(cx).table().is_none()
                && match &app.read(cx).tabs[0] {
                    WorkspaceTab::Table(table) => table.read(cx).can_change_schema(cx),
                    _ => false,
                }
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            assert_eq!(
                window.find("structure-status").label(),
                Some("Schema change applied")
            );
            assert_eq!(
                structure.read(cx).tables[0]
                    .read(cx)
                    .delegate()
                    .data
                    .rows
                    .len(),
                5
            );
            window.click_at(
                ("row", 2_usize),
                point(window.rem_size(), Size::Small.table_row_height() / 2.),
                cx,
            );
            window.click("modify-column", cx);
            assert_eq!(
                window.find("schema-definition").value(),
                Some("varchar(100)")
            );
            assert_eq!(window.find("schema-default").value(), Some("'hello'"));
            assert_eq!(window.find("schema-comment").value(), Some("keep me"));
            window.click("schema-name", cx);
            window.press("secondary-a", cx);
            window.input("renamed", cx);
            window.click("apply-schema-change", cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| {
            !structure.read(cx).is_running()
        })
        .await;
        let metadata =
            async_std::task::block_on(session.read_structure("structure_forms_test", "widgets"))
                .unwrap();
        assert_eq!(metadata.columns.rows[2][0].as_deref(), Some("renamed"));
        assert_eq!(metadata.columns.rows[2][3].as_deref(), Some("'hello'"));
        assert_eq!(metadata.columns.rows[2][7].as_deref(), Some("keep me"));
        assert_eq!(
            metadata.columns.rows[4][7].as_deref(),
            Some("it's \\ useful")
        );
        cx.update_window(handle.into(), |_, window, cx| {
            window.within("structure-sections").click(1_usize, cx);
            window.click("add-index", cx);
            for (id, value) in [
                ("schema-name", "ix_notes"),
                ("schema-definition", "`missing`"),
            ] {
                window.click(id, cx);
                window.press("secondary-a", cx);
                window.input(value, cx);
            }
            window.click("apply-schema-change", cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |window, cx| {
            !structure.read(cx).is_running() && window.try_find("schema-form-error").is_some()
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            assert_eq!(window.find("schema-name").value(), Some("ix_notes"));
            assert_eq!(window.find("schema-definition").value(), Some("`missing`"));
            window.click("schema-definition", cx);
            window.press("secondary-a", cx);
            window.input("`notes``text`(20), `renamed`", cx);
            window.click("schema-unique", cx);
            window.click("apply-schema-change", cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |window, cx| {
            !structure.read(cx).is_running() && !window.has_active_dialog(cx)
        })
        .await;
        let ix = cx.update(|cx| {
            structure.read(cx).tables[1]
                .read(cx)
                .delegate()
                .data
                .rows
                .iter()
                .position(|row| row[0].as_deref() == Some("ix_notes"))
                .unwrap()
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.click_at(
                ("row", ix),
                point(window.rem_size(), Size::Small.table_row_height() / 2.),
                cx,
            );
            window.click("drop-index", cx);
            window.press("escape", cx);
            assert!(!window.has_active_dialog(cx));
            window.click("drop-index", cx);
            window.click("apply-schema-change", cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |window, cx| {
            !structure.read(cx).is_running() && !window.has_active_dialog(cx)
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.within("structure-sections").click(0_usize, cx);
            window.click_at(
                ("row", 4_usize),
                point(window.rem_size(), Size::Small.table_row_height() / 2.),
                cx,
            );
            window.click("drop-column", cx);
            window.click("apply-schema-change", cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |window, cx| {
            !structure.read(cx).is_running() && !window.has_active_dialog(cx)
        })
        .await;
        let metadata =
            async_std::task::block_on(session.read_structure("structure_forms_test", "widgets"))
                .unwrap();
        assert_eq!(metadata.columns.rows.len(), 4);
        async_std::task::block_on(async {
            session
                .run_sql(
                    "structure_forms_test",
                    "SET SESSION sql_mode = 'NO_BACKSLASH_ESCAPES'",
                )
                .await
                .unwrap();
            session.run_sql("structure_forms_test", "ALTER TABLE widgets MODIFY renamed VARCHAR(100) NOT NULL DEFAULT 'a\\b''c' COMMENT 'keep me'").await.unwrap();
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("refresh-structure", cx)
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |_, cx| !structure.read(cx).loading)
            .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.click_at(
                ("row", 2_usize),
                point(window.rem_size(), Size::Small.table_row_height() / 2.),
                cx,
            );
            window.click("modify-column", cx);
            assert_eq!(window.find("schema-default").value(), Some("'a\\b''c'"));
            window.click("apply-schema-change", cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), timeout, |window, cx| {
            !structure.read(cx).is_running() && !window.has_active_dialog(cx)
        })
        .await;
        let metadata =
            async_std::task::block_on(session.read_structure("structure_forms_test", "widgets"))
                .unwrap();
        assert_eq!(metadata.columns.rows[2][3].as_deref(), Some("'a\\\\b''c'"));
        async_std::task::block_on(
            session.run_sql("structure_forms_test", "SET SESSION sql_mode = DEFAULT"),
        )
        .unwrap();
        async_std::task::block_on(
            session.run_sql("tablelane_test", "DROP DATABASE structure_forms_test"),
        )
        .unwrap();
    }
}
