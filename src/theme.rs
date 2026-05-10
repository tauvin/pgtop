//! Color theme — семантические цвета, разные для dark/light терминалов.
//!
//! ANSI-цвета (Red/Green/Yellow) обычно одинаково смотрятся на любом фоне —
//! их рендерит сам терминал согласно своей цветовой схеме. Реальная разница
//! между dark и light: «dim» (тусклые) элементы. `Color::DarkGray` отлично
//! читается на тёмном фоне, но плохо — на светлом; для light theme'ы
//! используем `Color::Gray`.
//!
//! Палитра намеренно небольшая: семантические роли (success/warning/danger),
//! а не «caliper-точные» цвета. Расширять по мере надобности.

use ratatui::style::Color;

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    /// Тусклые элементы: self-row в Activity, фоновые подписи. Главное
    /// различие dark vs light.
    pub muted: Color,

    /// Успешный исход (✓ в action result, active < 10s в Activity).
    pub success: Color,

    /// Предупреждение (⚠ в action result, idle in transaction в Activity).
    pub warning: Color,

    /// Ошибка/опасность (✗ в action result, long query > 10s, waiting lock).
    pub danger: Color,
}

impl Theme {
    /// Тёмная тема — default.
    pub const fn dark() -> Self {
        Self {
            muted: Color::DarkGray,
            success: Color::Green,
            warning: Color::Yellow,
            danger: Color::Red,
        }
    }

    /// Светлая тема — основное отличие в `muted`. Семантические цвета
    /// (Green/Yellow/Red) остаются ANSI-стандартом: терминал рендерит их
    /// в обоих фонах нормально читаемо.
    pub const fn light() -> Self {
        Self {
            muted: Color::Gray,
            success: Color::Green,
            warning: Color::Yellow,
            danger: Color::Red,
        }
    }

    /// Парсинг из строки конфига. Unknown name → fallback на dark
    /// (типичная UX для опционального config-поля).
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
