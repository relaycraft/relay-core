//! TUI color palette — tuned for dark terminals (readable muted text, not near-black gray).

use ratatui::style::{Color, Modifier, Style};

/// Cohesive dark-theme colors (slate + soft accent).
pub struct Theme;

impl Theme {
    // Surfaces
    pub const BG_ELEVATED: Color = Color::Rgb(30, 33, 46);
    pub const ROW_SELECTED: Color = Color::Rgb(51, 65, 85);

    // Typography hierarchy
    pub const TEXT: Color = Color::Rgb(226, 232, 240);
    pub const MUTED: Color = Color::Rgb(148, 163, 184);
    pub const LABEL: Color = Color::Rgb(125, 211, 252);
    pub const HEADER_KEY: Color = Color::Rgb(186, 230, 253);
    pub const HEADER_VALUE: Color = Color::Rgb(203, 213, 225);

    // Chrome / focus
    pub const ACCENT: Color = Color::Rgb(251, 191, 36);
    pub const ACCENT_DIM: Color = Color::Rgb(253, 224, 71);
    pub const SECTION: Color = Color::Rgb(147, 197, 253);
    pub const UPTIME: Color = Color::Rgb(134, 239, 172);
    pub const STAT_OK: Color = Color::Rgb(74, 222, 128);
    pub const STAT_INFO: Color = Color::Rgb(56, 189, 248);

    pub fn label() -> Style {
        Style::default().fg(Self::LABEL)
    }

    pub fn value() -> Style {
        Style::default().fg(Self::HEADER_VALUE)
    }

    pub fn text() -> Style {
        Style::default().fg(Self::TEXT)
    }

    pub fn header_key() -> Style {
        Style::default().fg(Self::HEADER_KEY)
    }

    pub fn section() -> Style {
        Style::default()
            .fg(Self::SECTION)
            .add_modifier(Modifier::BOLD)
    }

    pub fn accent() -> Style {
        Style::default().fg(Self::ACCENT)
    }

    pub fn accent_bold() -> Style {
        Style::default()
            .fg(Self::ACCENT)
            .add_modifier(Modifier::BOLD)
    }

    pub fn uptime() -> Style {
        Style::default().fg(Self::UPTIME)
    }

    pub fn border(active: bool) -> Style {
        if active {
            Style::default().fg(Self::ACCENT)
        } else {
            Style::default().fg(Self::MUTED)
        }
    }

    pub fn row_highlight() -> Style {
        Style::default()
            .fg(Self::TEXT)
            .add_modifier(Modifier::BOLD)
            .bg(Self::ROW_SELECTED)
    }

    pub fn table_header() -> Style {
        Style::default()
            .fg(Self::LABEL)
            .add_modifier(Modifier::BOLD)
    }

    pub fn method(method: &str) -> Color {
        match method {
            "GET" => Color::Rgb(96, 165, 250),
            "POST" => Color::Rgb(74, 222, 128),
            "PUT" => Color::Rgb(250, 204, 21),
            "DELETE" => Color::Rgb(248, 113, 113),
            "PATCH" => Color::Rgb(45, 212, 191),
            "HEAD" => Color::Rgb(192, 132, 252),
            "OPTIONS" => Color::Rgb(226, 232, 240),
            "WS" => Color::Rgb(216, 180, 254),
            _ => Self::MUTED,
        }
    }

    pub fn status(code: &str) -> Color {
        if code.starts_with('2') {
            Color::Rgb(74, 222, 128)
        } else if code.starts_with('3') {
            Color::Rgb(250, 204, 21)
        } else if code.starts_with('4') || code.starts_with('5') {
            Color::Rgb(248, 113, 113)
        } else {
            Self::MUTED
        }
    }

    pub fn filter_hit() -> Style {
        Style::default()
            .fg(Self::ACCENT_DIM)
            .add_modifier(Modifier::BOLD)
    }

    pub fn muted() -> Style {
        Style::default().fg(Self::MUTED)
    }

    pub fn stat_ok() -> Style {
        Style::default().fg(Self::STAT_OK)
    }

    pub fn stat_info() -> Style {
        Style::default().fg(Self::STAT_INFO)
    }

    pub fn hotkey() -> Style {
        Style::default().fg(Self::TEXT).add_modifier(Modifier::BOLD)
    }

    pub fn error() -> Style {
        Style::default().fg(Color::Rgb(248, 113, 113))
    }

    pub fn error_bold() -> Style {
        Self::error().add_modifier(Modifier::BOLD)
    }

    pub fn ws_outbound() -> Style {
        Style::default().fg(Self::STAT_OK)
    }

    pub fn ws_inbound() -> Style {
        Style::default().fg(Self::STAT_INFO)
    }

    pub fn ws_opcode() -> Style {
        Style::default().fg(Self::ACCENT_DIM)
    }
}
