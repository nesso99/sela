//! When to draw the in-window title bar (client-side decorations).

use gpui_kit::{Decorations, MouseButton};

/// True when the platform is not drawing a title bar and the app must.
///
/// Prefer requesting [`gpui_kit::WindowDecorations::Server`] so X11 / KDE and
/// other compositors that support server-side decorations keep the native
/// frame. GNOME on Wayland (and any compositor without SSD) falls back to
/// [`Decorations::Client`]; only then do we render gpui-component's `TitleBar`.
pub(crate) fn should_show_client_title_bar(decorations: Decorations) -> bool {
    matches!(decorations, Decorations::Client { .. })
}

/// Product decision: no title-bar right-click menu under CSD.
///
/// gpui-component's `TitleBar` registers a Linux client-decoration overlay that
/// calls `window.show_window_menu` on right mouse down. When this returns true,
/// Sela stops that event in the capture phase so the platform menu never opens.
pub(crate) fn should_suppress_title_bar_window_menu(button: MouseButton) -> bool {
    matches!(button, MouseButton::Right)
}

#[cfg(test)]
mod tests {
    use gpui_kit::{Decorations, MouseButton, Tiling};

    use super::{should_show_client_title_bar, should_suppress_title_bar_window_menu};

    #[test]
    fn server_decorations_hide_client_title_bar() {
        assert!(!should_show_client_title_bar(Decorations::Server));
    }

    #[test]
    fn client_decorations_show_client_title_bar() {
        assert!(should_show_client_title_bar(Decorations::Client {
            tiling: Tiling::default(),
        }));
    }

    #[test]
    fn tiled_client_decorations_still_show_title_bar() {
        assert!(should_show_client_title_bar(Decorations::Client {
            tiling: Tiling::tiled(),
        }));
    }

    #[test]
    fn right_click_suppresses_title_bar_window_menu() {
        assert!(should_suppress_title_bar_window_menu(MouseButton::Right));
    }

    #[test]
    fn left_click_does_not_suppress_title_bar_interactions() {
        assert!(!should_suppress_title_bar_window_menu(MouseButton::Left));
        assert!(!should_suppress_title_bar_window_menu(MouseButton::Middle));
    }
}
