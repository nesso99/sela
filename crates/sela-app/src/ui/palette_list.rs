//! Command-palette list colors (opaque selection wash + match highlight).

use gpui_kit::component::Theme;
use gpui_kit::Hsla;

use crate::ui::theme::SelaPalette;

/// Colors used to paint the palette quick-open list (render + contrast tests).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PaletteListColors {
    pub panel_background: Hsla,
    pub selected_row_background: Hsla,
    pub match_highlight: Hsla,
    pub selection_indicator: Hsla,
}

pub(crate) fn palette_list_colors(theme: &Theme) -> PaletteListColors {
    let sela = SelaPalette::for_mode(theme.mode);
    PaletteListColors {
        panel_background: theme.background,
        selected_row_background: sela.palette_list_selected_bg,
        match_highlight: sela.palette_list_match_highlight,
        selection_indicator: theme.ring,
    }
}

#[cfg(test)]
pub(crate) mod contrast {
    use super::PaletteListColors;
    use gpui_kit::Hsla;

    /// WCAG relative luminance (opaque sRGB).
    pub(crate) fn relative_luminance(c: Hsla) -> f32 {
        let rgba = c.to_rgb();
        let channel = |v: f32| {
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        let r = channel(rgba.r);
        let g = channel(rgba.g);
        let b = channel(rgba.b);
        0.2126 * r + 0.7152 * g + 0.0722 * b
    }

    pub(crate) fn contrast_ratio(a: Hsla, b: Hsla) -> f32 {
        let l1 = relative_luminance(a);
        let l2 = relative_luminance(b);
        let (hi, lo) = if l1 > l2 { (l1, l2) } else { (l2, l1) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// Alpha-composite `fg` over `bg` luminance (for kit tokens capped at 20% opacity).
    pub(crate) fn composite_luminance(fg: Hsla, bg: Hsla) -> f32 {
        let fa = fg.a.clamp(0., 1.);
        let f = fg.to_rgb();
        let b = bg.to_rgb();
        let inv = 1.0 - fa;
        let mix = |x: f32, y: f32| x * fa + y * inv;
        let channel = |v: f32| {
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        let r = channel(mix(f.r, b.r));
        let g = channel(mix(f.g, b.g));
        let bl = channel(mix(f.b, b.b));
        0.2126 * r + 0.7152 * g + 0.0722 * bl
    }

    #[cfg(test)]
    pub(crate) fn assert_palette_list_contrast(
        colors: PaletteListColors,
        mode: gpui_kit::component::ThemeMode,
    ) {
        let panel = colors.panel_background;
        let selected = colors.selected_row_background;
        let highlight = colors.match_highlight;
        let indicator = colors.selection_indicator;

        assert!(
            contrast_ratio(highlight, panel) >= 4.5,
            "{mode:?} match on panel: {}",
            contrast_ratio(highlight, panel)
        );
        assert!(
            contrast_ratio(highlight, selected) >= 4.5,
            "{mode:?} match on selected row: {}",
            contrast_ratio(highlight, selected)
        );
        assert!(
            contrast_ratio(indicator, panel) >= 3.0,
            "{mode:?} selection indicator on panel: {}",
            contrast_ratio(indicator, panel)
        );
        assert!(
            contrast_ratio(selected, panel) >= 1.2,
            "{mode:?} selected row on panel: {}",
            contrast_ratio(selected, panel)
        );
    }
}
