//! Bottom status bar.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{h_flex, ActiveTheme, Disableable as _, Sizable as _};
use gpui_kit::prelude::*;
use gpui_kit::{px, Context, IntoElement, Window};

use sela_config::{
    clamp_font_size, AppSettings, ThemePreference, DEFAULT_FONT_SIZE, MAX_FONT_SIZE, MIN_FONT_SIZE,
};
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
        let _ = self.settings.save(AppSettings {
            theme: preference,
            font_size: self.font_size,
        });
        super::super::theme::apply_preference(preference, Some(window), cx);
        cx.notify();
    }

    pub(super) fn set_font_size(
        &mut self,
        font_size: u8,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let font_size = clamp_font_size(i64::from(font_size));
        if self.font_size == font_size {
            return;
        }
        self.font_size = font_size;
        let _ = self.settings.save(AppSettings {
            theme: self.theme_preference,
            font_size,
        });
        super::super::theme::install_sela_themes(
            self.theme_preference,
            font_size,
            Some(window),
            cx,
        );
        cx.notify();
    }

    pub(super) fn bump_font_size(
        &mut self,
        delta: i8,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let next = i64::from(self.font_size) + i64::from(delta);
        self.set_font_size(clamp_font_size(next), window, cx);
    }

    pub(super) fn reset_font_size(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_font_size(DEFAULT_FONT_SIZE, window, cx);
    }

    pub(super) fn status_bar(&self, cx: &Context<Self>) -> impl IntoElement {
        let this = cx.entity();
        let view = self.active_view();
        let view_status = view.map(|v| v.status.as_str());
        // Prefer an explicit status; when tabs are gone after Disconnect, still
        // surface workspace status (Duplicate) or a session-only keychain warning.
        let status =
            super::state::resolve_workspace_status(view_status, self.workspace_status.as_deref())
                .or_else(|| self.keychain_session_warning.clone())
                .unwrap_or_default();
        let label = view
            .map(|v| v.source_label())
            .unwrap_or_else(|| "No table open".into());
        let connected = self.session.is_some();
        let busy = view.map(|v| v.busy).unwrap_or(false);
        let transfer_active = self.transfer_job.is_some();
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
        let font_size = self.font_size;
        let font_label = font_size.to_string();

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
                    })
                    .when(transfer_active, |row| {
                        // Status-bar Cancel for export / import (EX-06 / IM-04).
                        let this = this.clone();
                        row.child({
                            Button::new("status-transfer-cancel")
                                .xsmall()
                                .ghost()
                                .label("Cancel")
                                .tooltip("Cancel export or import (Ctrl/⌘+.)")
                                .on_click(move |_, _, cx| {
                                    this.update(cx, |workspace, cx| {
                                        workspace.stop_query(cx);
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
                        Button::new("font-size-menu")
                            .xsmall()
                            .ghost()
                            .label(font_label)
                            .tooltip("Font size (Ctrl/⌘+= / − / 0)")
                            .dropdown_menu(move |menu, _, _| {
                                let mut menu = menu;
                                for size in MIN_FONT_SIZE..=MAX_FONT_SIZE {
                                    let this = this.clone();
                                    menu = menu.item(
                                        PopupMenuItem::new(size.to_string())
                                            .checked(current_font_checked(font_size, size))
                                            .on_click(move |_, window, cx| {
                                                this.update(cx, |workspace, cx| {
                                                    workspace.set_font_size(size, window, cx);
                                                });
                                            }),
                                    );
                                }
                                let this = this.clone();
                                menu.item(PopupMenuItem::new("Reset").on_click(
                                    move |_, window, cx| {
                                        this.update(cx, |workspace, cx| {
                                            workspace.reset_font_size(window, cx);
                                        });
                                    },
                                ))
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

fn current_font_checked(current: u8, size: u8) -> bool {
    current == size
}
