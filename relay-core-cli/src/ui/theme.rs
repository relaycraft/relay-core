//! TUI color palettes — preset themes aligned with relaycore.dev branding.

use ratatui::style::{Color, Modifier, Style};
use serde::Deserialize;
use std::sync::OnceLock;

/// Built-in TUI color presets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeId {
    /// relaycore.dev phosphor cyan on near-black (default).
    Relay,
    /// Previous slate + amber focus palette.
    Slate,
    /// Brighter accents and borders for low-contrast terminals.
    HighContrast,
}

impl ThemeId {
    pub const DEFAULT: Self = Self::Relay;

    pub fn id(self) -> &'static str {
        match self {
            Self::Relay => "relay",
            Self::Slate => "slate",
            Self::HighContrast => "high-contrast",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Relay => "relaycore.dev brand (cyan on near-black)",
            Self::Slate => "slate background with amber focus",
            Self::HighContrast => "brighter accents for weak displays",
        }
    }

    pub fn palette(self) -> &'static ThemePalette {
        match self {
            Self::Relay => &RELAY,
            Self::Slate => &SLATE,
            Self::HighContrast => &HIGH_CONTRAST,
        }
    }

    /// Parse a theme name (CLI flag, env, or config). Case-insensitive.
    pub fn parse(name: &str) -> Result<Self, String> {
        match name.trim().to_ascii_lowercase().as_str() {
            "relay" | "brand" | "default" => Ok(Self::Relay),
            "slate" | "legacy" => Ok(Self::Slate),
            "high-contrast" | "highcontrast" | "high_contrast" | "hc" => Ok(Self::HighContrast),
            other => Err(format!(
                "unknown TUI theme '{other}' (choices: relay, slate, high-contrast)"
            )),
        }
    }
}

/// Resolve theme: CLI/`RELAY_CORE_TUI_THEME` (via clap) → `~/.relay-core/config.toml` → default.
pub fn resolve_theme(cli_or_env: Option<String>) -> Result<ThemeId, String> {
    if let Some(name) = cli_or_env.filter(|s| !s.trim().is_empty()) {
        return ThemeId::parse(&name);
    }
    if let Some(name) = load_config_theme() {
        return ThemeId::parse(&name);
    }
    Ok(ThemeId::DEFAULT)
}

/// Activate a palette for the process. First call wins; safe to skip before TUI draw.
pub fn init(id: ThemeId) {
    let _ = ACTIVE.get_or_init(|| *id.palette());
}

fn palette() -> &'static ThemePalette {
    ACTIVE.get().unwrap_or(&RELAY)
}

static ACTIVE: OnceLock<ThemePalette> = OnceLock::new();

#[derive(Debug, Clone, Copy)]
pub struct ThemePalette {
    pub bg_elevated: Color,
    /// Zebra stripe (odd rows).
    pub row_alt: Color,
    pub row_selected: Color,
    pub text: Color,
    pub muted: Color,
    pub label: Color,
    pub header_key: Color,
    pub header_value: Color,
    pub accent: Color,
    pub accent_dim: Color,
    /// Unfocused panel / status bar borders (low contrast).
    pub border_subtle: Color,
    pub section: Color,
    pub uptime: Color,
    pub stat_ok: Color,
    pub stat_info: Color,
    pub error: Color,
    pub duration_slow: Color,
    pub method_get: Color,
    pub method_post: Color,
    pub method_put: Color,
    pub method_delete: Color,
    pub method_patch: Color,
    pub method_head: Color,
    pub method_options: Color,
    pub method_ws: Color,
    pub status_2xx: Color,
    pub status_3xx: Color,
    pub status_4xx_5xx: Color,
    pub json_string: Color,
    pub json_number: Color,
    pub json_bool: Color,
    pub host_colors: [Color; 8],
}

