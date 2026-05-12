//! Tab and sort-key enums. Stateless metadata: labels, indices, stable
//! string ids for persisted state. `strum` derives generate `iter()` (used
//! to map between `usize` index and variant) and `AsRef<str>` /
//! `EnumString` (used for the persisted-state id).

use strum::{EnumIter, EnumString, IntoEnumIterator, IntoStaticStr};

/// Active TUI tab.
//
// `serialize`-attributes drive both `IntoStaticStr` (used for `id()`) and
// `EnumString::from_str()` (used for `from_id()`). Display labels are not
// derived because `Top Queries` contains a space, which would awkwardly
// collide with the serialize attribute; `label()` stays a manual match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumIter, IntoStaticStr, EnumString)]
pub enum Tab {
    #[strum(serialize = "activity")]
    Activity,
    #[strum(serialize = "locks")]
    Locks,
    #[strum(serialize = "top_queries")]
    TopQueries,
    #[strum(serialize = "replication")]
    Replication,
    #[strum(serialize = "databases")]
    Databases,
    #[strum(serialize = "tables")]
    Tables,
    #[strum(serialize = "waits")]
    Waits,
}

impl Tab {
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
        Self::iter().position(|t| t == self).unwrap_or(0)
    }

    pub fn from_index(i: usize) -> Option<Tab> {
        Self::iter().nth(i)
    }

    /// Next tab in declaration order, wrapping around from the last back
    /// to the first. Panic-free: returns `self` if the iterator is empty
    /// (compile-time impossible — kept as a defensive fallback).
    pub fn cycle_next(self) -> Self {
        let mut it = Self::iter().cycle().skip_while(|&t| t != self);
        it.next();
        it.next().unwrap_or(self)
    }

    /// Stable string id used for persisted UI state.
    pub fn id(self) -> &'static str {
        self.into()
    }

    pub fn from_id(s: &str) -> Option<Self> {
        s.parse().ok()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumIter, IntoStaticStr, EnumString)]
pub enum SortBy {
    #[strum(serialize = "pid")]
    Pid,
    #[strum(serialize = "user")]
    User,
    #[strum(serialize = "state")]
    State,
    #[strum(serialize = "wait")]
    Wait,
    #[strum(serialize = "duration")]
    Duration,
    #[strum(serialize = "query")]
    Query,
}

impl SortBy {
    pub fn next(self) -> Self {
        let mut it = Self::iter().cycle().skip_while(|&v| v != self);
        it.next();
        it.next().expect("non-empty cycle")
    }

    pub fn label(self) -> &'static str {
        self.into()
    }

    pub fn from_label(s: &str) -> Option<Self> {
        s.parse().ok()
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

    #[test]
    fn tab_id_round_trips() {
        for t in Tab::iter() {
            assert_eq!(Tab::from_id(t.id()), Some(t));
        }
    }

    #[test]
    fn tab_index_round_trips() {
        for t in Tab::iter() {
            assert_eq!(Tab::from_index(t.index()), Some(t));
        }
    }

    #[test]
    fn sort_by_label_round_trips() {
        for s in SortBy::iter() {
            assert_eq!(SortBy::from_label(s.label()), Some(s));
        }
    }
}
