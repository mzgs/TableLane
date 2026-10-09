use gpui_kit::{
    component::{
        ActiveTheme, Selectable, WindowExt,
        button::{Button, ButtonVariants},
        v_flex,
    },
    *,
};

const DATABASES: [&str; 5] = ["MySQL", "MariaDB", "MongoDB", "SQLite", "PostgreSQL"];

pub(crate) fn open(window: &mut Window, cx: &mut App) {
    let view = cx.new(|_| ConnectionDialog {
        selected: DATABASES[0],
    });
    window.open_dialog(cx, move |dialog, window, _| {
        dialog
            .title("Add connection")
            .width(window.rem_size() * 32.)
            .child(view.clone())
    });
}

struct ConnectionDialog {
    selected: &'static str,
}

impl Render for ConnectionDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .w_full()
            .h_72()
            .child(
                v_flex()
                    .id("database-list")
                    .test_support()
                    .w_40()
                    .h_full()
                    .flex_none()
                    .p_2()
                    .gap_1()
                    .bg(cx.theme().sidebar)
                    .rounded(cx.theme().radius)
                    .children(DATABASES.map(|database| {
                        Button::new(database)
                            .ghost()
                            .label(database)
                            .justify_start()
                            .selected(self.selected == database)
                            .toggled(self.selected == database)
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.selected = database;
                                cx.notify();
                            }))
                    })),
            )
            .child(div().flex_1().min_w_0())
    }
}
