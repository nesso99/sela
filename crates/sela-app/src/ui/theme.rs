//! Sela light/dark theme configs and palette gaps (M1-7 / docs/design/ux/06-theme-tokens.md).

use std::rc::Rc;

use gpui_kit::component::{try_parse_color, Theme, ThemeConfig, ThemeMode};
use gpui_kit::{App, Hsla, Window, WindowAppearance};

use sela_config::ThemePreference;

/// Colors with no `ThemeColor` field in gpui-component 0.10.
///
/// Extra status hues are kept for history / connection Test (M1-5+) even when
/// the grid only reads the decode and error-banner fields today.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub struct SelaPalette {
    pub text_disabled: Hsla,
    pub status_success: Hsla,
    pub status_warning: Hsla,
    pub status_error: Hsla,
    pub status_info: Hsla,
    pub status_error_wash: Hsla,
    pub cell_decode_text: Hsla,
    pub cell_decode_bg: Hsla,
    /// Inserted / duplicated row wash (`row.insert.bg`).
    pub row_insert_bg: Hsla,
    /// Dirty-cell triangle (`cell.dirty.mark`).
    pub cell_dirty_mark: Hsla,
}

impl SelaPalette {
    pub fn for_mode(mode: ThemeMode) -> Self {
        if mode.is_dark() {
            Self::parse_dark()
        } else {
            Self::parse_light()
        }
    }

    fn parse_light() -> Self {
        Self {
            text_disabled: color("#A3A3A3"),
            status_success: color("#15803D"),
            status_warning: color("#854D0E"),
            status_error: color("#B91C1C"),
            status_info: color("#0E7490"),
            status_error_wash: color("#FEF2F2"),
            cell_decode_text: color("#B91C1C"),
            cell_decode_bg: color("#FEF2F2"),
            row_insert_bg: color("#F0FDF4"),
            cell_dirty_mark: color("#854D0E"),
        }
    }

    fn parse_dark() -> Self {
        Self {
            text_disabled: color("#525252"),
            status_success: color("#4ADE80"),
            status_warning: color("#FACC15"),
            status_error: color("#F87171"),
            status_info: color("#22D3EE"),
            status_error_wash: color("#3F1212"),
            cell_decode_text: color("#F87171"),
            cell_decode_bg: color("#3F1212"),
            row_insert_bg: color("#052E16"),
            cell_dirty_mark: color("#FACC15"),
        }
    }
}

fn color(hex: &str) -> Hsla {
    try_parse_color(hex).unwrap_or_else(|_| Hsla::default())
}

/// Build the two ThemeConfigs (kit defaults + Sela overrides) and apply the
/// active preference.
pub fn install_sela_themes(preference: ThemePreference, window: Option<&mut Window>, cx: &mut App) {
    let light = Rc::new(sela_theme_config(ThemeMode::Light));
    let dark = Rc::new(sela_theme_config(ThemeMode::Dark));
    Theme::update(cx, |theme| {
        theme.light_theme = light;
        theme.dark_theme = dark;
    });
    apply_preference(preference, window, cx);
}

pub fn apply_preference(preference: ThemePreference, window: Option<&mut Window>, cx: &mut App) {
    match preference {
        ThemePreference::Light => Theme::change(ThemeMode::Light, window, cx),
        ThemePreference::Dark => Theme::change(ThemeMode::Dark, window, cx),
        ThemePreference::System => {
            Theme::sync_system_appearance(window, cx);
        }
    }
}

/// Whether this build can report window appearance (System menu item).
pub fn system_appearance_available(window: &Window) -> bool {
    // gpui-kit 0.10 always exposes Window::appearance on desktop targets.
    // Touch the value so System mode can rely on the same API surface.
    let _ = system_is_dark(window);
    true
}

/// Whether the OS/window appearance currently reports dark.
pub fn system_is_dark(window: &Window) -> bool {
    match window.appearance() {
        WindowAppearance::Dark | WindowAppearance::VibrantDark => true,
        WindowAppearance::Light | WindowAppearance::VibrantLight => false,
    }
}

fn sela_theme_config(mode: ThemeMode) -> ThemeConfig {
    let (name, overrides) = match mode {
        ThemeMode::Light => ("Sela Light", light_overrides()),
        ThemeMode::Dark => ("Sela Dark", dark_overrides()),
    };
    let value = serde_json::json!({
        "name": name,
        "mode": if mode.is_dark() { "dark" } else { "light" },
        "font.size": 13,
        "mono_font.size": 12,
        "radius": 4,
        "radius.lg": 6,
        "window.radius": 0,
        "colors": overrides,
        "highlight": highlight_block(mode),
    });
    // Merge onto a default so unspecified kit colors remain.
    let mut config: ThemeConfig =
        serde_json::from_value(value).expect("Sela ThemeConfig JSON is valid");
    config.mode = mode;
    config
}

