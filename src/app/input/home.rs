//! Keyboard-first home input: directional pane focus, list selection, and the
//! alt-direct command chords for the control surface.
//!
//! Layout panes: Control (top-left, repos), Agents (bottom-left), Main (right).
//! `alt+h/j/k/l` moves focus between them; arrows move the selection inside the
//! focused list; Enter activates the selection but leaves focus where it is.

use crossterm::event::{KeyCode, KeyModifiers};

use crate::app::state::{AppState, FocusPane, LeftHalf, Mode, ReviewState};
use crate::app::App;
use crate::input::TerminalKey;

impl App {
    /// Async entry for home-mode keys. Applies the command, and (TODO, next
    /// phase) forwards unconsumed keys to the focused agent pane when Main has
    /// focus.
    pub(super) async fn handle_home_key(&mut self, key: TerminalKey) {
        // The decision logic is shared with the headless server path
        // (`handle_home_key_headless`); only the byte-send differs (async here).
        if self.dispatch_home_key(key) {
            self.forward_key_to_main(key).await;
        }
    }

    /// Headless-server variant of [`Self::handle_home_key`]: same routing, but
    /// keys destined for Main are sent to the pane synchronously (the server has
    /// no async context here). Without this, typing/PR/terminal commands in the
    /// home surface are silently dropped by the headless input loop.
    pub(crate) fn handle_home_key_headless(&mut self, key: TerminalKey) {
        if self.dispatch_home_key(key) {
            self.forward_key_to_main_headless(key);
        }
    }

    /// Apply a home-surface key. Returns `true` when the caller should forward
    /// the (unconsumed) key to the focused Main pane, `false` when it was fully
    /// handled here. App-level commands (PR via `gh`, opening a terminal) run
    /// directly; pure state changes go through [`AppState::apply_home_key`].
    fn dispatch_home_key(&mut self, key: TerminalKey) -> bool {
        let event = key.as_key_event();
        // `p` (plain in the agents pane, or alt+p anywhere) submits a PR for the
        // focused agent's branch; this runs `gh`, so it happens at the App level.
        if event.code == KeyCode::Char('p') && self.state.control.focus == FocusPane::Agents {
            self.submit_pr_for_selected_agent();
            return false;
        }

        // `t` in the Control pane opens a plain terminal in the selected repo.
        // This spawns a shell, so it runs at the App level.
        if event.code == KeyCode::Char('t') && self.state.control.focus == FocusPane::Control {
            self.open_terminal_in_selected_repo();
            return false;
        }

        // alt+s enters copy mode (keyboard scrollback + selection + yank) on the
        // focused Main pane. enter_copy_mode needs the runtime registry, so it
        // runs here at the App level rather than in `apply_home_key`.
        if event.code == KeyCode::Char('s') && event.modifiers.contains(KeyModifiers::ALT) {
            self.state.enter_copy_mode(&self.terminal_runtimes);
            return false;
        }

        // With vim focused in Main, Alt+h/j/k/l drive vim's window navigation
        // rather than herdr's pane focus. vim signals back (an OSC our vimrc
        // emits, handled as AppEvent::PaneFocusSignal) when it has no window in
        // that direction, so leaving the leftmost window returns to the sidebar.
        if self.state.control.focus == FocusPane::Main
            && event.modifiers.contains(KeyModifiers::ALT)
            && matches!(event.code, KeyCode::Char('h' | 'j' | 'k' | 'l'))
            && self.focused_main_is_vim()
        {
            return true;
        }

        if self.state.apply_home_key(key) {
            return false;
        }
        // Unconsumed keys with Main focused are typed into the agent pane.
        self.state.control.focus == FocusPane::Main
    }

    /// Whether vim/nvim is the foreground program in the focused Main pane.
    fn focused_main_is_vim(&self) -> bool {
        self.state
            .active
            .and_then(|ws_idx| {
                self.state
                    .focused_runtime_in_workspace(&self.terminal_runtimes, ws_idx)
            })
            .is_some_and(|rt| rt.foreground_command_is_vim())
    }

