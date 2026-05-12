//! Top Queries snapshot export.
//!
//! The Top Queries tab (`pg_stat_statements`) shows a snapshot that's
//! often worth handing off for further analysis — to a teammate, to a
//! ticket, or to an LLM. This module serialises the active connection's
//! snapshot to a self-contained JSON file with a timestamp in its name so
//! exports never overwrite each other.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::db::{Backend, TopQuery};

/// One row in the exported JSON. `share_of_total_time_pct` is computed
/// against the sum of `total_exec_time_ms` across the snapshot — the
/// 80/20 view that's usually the first question ("which query owns the
/// runtime?").
#[derive(Serialize)]
struct ExportedQuery<'a> {
    rank: usize,
    query: &'a str,
    calls: i64,
    total_exec_time_ms: f64,
    mean_exec_time_ms: f64,
    rows: i64,
    share_of_total_time_pct: f64,
}

#[derive(Serialize)]
struct Export<'a> {
    generated_at: DateTime<Utc>,
    profile: Option<&'a str>,
    queries: Vec<ExportedQuery<'a>>,
}

/// Render the snapshot to a JSON string. Returns the body and the
/// timestamp used in the suggested filename.
pub fn render_json(
    queries: &[TopQuery],
    profile: Option<&str>,
    now: DateTime<Utc>,
) -> serde_json::Result<String> {
    let total: f64 = queries.iter().map(|q| q.total_exec_time_ms).sum();
    let exported: Vec<ExportedQuery<'_>> = queries
        .iter()
        .enumerate()
        .map(|(i, q)| ExportedQuery {
            rank: i + 1,
            query: &q.query,
            calls: q.calls,
            total_exec_time_ms: q.total_exec_time_ms,
            mean_exec_time_ms: q.mean_exec_time_ms,
            rows: q.rows,
            share_of_total_time_pct: if total > 0.0 {
                100.0 * q.total_exec_time_ms / total
            } else {
                0.0
            },
        })
        .collect();
    let export = Export {
        generated_at: now,
        profile,
        queries: exported,
    };
    serde_json::to_string_pretty(&export)
}

/// Suggested output directory (`$XDG_DATA_HOME/pgtop/exports` on Linux,
/// `~/Library/Application Support/pgtop/exports` on macOS). Created on
/// demand.
pub fn export_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("pgtop")
        .join("exports")
}

/// Build a timestamped export path. `kind` distinguishes export shapes
/// (`"top-queries"` vs `"activity"`); the profile (if any) is included
/// for at-a-glance recognition.
fn timestamped_path(kind: &str, profile: Option<&str>, now: DateTime<Utc>) -> PathBuf {
    let stamp = now.format("%Y%m%d-%H%M%S");
    let name = match profile {
        Some(p) => format!("{kind}-{p}-{stamp}.json"),
        None => format!("{kind}-{stamp}.json"),
    };
    export_dir().join(name)
}

pub fn export_path(profile: Option<&str>, now: DateTime<Utc>) -> PathBuf {
    timestamped_path("top-queries", profile, now)
}

pub fn activity_export_path(profile: Option<&str>, now: DateTime<Utc>) -> PathBuf {
    timestamped_path("activity", profile, now)
}

/// Write the snapshot to disk, creating the export dir if needed.
/// Returns the path on success.
pub fn write(
    queries: &[TopQuery],
    profile: Option<&str>,
    now: DateTime<Utc>,
) -> std::io::Result<PathBuf> {
    let body = render_json(queries, profile, now)
        .map_err(|e| std::io::Error::other(format!("serialise top queries: {e}")))?;
    let dir = export_dir();
    std::fs::create_dir_all(&dir)?;
    let path = export_path(profile, now);
    std::fs::write(&path, body)?;
    Ok(path)
}

