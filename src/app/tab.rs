//! Tab and sort-key enums. Stateless metadata: labels, indices, stable
//! string ids for persisted state.

/// Active TUI tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Activity,
    Locks,
    TopQueries,
    Replication,
    Databases,
    Tables,
    Waits,
}

impl Tab {
    pub const fn all() -> &'static [Tab] {
        &[
            Tab::Activity,
            Tab::Locks,
            Tab::TopQueries,
            Tab::Replication,
            Tab::Databases,
            Tab::Tables,
            Tab::Waits,
        ]
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Activity => "Activity",
            Self::Locks => "Locks",
            Self::TopQueries => "Top Queries",
            Self::Replication => "Replication",
            Self::Databases => "Databases",
            Self::Tables => "Tables",
            Self::Waits => "Waits",
        }
    }

    pub fn index(self) -> usize {
        match self {
            Self::Activity => 0,
            Self::Locks => 1,
            Self::TopQueries => 2,
            Self::Replication => 3,
            Self::Databases => 4,
            Self::Tables => 5,
            Self::Waits => 6,
        }
    }

    pub fn from_index(i: usize) -> Option<Tab> {
        Self::all().get(i).copied()
    }

    /// Stable string id used for persisted UI state — matches the label
    /// in lowercase + no spaces.
    pub fn id(self) -> &'static str {
        match self {
            Self::Activity => "activity",
            Self::Locks => "locks",
            Self::TopQueries => "top_queries",
            Self::Replication => "replication",
            Self::Databases => "databases",
            Self::Tables => "tables",
            Self::Waits => "waits",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        match s {
            "activity" => Some(Self::Activity),
            "locks" => Some(Self::Locks),
            "top_queries" => Some(Self::TopQueries),
            "replication" => Some(Self::Replication),
            "databases" => Some(Self::Databases),
            "tables" => Some(Self::Tables),
            "waits" => Some(Self::Waits),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortBy {
    Pid,
    User,
    State,
    Wait,
    Duration,
    Query,
}

impl SortBy {
    pub fn next(self) -> Self {
        match self {
            Self::Pid => Self::User,
            Self::User => Self::State,
            Self::State => Self::Wait,
            Self::Wait => Self::Duration,
            Self::Duration => Self::Query,
            Self::Query => Self::Pid,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Pid => "pid",
            Self::User => "user",
            Self::State => "state",
            Self::Wait => "wait",
            Self::Duration => "duration",
            Self::Query => "query",
        }
    }

    pub fn from_label(s: &str) -> Option<Self> {
        match s {
            "pid" => Some(Self::Pid),
            "user" => Some(Self::User),
            "state" => Some(Self::State),
            "wait" => Some(Self::Wait),
            "duration" => Some(Self::Duration),
            "query" => Some(Self::Query),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDirection {
    Asc,
    Desc,
}

impl SortDirection {
    pub fn flip(self) -> Self {
        match self {
            Self::Asc => Self::Desc,
            Self::Desc => Self::Asc,
        }
    }

    pub fn arrow(self) -> &'static str {
        match self {
            Self::Asc => "▲",
            Self::Desc => "▼",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Sort {
    pub by: SortBy,
    pub direction: SortDirection,
}

impl Default for Sort {
    fn default() -> Self {
        Self {
            by: SortBy::Pid,
            direction: SortDirection::Asc,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sort_by_cycles_through_all_columns() {
        let mut s = SortBy::Pid;
        let mut seen = vec![s];
        for _ in 0..6 {
            s = s.next();
            seen.push(s);
        }
        assert_eq!(
            seen,
            vec![
                SortBy::Pid,
                SortBy::User,
                SortBy::State,
                SortBy::Wait,
                SortBy::Duration,
                SortBy::Query,
                SortBy::Pid,
            ]
        );
    }

    #[test]
    fn sort_direction_flip_round_trips() {
        assert_eq!(SortDirection::Asc.flip(), SortDirection::Desc);
        assert_eq!(SortDirection::Asc.flip().flip(), SortDirection::Asc);
    }
}
