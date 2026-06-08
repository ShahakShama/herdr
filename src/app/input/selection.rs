use crate::{app::state::AppState, terminal::TerminalRuntimeRegistry};

impl AppState {
    pub(crate) fn update_selection_cursor(
        &mut self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        pane_id: crate::layout::PaneId,
        screen_col: u16,
        screen_row: u16,
    ) {
        let Some(info) = self.pane_info_by_id(pane_id).cloned() else {
            return;
        };
        let metrics = self.pane_scroll_metrics(terminal_runtimes, pane_id);
        if let Some(selection) = self.selection.as_mut() {
            selection.drag(screen_col, screen_row, info.inner_rect, metrics);
        }
    }
}