/// One backend in the Activity export. Mirrors the columns shown in
/// pg_stat_activity, plus two derived fields (`duration_secs`,
/// `idle_secs`) that fold the timestamps against `generated_at` so the
/// reader doesn't have to do the arithmetic.
#[derive(Serialize)]
struct ExportedBackend<'a> {
    pid: i32,
    datname: Option<&'a str>,
    usename: Option<&'a str>,
    application_name: Option<&'a str>,
    client_addr: Option<&'a str>,
    state: Option<&'a str>,
    wait_event_type: Option<&'a str>,
    wait_event: Option<&'a str>,
    backend_type: Option<&'a str>,
    /// `application_name == "pgtop"` — pgtop's own connections. Excluded
    /// from cancel/terminate actions in the TUI; flagged here so the
    /// reader can do the same.
    is_self: bool,
    /// `(generated_at - query_start).num_seconds()`. For active backends
    /// this is the current query duration; for idle ones, the duration
    /// of the most recent query before the connection went idle.
    duration_secs: Option<i64>,
    /// `(generated_at - state_change).num_seconds()` for any state that
    /// starts with `idle`. Long idle-in-transaction is usually the
    /// problem you're hunting.
    idle_secs: Option<i64>,
    backend_start: Option<DateTime<Utc>>,
    xact_start: Option<DateTime<Utc>>,
    query_start: Option<DateTime<Utc>>,
    state_change: Option<DateTime<Utc>>,
    backend_xid: Option<&'a str>,
    backend_xmin: Option<&'a str>,
    query: Option<&'a str>,
}

#[derive(Serialize)]
struct ActivityExport<'a> {
    generated_at: DateTime<Utc>,
    profile: Option<&'a str>,
    /// Active filter regex when the export was triggered (`None` if no
    /// filter is set). Helpful context when the exported set is a
    /// subset of all live backends.
    filter: Option<&'a str>,
    /// Number of backends in `backends` — i.e. what the user saw.
    exported_count: usize,
    /// Total number of backends in the snapshot before the filter was
    /// applied. Equal to `exported_count` when no filter is set.
    total_count: usize,
    backends: Vec<ExportedBackend<'a>>,
}

fn to_exported_backend<'a>(b: &'a Backend, now: DateTime<Utc>) -> ExportedBackend<'a> {
    let duration_secs = b.query_start.map(|t| (now - t).num_seconds());
    let is_idle = b.state.as_deref().is_some_and(|s| s.starts_with("idle"));
    let idle_secs = if is_idle {
        b.state_change.map(|t| (now - t).num_seconds())
    } else {
        None
    };
    ExportedBackend {
        pid: b.pid,
        datname: b.datname.as_deref(),
        usename: b.usename.as_deref(),
        application_name: b.application_name.as_deref(),
        client_addr: b.client_addr.as_deref(),
        state: b.state.as_deref(),
        wait_event_type: b.wait_event_type.as_deref(),
        wait_event: b.wait_event.as_deref(),
        backend_type: b.backend_type.as_deref(),
        is_self: b.is_self(),
        duration_secs,
        idle_secs,
        backend_start: b.backend_start,
        xact_start: b.xact_start,
        query_start: b.query_start,
        state_change: b.state_change,
        backend_xid: b.backend_xid.as_deref(),
        backend_xmin: b.backend_xmin.as_deref(),
        query: b.query.as_deref(),
    }
}

/// Render an Activity snapshot. `backends` should be the filtered slice
/// the user is looking at; `total_count` is the unfiltered backend count.
pub fn render_activity_json(
    backends: &[&Backend],
    total_count: usize,
    profile: Option<&str>,
    filter: Option<&str>,
    now: DateTime<Utc>,
) -> serde_json::Result<String> {
    let exported: Vec<ExportedBackend<'_>> = backends
        .iter()
        .map(|b| to_exported_backend(b, now))
        .collect();
    let export = ActivityExport {
        generated_at: now,
        profile,
        filter,
        exported_count: exported.len(),
        total_count,
        backends: exported,
    };
    serde_json::to_string_pretty(&export)
}

