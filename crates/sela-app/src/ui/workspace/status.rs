//! Bottom status bar.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{h_flex, ActiveTheme, Disableable as _, Sizable as _};
use gpui_kit::prelude::*;
use gpui_kit::{px, Context, IntoElement, Window};

use sela_config::ThemePreference;
use sela_core::RowCountKind;

use super::Workspace;

impl Workspace {
    pub(super) fn set_theme_preference(
        &mut self,
        preference: ThemePreference,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.theme_preference = preference;
        let _ = self.settings.save(preference);
        super::super::theme::apply_preference(preference, Some(window), cx);
        cx.notify();
    }

    pub(super) fn status_bar(&self, cx: &Context<Self>) -> impl IntoElement {
        let this = cx.entity();
        let view = self.active_view();
        let status = view.map(|v| v.status.clone()).unwrap_or_default();
        let label = view
            .map(|v| v.source_label())
            .unwrap_or_else(|| "No table open".into());
        let connected = self.session.is_some();
        let busy = view.map(|v| v.busy).unwrap_or(false);
        let truncated = view.map(|v| v.truncated).unwrap_or(false);
        let can_load_more = view.map(|v| v.can_load_more).unwrap_or(false);
        let is_sql = view.is_some_and(|v| matches!(v.source, super::GridSource::Sql(_)));
        let show_count = view.is_some_and(|v| {
            matches!(v.source, super::GridSource::Table { .. })
                && matches!(
                    v.row_count_kind,
                    RowCountKind::Estimated | RowCountKind::Unknown
                )
                && !v.busy
        });
        let filtered = view.is_some_and(|v| !v.filters.is_empty());
        let count_tooltip = if filtered {
            "Filtered results are not estimated. Count runs COUNT(*) for this filter."
        } else {
            "No estimate for this table. Count runs COUNT(*)."
        };
        let theme_label = self.theme_preference.label();
        let current = self.theme_preference;
        let offer_system = self.system_theme_available;

        let history_open = self.history_open;
        h_flex()
            .h(px(28.))
            .px_3()
            .items_center()
            .justify_between()
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().status_bar)
            .text_xs()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(status)
                    .when(truncated && can_load_more && is_sql && !busy, |row| {
                        let this = this.clone();
                        row.child({
                            Button::new("load-more")
                                .xsmall()
                                .label("Load more")
                                .disabled(!connected)
                                .on_click(move |_, _, cx| {
                                    this.update(cx, |workspace, cx| workspace.load_more(cx));
                                })
                        })
                    })
                    .when(show_count, |row| {
                        let this = this.clone();
                        row.child({
                            Button::new("status-count")
                                .xsmall()
                                .label("Count")
                                .disabled(!connected || busy)
                                .tooltip(count_tooltip)
                                .on_click(move |_, _, cx| {
                                    this.update(cx, |workspace, cx| {
                                        workspace.request_exact_count(cx)
                                    });
                                })
                        })
                    }),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child({
                        let this = this.clone();
                        Button::new("status-history")
                            .xsmall()
                            .label("History")
                            .primary()
                            .when(!history_open, |button| button.ghost())
                            .tooltip("Toggle query history (Ctrl/⌘+Shift+H)")
                            .on_click(move |_, window, cx| {
                                this.update(cx, |workspace, cx| {
                                    workspace.toggle_history_panel(window, cx)
                                });
                            })
                    })
                    .child({
                        let this = this.clone();
                        Button::new("theme-menu")
                            .xsmall()
                            .ghost()
                            .label(theme_label)
                            .tooltip("Color theme: Light, Dark, or System")
                            .dropdown_menu(move |menu, _, _| {
                                let mut menu = menu
                                    .item(
                                        PopupMenuItem::new("Light")
                                            .checked(current == ThemePreference::Light)
                                            .on_click({
                                                let this = this.clone();
                                                move |_, window, cx| {
                                                    this.update(cx, |workspace, cx| {
                                                        workspace.set_theme_preference(
                                                            ThemePreference::Light,
                                                            window,
                                                            cx,
                                                        );
                                                    });
                                                }
                                            }),
                                    )
                                    .item(
                                        PopupMenuItem::new("Dark")
                                            .checked(current == ThemePreference::Dark)
                                            .on_click({
                                                let this = this.clone();
                                                move |_, window, cx| {
                                                    this.update(cx, |workspace, cx| {
                                                        workspace.set_theme_preference(
                                                            ThemePreference::Dark,
                                                            window,
                                                            cx,
                                                        );
                                                    });
                                                }
                                            }),
                                    );
                                if offer_system {
                                    menu = menu.item(
                                        PopupMenuItem::new("System")
                                            .checked(current == ThemePreference::System)
                                            .on_click({
                                                let this = this.clone();
                                                move |_, window, cx| {
                                                    this.update(cx, |workspace, cx| {
                                                        workspace.set_theme_preference(
                                                            ThemePreference::System,
                                                            window,
                                                            cx,
                                                        );
                                                    });
                                                }
                                            }),
                                    );
                                }
                                menu
                            })
                    })
                    .child(label),
            )
    }
}