// relaycore.dev tokens (--accent-primary #00d4ff, --bg-elevated #141414, etc.)
const RELAY: ThemePalette = ThemePalette {
    bg_elevated: Color::Rgb(20, 20, 20),
    row_alt: Color::Rgb(24, 26, 30),
    row_selected: Color::Rgb(30, 34, 40),
    text: Color::Rgb(224, 224, 224),
    muted: Color::Rgb(102, 102, 102),
    label: Color::Rgb(0, 212, 255),
    header_key: Color::Rgb(0, 212, 255),
    header_value: Color::Rgb(224, 224, 224),
    accent: Color::Rgb(0, 212, 255),
    accent_dim: Color::Rgb(0, 153, 204),
    border_subtle: Color::Rgb(52, 56, 60),
    section: Color::Rgb(0, 212, 255),
    uptime: Color::Rgb(0, 255, 65),
    stat_ok: Color::Rgb(0, 255, 65),
    stat_info: Color::Rgb(0, 153, 204),
    error: Color::Rgb(255, 51, 102),
    duration_slow: Color::Rgb(255, 51, 102),
    method_get: Color::Rgb(0, 255, 65),
    method_post: Color::Rgb(255, 149, 0),
    method_put: Color::Rgb(0, 212, 255),
    method_delete: Color::Rgb(255, 51, 102),
    method_patch: Color::Rgb(0, 153, 204),
    method_head: Color::Rgb(192, 132, 252),
    method_options: Color::Rgb(128, 128, 128),
    method_ws: Color::Rgb(0, 212, 255),
    status_2xx: Color::Rgb(0, 255, 65),
    status_3xx: Color::Rgb(255, 149, 0),
    status_4xx_5xx: Color::Rgb(255, 51, 102),
    json_string: Color::Rgb(0, 255, 65),
    json_number: Color::Rgb(255, 149, 0),
    json_bool: Color::Rgb(0, 212, 255),
    host_colors: [
        Color::Rgb(0, 212, 255),
        Color::Rgb(0, 255, 65),
        Color::Rgb(255, 149, 0),
        Color::Rgb(255, 51, 102),
        Color::Rgb(192, 132, 252),
        Color::Rgb(0, 153, 204),
        Color::Rgb(224, 224, 224),
        Color::Rgb(128, 128, 128),
    ],
};

/// Previous default (Tokyo Night–style slate + amber).
const SLATE: ThemePalette = ThemePalette {
    bg_elevated: Color::Rgb(30, 33, 46),
    row_alt: Color::Rgb(34, 38, 52),
    row_selected: Color::Rgb(51, 65, 85),
    text: Color::Rgb(226, 232, 240),
    muted: Color::Rgb(148, 163, 184),
    label: Color::Rgb(125, 211, 252),
    header_key: Color::Rgb(186, 230, 253),
    header_value: Color::Rgb(203, 213, 225),
    accent: Color::Rgb(251, 191, 36),
    accent_dim: Color::Rgb(253, 224, 71),
    border_subtle: Color::Rgb(55, 62, 78),
    section: Color::Rgb(147, 197, 253),
    uptime: Color::Rgb(134, 239, 172),
    stat_ok: Color::Rgb(74, 222, 128),
    stat_info: Color::Rgb(56, 189, 248),
    error: Color::Rgb(248, 113, 113),
    duration_slow: Color::Rgb(248, 113, 113),
    method_get: Color::Rgb(96, 165, 250),
    method_post: Color::Rgb(74, 222, 128),
    method_put: Color::Rgb(250, 204, 21),
    method_delete: Color::Rgb(248, 113, 113),
    method_patch: Color::Rgb(45, 212, 191),
    method_head: Color::Rgb(192, 132, 252),
    method_options: Color::Rgb(226, 232, 240),
    method_ws: Color::Rgb(216, 180, 254),
    status_2xx: Color::Rgb(74, 222, 128),
    status_3xx: Color::Rgb(250, 204, 21),
    status_4xx_5xx: Color::Rgb(248, 113, 113),
    json_string: Color::Rgb(165, 214, 167),
    json_number: Color::Rgb(247, 140, 108),
    json_bool: Color::Rgb(130, 170, 255),
    host_colors: [
        Color::Rgb(96, 165, 250),
        Color::Rgb(74, 222, 128),
        Color::Rgb(250, 204, 21),
        Color::Rgb(248, 113, 113),
        Color::Rgb(192, 132, 252),
        Color::Rgb(45, 212, 191),
        Color::Rgb(251, 146, 60),
        Color::Rgb(148, 163, 184),
    ],
};

