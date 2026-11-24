//! When to draw the in-window title bar (client-side decorations).

use gpui_kit::Decorations;

/// True when the platform is not drawing a title bar and the app must.
///
/// Prefer requesting [`gpui_kit::WindowDecorations::Server`] so X11 / KDE and
/// other compositors that support server-side decorations keep the native
/// frame. GNOME on Wayland (and any compositor without SSD) falls back to
/// [`Decorations::Client`]; only then do we render gpui-component's `TitleBar`.
pub(crate) fn should_show_client_title_bar(decorations: Decorations) -> bool {
    matches!(decorations, Decorations::Client { .. })
}

#[cfg(test)]
mod tests {
    use gpui_kit::{Decorations, Tiling};

    use super::should_show_client_title_bar;

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
}