    /// Encode and forward a key straight to the focused agent pane, bypassing the
    /// legacy prefix/command interception (the prefix is dropped in this UI).
    async fn forward_key_to_main(&mut self, key: TerminalKey) {
        let Some(ws_idx) = self.state.active else {
            return;
        };
        let Some(pane_id) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.focused_pane_id())
        else {
            return;
        };
        let bytes = {
            let Some(rt) =
                self.state
                    .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)
            else {
                return;
            };
            rt.scroll_reset();
            rt.encode_terminal_key(key)
        };
        if bytes.is_empty() {
            return;
        }
        if let Some(sender) = self.lookup_runtime_sender(ws_idx, pane_id) {
            let _ = sender.send_bytes(bytes::Bytes::from(bytes)).await;
        }
    }

    /// Synchronous sibling of [`Self::forward_key_to_main`] for the headless
    /// server input loop, which has no async context to `.await` the send.
    fn forward_key_to_main_headless(&mut self, key: TerminalKey) {
        let Some(ws_idx) = self.state.active else {
            return;
        };
        let Some(pane_id) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.focused_pane_id())
        else {
            return;
        };
        let bytes = {
            let Some(rt) =
                self.state
                    .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)
            else {
                return;
            };
            rt.scroll_reset();
            rt.encode_terminal_key(key)
        };
        if bytes.is_empty() {
            return;
        }
        if let Some(sender) = self.lookup_runtime_sender(ws_idx, pane_id) {
            let _ = sender.try_send_bytes(bytes::Bytes::from(bytes));
        }
    }
}

impl AppState {
    /// Handle a key while in [`Mode::Home`]. Returns `true` when the key was a
    /// home command and was consumed; `false` means it should fall through (e.g.
    /// to the focused agent pane when focus is on Main).
    pub(crate) fn apply_home_key(&mut self, key: TerminalKey) -> bool {
        let event = key.as_key_event();
        let alt = event.modifiers.contains(KeyModifiers::ALT);
        // The Control/Agents panes are not text inputs, so command keys work as
        // plain letters there. `alt+` also works everywhere (e.g. from Main, where
        // plain keys are typed into the agent).
        let in_list = self.control.focus != FocusPane::Main;
        let cmd = alt || in_list;

        match event.code {
            // Directional pane focus — alt so it also works while typing in Main.
            KeyCode::Char('h') if alt => self.home_focus_left(),
            KeyCode::Char('j') if alt => self.set_home_focus(FocusPane::Agents),
            KeyCode::Char('k') if alt => self.set_home_focus(FocusPane::Control),
            KeyCode::Char('l') if alt => self.set_home_focus(FocusPane::Main),

            // Commands.
            KeyCode::Char('q') if cmd => self.should_quit = true,
            KeyCode::Char(',') if cmd => self.mode = Mode::Settings,
            KeyCode::Char('?') if cmd => self.mode = Mode::KeybindHelp,
            KeyCode::Char('n') if cmd => self.request_home_new_agent(),
            // `r` renames the selected agent in the Agents pane, but opens the
            // review branch-picker for the selected repo in the Control pane.
            KeyCode::Char('r') if cmd && self.control.focus == FocusPane::Agents => {
                self.request_home_rename_agent();
            }
            KeyCode::Char('r') if cmd => self.request_home_review(),
            KeyCode::Char('x') if cmd => self.request_home_kill_agent(),
            KeyCode::Char('p') if cmd => self.request_home_submit_pr(),
            KeyCode::Char(c) if cmd && c.is_ascii_digit() && c != '0' => {
                self.home_jump_to_agent((c as u8 - b'1') as usize);
            }

            // Selection/activation only in a list pane; with Main focused these
            // fall through to the agent pane.
            KeyCode::Up if in_list => self.home_move_selection(-1),
            KeyCode::Down if in_list => self.home_move_selection(1),
            KeyCode::Enter if in_list => self.home_activate(),

            // Vim-style navigation in the list panes: hjkl mirror the arrow keys.
            // The lists are vertical, so h/l are inert just like Left/Right.
            // (alt+h/j/k/l moved focus above, so these are the plain presses.)
            KeyCode::Char('k') if in_list => self.home_move_selection(-1),
            KeyCode::Char('j') if in_list => self.home_move_selection(1),
            KeyCode::Char('h') | KeyCode::Char('l') if in_list => {}

            _ => return false,
        }
        true
    }