const HIGH_CONTRAST: ThemePalette = ThemePalette {
    bg_elevated: Color::Rgb(10, 10, 10),
    row_alt: Color::Rgb(18, 18, 18),
    row_selected: Color::Rgb(55, 55, 55),
    text: Color::Rgb(255, 255, 255),
    muted: Color::Rgb(180, 180, 180),
    label: Color::Rgb(0, 255, 255),
    header_key: Color::Rgb(0, 255, 255),
    header_value: Color::Rgb(255, 255, 255),
    accent: Color::Rgb(0, 255, 255),
    accent_dim: Color::Rgb(120, 255, 255),
    border_subtle: Color::Rgb(70, 70, 70),
    section: Color::Rgb(0, 255, 255),
    uptime: Color::Rgb(0, 255, 120),
    stat_ok: Color::Rgb(0, 255, 120),
    stat_info: Color::Rgb(100, 220, 255),
    error: Color::Rgb(255, 80, 120),
    duration_slow: Color::Rgb(255, 80, 120),
    method_get: Color::Rgb(100, 220, 255),
    method_post: Color::Rgb(255, 200, 0),
    method_put: Color::Rgb(0, 255, 255),
    method_delete: Color::Rgb(255, 80, 120),
    method_patch: Color::Rgb(0, 255, 200),
    method_head: Color::Rgb(220, 160, 255),
    method_options: Color::Rgb(220, 220, 220),
    method_ws: Color::Rgb(200, 160, 255),
    status_2xx: Color::Rgb(0, 255, 120),
    status_3xx: Color::Rgb(255, 200, 0),
    status_4xx_5xx: Color::Rgb(255, 80, 120),
    json_string: Color::Rgb(0, 255, 120),
    json_number: Color::Rgb(255, 180, 100),
    json_bool: Color::Rgb(140, 200, 255),
    host_colors: [
        Color::Rgb(100, 220, 255),
        Color::Rgb(0, 255, 120),
        Color::Rgb(255, 200, 0),
        Color::Rgb(255, 80, 120),
        Color::Rgb(220, 160, 255),
        Color::Rgb(0, 255, 200),
        Color::Rgb(255, 255, 255),
        Color::Rgb(180, 180, 180),
    ],
};

#[derive(Debug, Deserialize)]
struct ConfigFile {
    tui: Option<TuiConfig>,
}

#[derive(Debug, Deserialize)]
struct TuiConfig {
    theme: Option<String>,
}

fn load_config_theme() -> Option<String> {
    let path = relay_core_runtime::paths::resolve_data_dir().join("config.toml");
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return None, // missing file is normal
    };
    match toml::from_str::<ConfigFile>(&content) {
        Ok(cfg) => cfg.tui?.theme.filter(|s| !s.trim().is_empty()),
        Err(e) => {
            tracing::warn!(
                "failed to parse {}: {} — theme defaults will be used",
                path.display(),
                e
            );
            None
        }
    }
}

/// Style helpers bound to the active palette.
pub struct Theme;

impl Theme {
    pub fn bg_elevated() -> Color {
        palette().bg_elevated
    }

    pub fn label() -> Style {
        Style::default().fg(palette().label)
    }

    pub fn value() -> Style {
        Style::default().fg(palette().header_value)
    }

    pub fn text() -> Style {
        Style::default().fg(palette().text)
    }

    pub fn header_key() -> Style {
        Style::default().fg(palette().header_key)
    }

    /// L2 — panel / table titles.
    pub fn panel_title() -> Style {
        Style::default().fg(palette().accent_dim)
    }

    /// L3 — in-panel section labels (Overview, Help).
    pub fn subsection() -> Style {
        Style::default().fg(palette().muted)
    }

    pub fn accent() -> Style {
        Style::default().fg(palette().accent)
    }

    pub fn accent_bold() -> Style {
        Style::default()
            .fg(palette().accent)
            .add_modifier(Modifier::BOLD)
    }

    pub fn uptime() -> Style {
        Style::default().fg(palette().uptime)
    }

    /// Outer panel border when focused (soft accent, not full brightness).
    pub fn border(active: bool) -> Style {
        if active {
            Style::default().fg(palette().accent_dim)
        } else {
            Style::default().fg(palette().border_subtle)
        }
    }

    /// Inner detail blocks — always low-contrast.
    pub fn border_inner() -> Style {
        Style::default().fg(palette().border_subtle)
    }

    pub fn status_bar_border() -> Style {
        Style::default().fg(palette().border_subtle)
    }

    /// Selected row caret in the flow/rules list.
    pub fn row_marker() -> Style {
        Style::default()
            .fg(palette().accent)
            .add_modifier(Modifier::BOLD)
    }

    /// Apply zebra stripe background without clobbering badge cell backgrounds.
    pub fn on_zebra_row(base: Style, zebra: bool) -> Style {
        if zebra {
            base.bg(palette().row_alt)
        } else {
            base
        }
    }

    pub fn row_highlight() -> Style {
        Style::default()
            .bg(palette().row_selected)
            .fg(palette().text)
    }

    /// In-flight HTTP status (no response yet).
    pub fn pending_status() -> Style {
        Style::default()
            .fg(palette().accent_dim)
            .add_modifier(Modifier::ITALIC)
    }

    pub fn accent_dim() -> Style {
        Style::default().fg(palette().accent_dim)
    }

    /// Detail panel tab bar — inactive label.
    pub fn tab_inactive() -> Style {
        Style::default().fg(palette().muted)
    }

    /// Detail panel tab bar — selected pill.
    pub fn tab_active() -> Style {
        Style::default()
            .fg(palette().accent)
            .bg(palette().row_selected)
            .add_modifier(Modifier::BOLD)
    }

