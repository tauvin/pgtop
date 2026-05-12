//! Application state shared across frames. Owns per-connection state plus
//! global UI state (mode, current tab, theme, in-flight EXPLAIN token).

mod connection;
mod tab;

pub use connection::{ConnectionState, ConnectionStatus, StatsHistory, WaitRow};
pub use tab::{Sort, SortBy, SortDirection, Tab};

use tokio_util::sync::CancellationToken;

use crate::actions::ActionResult;

/// Modal UI state. Global across the app — switching connections resets
/// `Mode` to `Normal` (see `App::set_active`).
#[derive(Debug, Clone)]
pub enum Mode {
    Normal,
    Detail(i32),
    Filter,
    ConfirmCancel(i32),
    ConfirmTerminate(i32, String),
    Explain(ExplainPopup),
    /// Jump-to-pid mode on the Activity tab. Holds the digits typed so far.
    JumpToPid(String),
}

/// State of the EXPLAIN popup: `Loading` while the query runs, `Ready`
/// with the plan text, or `Error` with the SQL error message.
#[derive(Debug, Clone)]
pub enum ExplainPopup {
    Loading { pid: i32 },
    Ready { pid: i32, plan: String },
    Error { pid: i32, message: String },
}

/// Root application state.
pub struct App {
    pub connections: Vec<ConnectionState>,
    /// Index of the active connection. Always valid — `set_active` clamps and
    /// the constructor requires a non-empty `Vec`.
    pub active: usize,

    pub mode: Mode,
    pub current_tab: Tab,
    pub theme: crate::theme::Theme,

    /// Cancellation token for the in-flight EXPLAIN task, if any. Owned by
    /// `App` so any mode transition (close_modal, set_active, mode change)
    /// can abort the task without leaving it running silently.
    pub explain_cancel: Option<CancellationToken>,
}

impl App {
    /// Requires a non-empty `connections`. Panics otherwise.
    pub fn new(connections: Vec<ConnectionState>) -> Self {
        assert!(
            !connections.is_empty(),
            "App requires at least one connection"
        );
        Self {
            connections,
            active: 0,
            mode: Mode::Normal,
            current_tab: Tab::Activity,
            theme: crate::theme::Theme::default(),
            explain_cancel: None,
        }
    }

    pub fn active(&self) -> &ConnectionState {
        &self.connections[self.active]
    }

    pub fn active_mut(&mut self) -> &mut ConnectionState {
        &mut self.connections[self.active]
    }

    pub fn connection_mut(&mut self, idx: usize) -> Option<&mut ConnectionState> {
        self.connections.get_mut(idx)
    }

    /// Set the active connection by index. Out-of-bounds is a no-op.
    /// Resets `Mode` to `Normal`, cancelling any in-flight EXPLAIN.
    pub fn set_active(&mut self, idx: usize) {
        if idx < self.connections.len() && idx != self.active {
            self.active = idx;
            self.cancel_explain();
            self.mode = Mode::Normal;
        }
    }

    /// Begin an EXPLAIN: cancel any prior in-flight task, store the new
    /// token, and switch to the loading popup.
    pub fn begin_explain(&mut self, pid: i32, cancel: CancellationToken) {
        if let Some(old) = self.explain_cancel.replace(cancel) {
            old.cancel();
        }
        self.mode = Mode::Explain(ExplainPopup::Loading { pid });
    }

    /// Replace the popup state when the EXPLAIN finishes (success or error)
    /// and drop the now-redundant cancel token.
    pub fn complete_explain(&mut self, popup: ExplainPopup) {
        self.explain_cancel = None;
        self.mode = Mode::Explain(popup);
    }

    fn cancel_explain(&mut self) {
        if let Some(c) = self.explain_cancel.take() {
            c.cancel();
        }
    }

    /// Close the active modal if its pid is no longer present in the active
    /// connection's backends.
    pub fn maybe_close_dead_modal(&mut self) {
        let active_pid = match &self.mode {
            Mode::Detail(pid) | Mode::ConfirmCancel(pid) => Some(*pid),
            Mode::ConfirmTerminate(pid, _) => Some(*pid),
            _ => None,
        };
        if let Some(pid) = active_pid
            && !self.active().backends.iter().any(|b| b.pid == pid)
        {
            self.mode = Mode::Normal;
        }
    }

    pub fn select_previous(&mut self) {
        let tab = self.current_tab;
        self.active_mut().select_previous(tab);
    }

    pub fn select_next(&mut self) {
        let tab = self.current_tab;
        self.active_mut().select_next(tab);
    }

    pub fn cycle_sort_column(&mut self) {
        self.active_mut().cycle_sort_column();
    }

    pub fn toggle_sort_direction(&mut self) {
        self.active_mut().toggle_sort_direction();
    }

    pub fn handle_filter_input(&mut self, key: crossterm::event::KeyEvent) {
        self.active_mut().handle_filter_input(key);
    }

