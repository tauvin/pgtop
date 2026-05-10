//! Persisted UI state: last tab, filter pattern, and sort settings,
//! restored on next startup. Best-effort — load/save errors are logged
//! and ignored, never block the user.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::app::{App, Sort, SortBy, SortDirection, Tab};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UiState {
    pub tab: Option<String>,
    pub filter: Option<String>,
    pub sort_by: Option<String>,
    pub sort_direction: Option<String>,
}

impl UiState {
    /// Snapshot the active connection's UI state into a serialisable struct.
    pub fn from_app(app: &App) -> Self {
        let conn = app.active();
        let filter = conn.filter.input.value();
        Self {
            tab: Some(app.current_tab.id().to_string()),
            filter: if filter.is_empty() {
                None
            } else {
                Some(filter.to_string())
            },
            sort_by: Some(conn.sort.by.label().to_string()),
            sort_direction: Some(match conn.sort.direction {
                SortDirection::Asc => "asc".to_string(),
                SortDirection::Desc => "desc".to_string(),
            }),
        }
    }

    /// Restore state into the app. Applies tab globally and filter+sort
    /// to every connection, so the user sees their last-used view across
    /// every connection at startup.
    pub fn apply(&self, app: &mut App) {
        if let Some(id) = &self.tab
            && let Some(tab) = Tab::from_id(id)
        {
            app.set_tab(tab);
        }
        let direction = match self.sort_direction.as_deref() {
            Some("desc") => Some(SortDirection::Desc),
            Some("asc") => Some(SortDirection::Asc),
            _ => None,
        };
        let by = self.sort_by.as_deref().and_then(SortBy::from_label);
        for conn in &mut app.connections {
            if let (Some(by), Some(direction)) = (by, direction) {
                conn.sort = Sort { by, direction };
            }
            if let Some(filter) = &self.filter {
                conn.filter.input = filter.clone().into();
                conn.filter.rebuild_regex();
            }
            let backends = std::mem::take(&mut conn.backends);
            conn.set_backends(backends);
        }
    }
}

pub fn state_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("pgtop")
        .join("state.toml")
}

pub fn load() -> Option<UiState> {
    let path = state_path();
    let s = std::fs::read_to_string(&path).ok()?;
    match toml::from_str::<UiState>(&s) {
        Ok(state) => Some(state),
        Err(e) => {
            tracing::warn!(?path, "failed to parse persisted state: {e}");
            None
        }
    }
}

pub fn save(state: &UiState) {
    let path = state_path();
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        tracing::warn!(?path, "failed to create state dir: {e}");
        return;
    }
    let s = match toml::to_string(state) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("failed to serialise state: {e}");
            return;
        }
    };
    if let Err(e) = std::fs::write(&path, s) {
        tracing::warn!(?path, "failed to write state: {e}");
    }
}