    pub fn duration_color(ms: u64) -> Color {
        let p = palette();
        if ms < 100 {
            p.stat_ok
        } else if ms < 500 {
            p.accent_dim
        } else {
            p.duration_slow
        }
    }

    pub fn duration_style(ms: Option<u64>) -> Style {
        let Some(ms) = ms else {
            return Self::muted();
        };
        let mut style = Style::default().fg(Self::duration_color(ms));
        if ms >= 2000 {
            style = style.add_modifier(Modifier::BOLD);
        }
        style
    }

    pub fn table_header() -> Style {
        Style::default()
            .fg(palette().accent_dim)
            .add_modifier(Modifier::BOLD)
    }

    fn tint_bg(fg: Color) -> Color {
        match fg {
            Color::Rgb(r, g, b) => Color::Rgb(r / 8, g / 8, b / 8),
            _ => palette().bg_elevated,
        }
    }

    pub fn method_badge(method: &str) -> Style {
        let fg = Self::method(method);
        Style::default().fg(fg).bg(Self::tint_bg(fg))
    }

    pub fn status_badge(code: &str) -> Style {
        if code == "---" || code == "…" {
            return Self::pending_status();
        }
        let fg = Self::status(code);
        Style::default().fg(fg).bg(Self::tint_bg(fg))
    }

    pub fn method(method: &str) -> Color {
        let p = palette();
        match method {
            "GET" => p.method_get,
            "POST" => p.method_post,
            "PUT" => p.method_put,
            "DELETE" => p.method_delete,
            "PATCH" => p.method_patch,
            "HEAD" => p.method_head,
            "OPTIONS" => p.method_options,
            "WS" => p.method_ws,
            _ => p.muted,
        }
    }

    pub fn status(code: &str) -> Color {
        let p = palette();
        if code.starts_with('2') {
            p.status_2xx
        } else if code.starts_with('3') {
            p.status_3xx
        } else if code.starts_with('4') || code.starts_with('5') {
            p.status_4xx_5xx
        } else {
            p.muted
        }
    }

    pub fn filter_hit() -> Style {
        Style::default()
            .fg(palette().accent_dim)
            .add_modifier(Modifier::BOLD)
    }

    pub fn muted_color() -> Color {
        palette().muted
    }

    pub fn muted() -> Style {
        Style::default().fg(palette().muted)
    }

    pub fn stat_ok() -> Style {
        Style::default().fg(palette().stat_ok)
    }

    pub fn stat_info() -> Style {
        Style::default().fg(palette().stat_info)
    }

    pub fn hotkey() -> Style {
        Style::default()
            .fg(palette().text)
            .add_modifier(Modifier::BOLD)
    }

    pub fn error() -> Style {
        Style::default().fg(palette().error)
    }

    pub fn error_bold() -> Style {
        Self::error().add_modifier(Modifier::BOLD)
    }

    pub fn ws_outbound() -> Style {
        Style::default().fg(palette().stat_ok)
    }

    pub fn ws_inbound() -> Style {
        Style::default().fg(palette().stat_info)
    }

    pub fn ws_opcode() -> Style {
        Style::default().fg(palette().accent_dim)
    }

    pub fn json_key() -> Style {
        Style::default().fg(palette().section)
    }

    pub fn json_string() -> Style {
        Style::default().fg(palette().json_string)
    }

    pub fn json_number() -> Style {
        Style::default().fg(palette().json_number)
    }

    pub fn json_bool() -> Style {
        Style::default().fg(palette().json_bool)
    }

    pub fn host_color(host: &str) -> Color {
        let colors = &palette().host_colors;
        let hash: u64 = host
            .bytes()
            .fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64));
        colors[hash as usize % colors.len()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_theme_names() {
        assert_eq!(ThemeId::parse("relay").unwrap(), ThemeId::Relay);
        assert_eq!(ThemeId::parse("SLATE").unwrap(), ThemeId::Slate);
        assert_eq!(
            ThemeId::parse("high-contrast").unwrap(),
            ThemeId::HighContrast
        );
        assert_eq!(ThemeId::parse("hc").unwrap(), ThemeId::HighContrast);
        assert!(ThemeId::parse("neon").is_err());
    }

    #[test]
    fn resolve_prefers_cli_over_config() {
        assert_eq!(resolve_theme(Some("slate".into())).unwrap(), ThemeId::Slate);
    }

    #[test]
    fn relay_palette_matches_site_accent() {
        assert_eq!(RELAY.accent, Color::Rgb(0, 212, 255));
        assert_eq!(RELAY.method_post, Color::Rgb(255, 149, 0));
    }
}