fn light_overrides() -> serde_json::Value {
    serde_json::json!({
        "muted.foreground": "#525252",
        "table.head.foreground": "#525252",
        "ring": "#2563EB",
        "input.border": "#767676",
        // A missing primary.foreground falls back to foreground (near-black),
        // which disappears on the near-black primary fill. Keep the kit pair.
        "primary.foreground": "#FAFAFA",
    })
}

fn dark_overrides() -> serde_json::Value {
    serde_json::json!({
        "sidebar.background": "#171717",
        "muted.foreground": "#A3A3A3",
        // A missing primary.foreground falls back to foreground (near-white),
        // which disappears on the near-white primary fill.
        "primary.foreground": "#171717",
        "table.even.background": "#171717",
        "table.hover.background": "#262626",
        "table.head.background": "#171717",
        "table.head.foreground": "#A3A3A3",
        "table.active.background": "#1d4ed88c",
        "list.hover.background": "#262626",
        "list.even.background": "#171717",
        "ring": "#60A5FA",
        "input.border": "#737373",
    })
}

fn highlight_block(mode: ThemeMode) -> serde_json::Value {
    match mode {
        ThemeMode::Light => serde_json::json!({
            "editor.background": "#FFFFFF",
            "editor.foreground": "#000000",
            "editor.line_number": "#525252",
            "syntax": {
                "keyword": { "color": "#0433FF" },
                "string": { "color": "#036A07" },
                "number": { "color": "#0433FF" },
                "comment": { "color": "#525252" },
                "comment.doc": { "color": "#525252" },
            }
        }),
        ThemeMode::Dark => serde_json::json!({
            "editor.background": "#0A0A0A",
            "editor.foreground": "#CACCCA",
            "editor.line_number": "#8F8F8F",
            "syntax": {
                "keyword": { "color": "#87B1F6" },
                "string": { "color": "#A3E09F" },
                "number": { "color": "#CC9E00" },
                "comment": { "color": "#9D9D9D" },
                "comment.doc": { "color": "#9D9D9D" },
            }
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::component::ThemeColor;
    use sela_config::resolve_theme_mode;

    #[test]
    fn theme_configs_deserialize() {
        let light = sela_theme_config(ThemeMode::Light);
        assert!(!light.mode.is_dark());
        assert_eq!(light.font_size, Some(13.));
        assert_eq!(light.mono_font_size, Some(12.));
        assert_eq!(light.radius, Some(4));
        assert!(light.highlight.is_some());

        let dark = sela_theme_config(ThemeMode::Dark);
        assert!(dark.mode.is_dark());
        assert!(dark.colors.sidebar.is_some());
        assert!(dark.colors.table_even.is_some());
        assert!(dark.colors.table_active.is_some());
    }

    #[test]
    fn palette_contrast_hues_parse() {
        let light = SelaPalette::for_mode(ThemeMode::Light);
        assert!(light.status_error.a > 0.9);
        let dark = SelaPalette::for_mode(ThemeMode::Dark);
        assert!(dark.cell_decode_bg.a > 0.9);
    }

    #[test]
    fn primary_button_label_contrasts_with_fill() {
        // apply_config replaces an omitted primary.foreground with foreground.
        // That is the same hue as the primary fill in both modes, so New, Run,
        // and the selected pane button paint white text on a white fill.
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            let mut theme = Theme::from(ThemeColor::light().as_ref());
            let config = Rc::new(sela_theme_config(mode));
            theme.apply_config(&config);
            let label = if mode.is_dark() {
                color("#171717")
            } else {
                color("#FAFAFA")
            };
            assert_eq!(theme.primary_foreground, label, "{mode:?}");
            assert_eq!(theme.button_primary_foreground, label, "{mode:?}");
            assert_ne!(
                theme.button_primary_foreground, theme.button_primary,
                "{mode:?} primary label matches its fill"
            );
        }
    }

    #[test]
    fn preference_resolution_matches_settings_helper() {
        assert!(!resolve_theme_mode(ThemePreference::Light, true));
        assert!(resolve_theme_mode(ThemePreference::System, true));
        assert!(resolve_theme_mode(ThemePreference::Dark, false));
        assert!(!resolve_theme_mode(ThemePreference::System, false));
    }
}
