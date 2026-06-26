mod body;
mod detail;
mod flow_list;
mod help;
mod status_bar;

use ratatui::{
    text::Span,
    widgets::{Block, BorderType, Borders, Padding},
};

use super::theme::Theme;

pub(super) use detail::render_flow_detail;
#[cfg(test)]
pub(crate) use detail::{PANEL_KV_LABEL_WIDTH, panel_kv_label_column};
pub(super) use flow_list::render_flow_list;
pub(super) use help::render_help_overlay;
pub(super) use status_bar::render_status_bar;

pub(super) fn outer_panel_block(title: &str, focused: bool) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Theme::border(focused))
        .title(Span::styled(format!(" {title} "), Theme::panel_title()))
        .style(Theme::surface())
}

pub(super) fn panel_body_padding() -> Padding {
    Padding {
        left: 1,
        right: 0,
        top: 0,
        bottom: 0,
    }
}
