use crate::terminal::TerminalId;

/// The role a pane plays inside a workspace's stacked layout.
///
/// A workspace can hold up to four stacked rows in fixed order top→bottom:
/// `Review` (topmost) / `Plan` / `Terminal` / `Agent` (always bottom). The role
/// determines split placement and keeps the extra rows out of the agents list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum PaneRole {
    #[default]
    Agent,
    Review,
    Terminal,
    /// An nvim row reviewing the plan the agent wrote (alt+p); decided with
    /// alt+s. See [`crate::plan_review`].
    Plan,
}

/// Viewport state for a pane.
///
/// Terminal identity, cwd, labels, and agent metadata live in TerminalState.
pub struct PaneState {
    pub attached_terminal_id: TerminalId,
    /// Whether the user has seen this pane since its last state change to Idle.
    /// False = "Done" (agent finished while user was in another workspace).
    pub seen: bool,
    /// What this pane represents within its workspace's stacked layout.
    pub role: PaneRole,
}

impl PaneState {
    pub fn new(attached_terminal_id: TerminalId) -> Self {
        Self {
            attached_terminal_id,
            seen: true,
            role: PaneRole::Agent,
        }
    }
}
