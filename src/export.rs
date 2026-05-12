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

use crate::db::TopQuery;

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

/// Build the path for a fresh export — timestamped to the second, with
/// the profile name (if any) for at-a-glance recognition.
pub fn export_path(profile: Option<&str>, now: DateTime<Utc>) -> PathBuf {
    let stamp = now.format("%Y%m%d-%H%M%S");
    let name = match profile {
        Some(p) => format!("top-queries-{p}-{stamp}.json"),
        None => format!("top-queries-{stamp}.json"),
    };
    export_dir().join(name)
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
}