    pub fn enter_filter_mode(&mut self) {
        self.mode = Mode::Filter;
    }

    pub fn exit_filter_mode(&mut self, commit: bool) {
        if !commit {
            self.active_mut().clear_filter();
        }
        self.mode = Mode::Normal;
    }

    pub fn on_enter(&mut self) {
        let conn = self.active();
        if let Some(idx) = conn.table_state.selected()
            && let Some(b) = conn.visible_backend(idx)
        {
            self.mode = Mode::Detail(b.pid);
        }
    }

    /// Selected backend's `(pid, query)` if Activity has a row selected and
    /// the backend has a non-empty query. Used to drive the EXPLAIN popup.
    pub fn selected_query(&self) -> Option<(i32, String)> {
        let conn = self.active();
        let idx = conn.table_state.selected()?;
        let b = conn.visible_backend(idx)?;
        let q = b.query.as_ref()?;
        if q.trim().is_empty() {
            return None;
        }
        Some((b.pid, q.clone()))
    }

    pub fn close_modal(&mut self) {
        self.cancel_explain();
        self.mode = Mode::Normal;
    }

    /// Enter jump-to-pid mode (Activity tab only). Initialises an empty
    /// digit buffer; user types digits, Enter jumps, Esc cancels.
    pub fn enter_jump_mode(&mut self) {
        if self.current_tab == Tab::Activity {
            self.mode = Mode::JumpToPid(String::new());
        }
    }

    /// Append a digit to the jump-to-pid input. No-op outside that mode or
    /// for non-digit characters.
    pub fn jump_input_push(&mut self, c: char) {
        if let Mode::JumpToPid(ref mut s) = self.mode
            && c.is_ascii_digit()
            && s.len() < 10
        {
            s.push(c);
        }
    }

    pub fn jump_input_pop(&mut self) {
        if let Mode::JumpToPid(ref mut s) = self.mode {
            s.pop();
        }
    }

    /// Try to jump the Activity selection to the typed pid. Returns
    /// `Ok(())` if the pid exists in the filtered list (selection updated,
    /// mode reset to Normal); `Err(_)` if the input parses but the pid is
    /// not visible.
    pub fn try_jump_to_pid(&mut self) -> Result<(), &'static str> {
        let Mode::JumpToPid(ref s) = self.mode else {
            return Err("not in jump mode");
        };
        let Ok(pid) = s.parse::<i32>() else {
            return Err("invalid pid");
        };
        let conn = self.active_mut();
        let idx = conn
            .filtered
            .iter()
            .position(|&i| conn.backends.get(i).is_some_and(|b| b.pid == pid));
        match idx {
            Some(i) => {
                conn.table_state.select(Some(i));
                self.mode = Mode::Normal;
                Ok(())
            }
            None => Err("pid not in current filter"),
        }
    }

    pub fn try_open_confirm_cancel(&mut self) -> bool {
        if self.current_tab != Tab::Activity {
            return false;
        }
        let conn = self.active();
        if !conn.actions_allowed {
            return false;
        }
        let Some(idx) = conn.table_state.selected() else {
            return false;
        };
        let Some(b) = conn.visible_backend(idx) else {
            return false;
        };
        if b.is_self() {
            return false;
        }
        self.mode = Mode::ConfirmCancel(b.pid);
        true
    }

    pub fn try_open_confirm_terminate(&mut self) -> bool {
        if self.current_tab != Tab::Activity {
            return false;
        }
        let conn = self.active();
        if !conn.actions_allowed {
            return false;
        }
        let Some(idx) = conn.table_state.selected() else {
            return false;
        };
        let Some(b) = conn.visible_backend(idx) else {
            return false;
        };
        if b.is_self() {
            return false;
        }
        self.mode = Mode::ConfirmTerminate(b.pid, String::new());
        true
    }

    pub fn terminate_input_push(&mut self, c: char) {
        if let Mode::ConfirmTerminate(_, text) = &mut self.mode {
            text.push(c);
        }
    }

    pub fn terminate_input_backspace(&mut self) {
        if let Mode::ConfirmTerminate(_, text) = &mut self.mode {
            text.pop();
        }
    }

    pub fn try_confirm_terminate(&mut self) -> Option<i32> {
        if let Mode::ConfirmTerminate(pid, text) = &self.mode
            && text == "yes"
        {
            let pid = *pid;
            self.close_modal();
            return Some(pid);
        }
        None
    }

    /// Stash an action result on the connection that produced it. The user
    /// sees it whenever they're (or switch back) on that connection.
    pub fn set_action_result(&mut self, conn_idx: usize, result: ActionResult) {
        if let Some(conn) = self.connections.get_mut(conn_idx) {
            conn.last_action_result = Some(result);
        }
    }

    pub fn set_tab(&mut self, tab: Tab) {
        self.current_tab = tab;
    }

    pub fn next_tab(&mut self) {
        self.current_tab = self.current_tab.cycle_next();
    }
}