pub fn write_activity(
    backends: &[&Backend],
    total_count: usize,
    profile: Option<&str>,
    filter: Option<&str>,
    now: DateTime<Utc>,
) -> std::io::Result<PathBuf> {
    let body = render_activity_json(backends, total_count, profile, filter, now)
        .map_err(|e| std::io::Error::other(format!("serialise activity: {e}")))?;
    let dir = export_dir();
    std::fs::create_dir_all(&dir)?;
    let path = activity_export_path(profile, now);
    std::fs::write(&path, body)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn tq(query: &str, total_ms: f64, calls: i64) -> TopQuery {
        TopQuery {
            query: query.to_string(),
            calls,
            total_exec_time_ms: total_ms,
            mean_exec_time_ms: total_ms / calls as f64,
            rows: calls * 10,
        }
    }

    #[test]
    fn render_includes_share_of_total_time() {
        let now = Utc.with_ymd_and_hms(2026, 5, 12, 10, 0, 0).unwrap();
        let queries = vec![tq("SELECT 1", 600.0, 10), tq("SELECT 2", 400.0, 20)];
        let json = render_json(&queries, Some("prod"), now).unwrap();
        assert!(json.contains("\"share_of_total_time_pct\": 60.0"));
        assert!(json.contains("\"share_of_total_time_pct\": 40.0"));
        assert!(json.contains("\"profile\": \"prod\""));
        assert!(json.contains("\"rank\": 1"));
    }

    #[test]
    fn render_handles_zero_total() {
        let now = Utc.with_ymd_and_hms(2026, 5, 12, 10, 0, 0).unwrap();
        let queries = vec![tq("SELECT pg_sleep(0)", 0.0, 1)];
        let json = render_json(&queries, None, now).unwrap();
        assert!(json.contains("\"share_of_total_time_pct\": 0.0"));
        assert!(json.contains("\"profile\": null"));
    }

    #[test]
    fn export_path_includes_profile_and_timestamp() {
        let now = Utc.with_ymd_and_hms(2026, 5, 12, 10, 34, 5).unwrap();
        let p = export_path(Some("prod"), now);
        assert!(
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .contains("top-queries-prod-20260512-103405.json")
        );
    }

    #[test]
    fn export_path_omits_profile_when_none() {
        let now = Utc.with_ymd_and_hms(2026, 5, 12, 10, 34, 5).unwrap();
        let p = export_path(None, now);
        assert_eq!(
            p.file_name().unwrap().to_string_lossy(),
            "top-queries-20260512-103405.json"
        );
    }

    #[test]
    fn activity_path_uses_distinct_prefix() {
        let now = Utc.with_ymd_and_hms(2026, 5, 12, 10, 34, 5).unwrap();
        let p = activity_export_path(Some("prod"), now);
        assert_eq!(
            p.file_name().unwrap().to_string_lossy(),
            "activity-prod-20260512-103405.json"
        );
    }

    fn empty_backend(pid: i32) -> Backend {
        Backend {
            pid,
            datname: None,
            usename: None,
            application_name: None,
            client_addr: None,
            backend_start: None,
            xact_start: None,
            query_start: None,
            state_change: None,
            wait_event_type: None,
            wait_event: None,
            state: None,
            backend_xid: None,
            backend_xmin: None,
            query: None,
            backend_type: None,
        }
    }

    #[test]
    fn activity_export_derives_duration_and_idle() {
        let now = Utc.with_ymd_and_hms(2026, 5, 12, 10, 0, 0).unwrap();
        let mut active = empty_backend(101);
        active.state = Some("active".into());
        active.query_start = Some(now - chrono::Duration::seconds(7));
        active.state_change = Some(now - chrono::Duration::seconds(7));

        let mut idle = empty_backend(202);
        idle.state = Some("idle in transaction".into());
        idle.query_start = Some(now - chrono::Duration::seconds(30));
        idle.state_change = Some(now - chrono::Duration::seconds(120));

        let backends = vec![&active, &idle];
        let json = render_activity_json(&backends, 2, Some("prod"), None, now).unwrap();
        assert!(json.contains("\"pid\": 101"));
        assert!(json.contains("\"duration_secs\": 7"));
        // Active backend: idle_secs stays null even with state_change set.
        assert!(json.contains("\"idle_secs\": null"));
        // Idle-in-transaction: 120s since state_change.
        assert!(json.contains("\"idle_secs\": 120"));
        assert!(json.contains("\"exported_count\": 2"));
        assert!(json.contains("\"total_count\": 2"));
        assert!(json.contains("\"filter\": null"));
    }

    #[test]
    fn activity_export_reports_filter_and_partial_count() {
        let now = Utc.with_ymd_and_hms(2026, 5, 12, 10, 0, 0).unwrap();
        let b = empty_backend(1);
        let backends = vec![&b];
        let json = render_activity_json(&backends, 50, None, Some("write"), now).unwrap();
        assert!(json.contains("\"filter\": \"write\""));
        assert!(json.contains("\"exported_count\": 1"));
        assert!(json.contains("\"total_count\": 50"));
    }

    #[test]
    fn activity_export_marks_self_connection() {
        let now = Utc.with_ymd_and_hms(2026, 5, 12, 10, 0, 0).unwrap();
        let mut b = empty_backend(7);
        b.application_name = Some("pgtop".into());
        let backends = vec![&b];
        let json = render_activity_json(&backends, 1, None, None, now).unwrap();
        assert!(json.contains("\"is_self\": true"));
    }
}