    fn set_home_focus(&mut self, focus: FocusPane) {
        self.control.focus = focus;
        match focus {
            FocusPane::Control => self.control.last_left = LeftHalf::Control,
            FocusPane::Agents => self.control.last_left = LeftHalf::Agents,
            FocusPane::Main => {}
        }
    }

    pub(crate) fn home_focus_left(&mut self) {
        let target = match self.control.last_left {
            LeftHalf::Control => FocusPane::Control,
            LeftHalf::Agents => FocusPane::Agents,
        };
        self.control.focus = target;
    }

    fn home_agent_count(&self) -> usize {
        crate::ui::agent_panel_entries_all(self).len()
    }

    fn home_move_selection(&mut self, delta: isize) {
        match self.control.focus {
            FocusPane::Control => {
                let len = self.control.repos.len();
                self.control.selected_repo = step_index(self.control.selected_repo, delta, len);
            }
            FocusPane::Agents => {
                let len = self.home_agent_count();
                self.control.selected_agent = step_index(self.control.selected_agent, delta, len);
            }
            FocusPane::Main => {}
        }
    }

    /// Enter: activate the current selection without moving focus.
    fn home_activate(&mut self) {
        match self.control.focus {
            FocusPane::Agents => self.home_focus_selected_agent_workspace(),
            // Control activation (expand repo actions) and Main are no-ops for now.
            FocusPane::Control | FocusPane::Main => {}
        }
    }

    fn home_jump_to_agent(&mut self, index: usize) {
        let len = self.home_agent_count();
        if index >= len {
            return;
        }
        self.control.selected_agent = index;
        self.control.focus = FocusPane::Agents;
        self.control.last_left = LeftHalf::Agents;
        self.home_focus_selected_agent_workspace();
    }

    /// Make the selected agent's workspace active so its pane fills Main.
    fn home_focus_selected_agent_workspace(&mut self) {
        let entries = crate::ui::agent_panel_entries_all(self);
        if let Some(entry) = entries.get(self.control.selected_agent) {
            let ws_idx = entry.ws_idx;
            if self.active != Some(ws_idx) {
                self.active = Some(ws_idx);
                self.selected = ws_idx;
                self.mark_session_dirty();
            }
        }
    }

    // --- command stubs wired in later phases -------------------------------

    fn request_home_new_agent(&mut self) {
        // Open the create-agent form for the selected repository.
        if self.control.selected_repository().is_some() {
            self.name_input.clear();
            self.name_input_replace_on_type = false;
            self.mode = Mode::CreateAgent;
        }
    }

    fn request_home_review(&mut self) {
        // Open the branch picker for the selected repository.
        if let Some(repo) = self.control.selected_repository().cloned() {
            let branches = crate::workspace::list_review_branches(&repo.root);
            self.control.review = Some(ReviewState {
                repo,
                branches,
                selected: 0,
                scroll: 0,
            });
            self.mode = Mode::Review;
        }
    }

    /// Move the branch selection in the review picker.
    pub(crate) fn review_move_selection(&mut self, delta: isize) {
        if let Some(review) = self.control.review.as_mut() {
            review.selected = step_index(review.selected, delta, review.branches.len());
        }
    }

    fn request_home_kill_agent(&mut self) {
        // Confirm before killing the selected agent.
        if self.home_agent_count() > 0 {
            self.mode = Mode::ConfirmKill;
        }
    }

    /// Open the rename form for the selected agent, prefilled with its current
    /// name (selected for replace-on-first-keystroke).
    fn request_home_rename_agent(&mut self) {
        let entries = crate::ui::agent_panel_entries_all(self);
        let Some(ws_idx) = entries.get(self.control.selected_agent).map(|e| e.ws_idx) else {
            return;
        };
        let current = self
            .workspaces
            .get(ws_idx)
            .map(|ws| ws.display_name())
            .unwrap_or_default();
        self.name_input = current;
        self.name_input_replace_on_type = true;
        self.mode = Mode::RenameAgent;
    }

