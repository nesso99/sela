//! Named connection color swatches (M2-5). Hex pairs live in the app palette.

use serde::{Deserialize, Deserializer, Serialize};

/// Named swatch stored on a saved connection. Never a free hex.
///
/// Wire values: `none`, `blue`, `green`, `amber`, `red`, `purple`, `teal`.
/// An unknown stored name loads as [`ConnectionColor::None`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ConnectionColor {
    #[default]
    None,
    Blue,
    Green,
    Amber,
    /// Production mark. See [`crate::default_safety_level_for_color`].
    Red,
    Purple,
    Teal,
}

impl ConnectionColor {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Blue => "blue",
            Self::Green => "green",
            Self::Amber => "amber",
            Self::Red => "red",
            Self::Purple => "purple",
            Self::Teal => "teal",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Blue => "Blue",
            Self::Green => "Green",
            Self::Amber => "Amber",
            Self::Red => "Red",
            Self::Purple => "Purple",
            Self::Teal => "Teal",
        }
    }

    pub fn all() -> &'static [ConnectionColor] {
        &[
            Self::None,
            Self::Blue,
            Self::Green,
            Self::Amber,
            Self::Red,
            Self::Purple,
            Self::Teal,
        ]
    }

    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "blue" => Self::Blue,
            "green" => Self::Green,
            "amber" => Self::Amber,
            "red" => Self::Red,
            "purple" => Self::Purple,
            "teal" => Self::Teal,
            "none" | "" => Self::None,
            _ => Self::None,
        }
    }

    pub fn is_none(&self) -> bool {
        *self == Self::None
    }
}

impl<'de> Deserialize<'de> for ConnectionColor {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Ok(Self::parse(&raw))
    }
}

/// Default write-guard level suggested by a connection color.
///
/// Red (production) defaults to Confirm writes. Every other swatch leaves Off.
/// M2-4 owns the stored `safe_mode` field and confirm UI; call this when a
/// **new** connection's color is set to Red before the user touches Safe mode
/// (see docs/design/ux/09-safe-mode.md § Where it is set).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefaultSafetyLevel {
    Off,
    ConfirmWrites,
}

impl DefaultSafetyLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::ConfirmWrites => "confirm",
        }
    }
}

/// Hook for M2-4: the default safety level for a color tag on a new connection.
pub fn default_safety_level_for_color(color: ConnectionColor) -> DefaultSafetyLevel {
    match color {
        ConnectionColor::Red => DefaultSafetyLevel::ConfirmWrites,
        _ => DefaultSafetyLevel::Off,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_color_loads_as_none() {
        let color: ConnectionColor = serde_json::from_str("\"magenta\"").unwrap();
        assert_eq!(color, ConnectionColor::None);
        assert_eq!(
            serde_json::to_string(&ConnectionColor::Red).unwrap(),
            "\"red\""
        );
    }

    #[test]
    fn red_defaults_to_confirm_writes() {
        assert_eq!(
            default_safety_level_for_color(ConnectionColor::Red),
            DefaultSafetyLevel::ConfirmWrites
        );
        assert_eq!(
            default_safety_level_for_color(ConnectionColor::Blue),
            DefaultSafetyLevel::Off
        );
        assert_eq!(
            default_safety_level_for_color(ConnectionColor::None),
            DefaultSafetyLevel::Off
        );
        assert_eq!(DefaultSafetyLevel::ConfirmWrites.as_str(), "confirm");
    }
}
