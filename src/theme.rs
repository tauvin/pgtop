//! Color theme — semantic colors with separate dark and light variants.

use ratatui::style::Color;

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    /// Dim elements: self-rows in Activity, secondary labels.
    pub muted: Color,

    /// Successful outcome.
    pub success: Color,

    /// Warning state.
    pub warning: Color,

    /// Error or danger.
    pub danger: Color,
}

impl Theme {
    /// Dark theme — the default.
    pub const fn dark() -> Self {
        Self {
            muted: Color::DarkGray,
            success: Color::Green,
            warning: Color::Yellow,
            danger: Color::Red,
        }
    }

    /// Light theme.
    pub const fn light() -> Self {
        Self {
            muted: Color::Gray,
            success: Color::Green,
            warning: Color::Yellow,
            danger: Color::Red,
        }
    }

    /// Parse a theme by name. Unknown names fall back to dark.
    pub fn from_name(name: &str) -> Self {
        match name {
            "light" => Self::light(),
            _ => Self::dark(),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}
