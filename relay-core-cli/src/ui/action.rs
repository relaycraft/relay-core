use super::format::BodyView;

/// Narrow action boundary for TUI commands and future async/control effects.
///
/// Keep cheap cursor/focus mutations in `TuiApp` for now. Actions are for
/// operations that have side effects, touch shared collections, or are likely
/// to become background/core commands later.
#[non_exhaustive]
pub enum TuiAction {
    Quit,
    ClearFlows,
    SetPaused(bool),
    ApplyFilter(String),
    ClearFilter,
    ApplyTheme(String),
    CopySelectedCurl,
    SetBodyView(BodyView),
    ShowCommandHelp,
    SetToast(String),
    ReplaySelectedFlow,
}