    /// Handle a key while confirming an agent kill.
    pub(crate) fn handle_confirm_kill_key(&mut self, key: crossterm::event::KeyEvent) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                self.kill_selected_agent();
                self.mode = Mode::Home;
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                self.mode = Mode::Home;
            }
            _ => {}
        }
    }

    /// Tear down the selected agent's workspace (kills its panes/PTYs) and clamp
    /// the home selection back into range.
    fn kill_selected_agent(&mut self) {
        let entries = crate::ui::agent_panel_entries_all(self);
        let Some(ws_idx) = entries.get(self.control.selected_agent).map(|e| e.ws_idx) else {
            return;
        };

        let workspace_terminal_ids = self.terminal_ids_for_workspace(ws_idx);
        self.workspaces.remove(ws_idx);
        self.remove_unattached_terminal_ids(workspace_terminal_ids);

        // Adjust active/selected for the removed (and shifted) workspaces.
        match self.active {
            Some(active) if active == ws_idx => self.active = None,
            Some(active) if active > ws_idx => self.active = Some(active - 1),
            _ => {}
        }
        if self.selected > ws_idx {
            self.selected -= 1;
        }
        if !self.workspaces.is_empty() && self.selected >= self.workspaces.len() {
            self.selected = self.workspaces.len() - 1;
        }

        let count = crate::ui::agent_panel_entries_all(self).len();
        if count == 0 {
            self.control.selected_agent = 0;
            self.control.focus = FocusPane::Control;
            self.control.last_left = LeftHalf::Control;
        } else if self.control.selected_agent >= count {
            self.control.selected_agent = count - 1;
        }
        self.mark_session_dirty();
    }

    fn request_home_submit_pr(&mut self) {
        // PR submission needs `gh`, so it runs at the App level: alt+p for the
        // focused agent is intercepted in `App::handle_home_key`, and alt+p in the
        // review picker is handled by `App::handle_review_key`. Nothing to do here.
    }
}

/// Move `index` by `delta`, clamped to `[0, len)`. Returns 0 when `len == 0`.
fn step_index(index: usize, delta: isize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let max = len - 1;
    let next = index as isize + delta;
    next.clamp(0, max as isize) as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::TerminalKey;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn alt(c: char) -> TerminalKey {
        TerminalKey::from(KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT))
    }

    fn plain(code: KeyCode) -> TerminalKey {
        TerminalKey::from(KeyEvent::new(code, KeyModifiers::empty()))
    }

    #[test]
    fn directional_focus_moves_between_panes() {
        let mut state = AppState::test_new();
        assert_eq!(state.control.focus, FocusPane::Control);

        assert!(state.apply_home_key(alt('j')));
        assert_eq!(state.control.focus, FocusPane::Agents);

        assert!(state.apply_home_key(alt('l')));
        assert_eq!(state.control.focus, FocusPane::Main);

        // alt+h returns to the last-focused left half (Agents).
        assert!(state.apply_home_key(alt('h')));
        assert_eq!(state.control.focus, FocusPane::Agents);

        assert!(state.apply_home_key(alt('k')));
        assert_eq!(state.control.focus, FocusPane::Control);
    }

    #[test]
    fn repo_selection_clamps_within_bounds() {
        let mut state = AppState::test_new();
        state.control.repos = vec![
            crate::workspace::Repository {
                key: "a".into(),
                root: "/a".into(),
                label: "a".into(),
            },
            crate::workspace::Repository {
                key: "b".into(),
                root: "/b".into(),
                label: "b".into(),
            },
        ];
        state.control.focus = FocusPane::Control;

        state.apply_home_key(plain(KeyCode::Up));
        assert_eq!(state.control.selected_repo, 0);
        state.apply_home_key(plain(KeyCode::Down));
        assert_eq!(state.control.selected_repo, 1);
        state.apply_home_key(plain(KeyCode::Down));
        assert_eq!(state.control.selected_repo, 1, "clamped at last repo");
    }

    #[test]
    fn alt_q_requests_quit_and_alt_comma_opens_settings() {
        let mut state = AppState::test_new();
        assert!(state.apply_home_key(alt(',')));
        assert_eq!(state.mode, Mode::Settings);

        let mut state = AppState::test_new();
        assert!(state.apply_home_key(alt('q')));
        assert!(state.should_quit);
    }

    #[test]
    fn alt_n_opens_create_agent_form_when_repo_selected() {
        let mut state = AppState::test_new();
        state.mode = Mode::Home;
        state.control.repos = vec![crate::workspace::Repository {
            key: "a".into(),
            root: "/a".into(),
            label: "a".into(),
        }];
        state.control.focus = FocusPane::Control;

        assert!(state.apply_home_key(alt('n')));
        assert_eq!(state.mode, Mode::CreateAgent);
    }

    #[test]
    fn alt_n_is_noop_without_repos() {
        let mut state = AppState::test_new();
        state.mode = Mode::Home;
        state.apply_home_key(alt('n'));
        assert_eq!(state.mode, Mode::Home);
    }

    #[test]
    fn kill_selected_agent_removes_its_workspace_with_confirm() {
        let mut state = AppState::test_new();
        state.mode = Mode::Home;
        state.workspaces = vec![
            crate::workspace::Workspace::test_new("a"),
            crate::workspace::Workspace::test_new("b"),
        ];
        state.ensure_test_terminals();
        // Only agent terminals appear in the agents half, so name them.
        let terminal_ids: Vec<_> = state
            .workspaces
            .iter()
            .map(|ws| {
                let pane = ws.tabs[0].root_pane;
                ws.tabs[0].panes[&pane].attached_terminal_id.clone()
            })
            .collect();
        for (i, terminal_id) in terminal_ids.iter().enumerate() {
            state
                .terminals
                .get_mut(terminal_id)
                .unwrap()
                .set_agent_name(format!("agent{i}"));
        }
        state.control.focus = FocusPane::Agents;
        state.control.selected_agent = 1;
        assert_eq!(crate::ui::agent_panel_entries_all(&state).len(), 2);

        // alt+x asks for confirmation.
        assert!(state.apply_home_key(alt('x')));
        assert_eq!(state.mode, Mode::ConfirmKill);

        // Cancelling leaves everything intact.
        state.handle_confirm_kill_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::empty()));
        assert_eq!(state.mode, Mode::Home);
        assert_eq!(state.workspaces.len(), 2);

        // Confirming kills the selected agent's workspace.
        state.apply_home_key(alt('x'));
        state.handle_confirm_kill_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::empty()));
        assert_eq!(state.mode, Mode::Home);
        assert_eq!(state.workspaces.len(), 1);
    }

    #[test]
    fn alt_r_opens_review_picker_and_arrows_clamp() {
        let mut state = AppState::test_new();
        state.mode = Mode::Home;
        state.control.repos = vec![crate::workspace::Repository {
            key: "a".into(),
            root: "/a".into(),
            label: "a".into(),
        }];
        state.control.focus = FocusPane::Control;

        assert!(state.apply_home_key(alt('r')));
        assert_eq!(state.mode, Mode::Review);
        assert!(state.control.review.is_some());

        // Seed branches and verify selection clamps within bounds.
        if let Some(review) = state.control.review.as_mut() {
            review.branches = vec![
                crate::workspace::Branch {
                    name: "main".into(),
                    is_current: true,
                    is_remote: false,
                    graph_prefix: String::new(),
                },
                crate::workspace::Branch {
                    name: "feat".into(),
                    is_current: false,
                    is_remote: false,
                    graph_prefix: String::new(),
                },
            ];
        }
        state.review_move_selection(1);
        assert_eq!(state.control.review.as_ref().unwrap().selected, 1);
        state.review_move_selection(1);
        assert_eq!(state.control.review.as_ref().unwrap().selected, 1);
        state.review_move_selection(-5);
        assert_eq!(state.control.review.as_ref().unwrap().selected, 0);
    }

    #[test]
    fn r_renames_in_agents_pane_but_reviews_in_control_pane() {
        let mut state = AppState::test_new();
        state.mode = Mode::Home;
        state.control.repos = vec![crate::workspace::Repository {
            key: "a".into(),
            root: "/a".into(),
            label: "a".into(),
        }];

        // In the Control pane, `r` opens the review picker.
        state.control.focus = FocusPane::Control;
        assert!(state.apply_home_key(alt('r')));
        assert_eq!(state.mode, Mode::Review);

        // Set up a single agent so the Agents pane has a selection to rename.
        state.mode = Mode::Home;
        state.control.review = None;
        state.workspaces = vec![crate::workspace::Workspace::test_new("a")];
        state.ensure_test_terminals();
        let terminal_id = {
            let pane = state.workspaces[0].tabs[0].root_pane;
            state.workspaces[0].tabs[0].panes[&pane]
                .attached_terminal_id
                .clone()
        };
        state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_agent_name("agent0".into());

        // In the Agents pane, `r` opens the rename form (prefilled).
        state.control.focus = FocusPane::Agents;
        assert!(state.apply_home_key(alt('r')));
        assert_eq!(state.mode, Mode::RenameAgent);
        assert!(state.name_input_replace_on_type);
    }

    #[test]
    fn plain_command_keys_fire_in_list_panes_but_not_main() {
        let mut state = AppState::test_new();
        state.mode = Mode::Home;
        state.control.repos = vec![crate::workspace::Repository {
            key: "a".into(),
            root: "/a".into(),
            label: "a".into(),
        }];

        // Plain `n` in the Control pane opens the create form.
        state.control.focus = FocusPane::Control;
        assert!(state.apply_home_key(plain(KeyCode::Char('n'))));
        assert_eq!(state.mode, Mode::CreateAgent);

        // Plain `r` in the Control pane opens the review picker.
        state.mode = Mode::Home;
        assert!(state.apply_home_key(plain(KeyCode::Char('r'))));
        assert_eq!(state.mode, Mode::Review);

        // With Main focused, the same plain key is left for the agent pane.
        state.mode = Mode::Home;
        state.control.focus = FocusPane::Main;
        assert!(!state.apply_home_key(plain(KeyCode::Char('n'))));
        assert_eq!(state.mode, Mode::Home);
    }

    #[test]
    fn hjkl_navigate_like_arrows_in_list_panes() {
        let mut state = AppState::test_new();
        state.mode = Mode::Home;
        state.control.repos = vec![
            crate::workspace::Repository {
                key: "a".into(),
                root: "/a".into(),
                label: "a".into(),
            },
            crate::workspace::Repository {
                key: "b".into(),
                root: "/b".into(),
                label: "b".into(),
            },
        ];
        state.control.focus = FocusPane::Control;

        // `j` moves down, `k` moves up — mirroring Down/Up arrows.
        assert!(state.apply_home_key(plain(KeyCode::Char('j'))));
        assert_eq!(state.control.selected_repo, 1);
        assert!(state.apply_home_key(plain(KeyCode::Char('k'))));
        assert_eq!(state.control.selected_repo, 0);

        // `h`/`l` are inert in the vertical lists, just like Left/Right.
        assert!(state.apply_home_key(plain(KeyCode::Char('h'))));
        assert!(state.apply_home_key(plain(KeyCode::Char('l'))));
        assert_eq!(state.control.selected_repo, 0);

        // With Main focused, plain hjkl fall through to the agent pane.
        state.control.focus = FocusPane::Main;
        assert!(!state.apply_home_key(plain(KeyCode::Char('j'))));
        assert!(!state.apply_home_key(plain(KeyCode::Char('k'))));
    }

    #[test]
    fn unhandled_key_falls_through() {
        let mut state = AppState::test_new();
        state.control.focus = FocusPane::Main;
        assert!(!state.apply_home_key(plain(KeyCode::Char('a'))));
    }
}
