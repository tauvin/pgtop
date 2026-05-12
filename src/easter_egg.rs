//! Hidden mode that sprinkles Linkin Park lyrics into the Activity and
//! Top Queries snapshots. Off by default; opt-in via the hidden CLI flag
//! `--with-the-bridge` or the `PGTOP_LP=1` environment variable.
//!
//! Marker fields on the injected rows make it possible to spot them on
//! close inspection: `usename = "linkin"`, `application_name = "linkin park"`,
//! `datname = "the_bridge"`, and pids in the 90000–99999 range (well above
//! what Postgres typically hands out).

use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::Utc;

use crate::db::{Backend, TopQuery};

static ENABLED: OnceLock<bool> = OnceLock::new();

/// Set once at startup. Subsequent calls are silently ignored.
pub fn init(enabled: bool) {
    let _ = ENABLED.set(enabled);
}

fn enabled() -> bool {
    *ENABLED.get().unwrap_or(&false)
}

const LYRICS: &[&str] = &[
    "In the end, it doesn't even matter",
    "I tried so hard and got so far",
    "Crawling in my skin, these wounds they will not heal",
    "I've become so numb, I can't feel you there",
    "I am a little bit of loneliness, a little bit of disregard",
    "I want to heal, I want to feel, what I thought was never real",
    "I'm breaking the habit, I'm breaking the habit tonight",
    "In this farewell there's no blood, there's no alibi",
    "Give me reason to prove me wrong, to wash this memory clean",
    "Do you feel cold and lost in desperation",
    "I'll never be what you want and I'll never come back home",
    "Take me down to the river bend, take me down to the fighting end",
    "We built it up to watch it burn down",
    "I bleed it out, digging deeper just to throw it away",
    "Shut up when I'm talking to you",
    "It's like I'm paranoid looking over my back",
    "Numb, so numb, becoming this all I want to do",
    "When my time comes, forget the wrong that I've done",
];

/// Deterministic but spread-out pid in 90000..=99999 based on the lyric's
/// hash, so the same lyric always gets the same pid (cosmetic continuity
/// across ticks).
fn lyric_pid(lyric: &str) -> i32 {
    let h: u32 = lyric
        .bytes()
        .fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
    90000 + (h % 10000) as i32
}

/// Cheap source of "randomness" — we don't need cryptographic strength,
/// just something that varies between ticks. Avoids pulling rand in.
fn pseudo_random_u32() -> u32 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    nanos.wrapping_mul(2654435761)
}

fn pick_lyric() -> &'static str {
    let idx = (pseudo_random_u32() as usize) % LYRICS.len();
    LYRICS[idx]
}

/// One-in-`n` coin flip.
fn one_in(n: u32) -> bool {
    pseudo_random_u32().is_multiple_of(n)
}

/// Maybe append a fake backend with lyrics. No-op when the egg is off.
/// Default firing rate: ~5% per call.
pub fn maybe_inject_backend(backends: &mut Vec<Backend>) {
    if !enabled() || !one_in(20) {
        return;
    }
    let lyric = pick_lyric();
    let now = Utc::now();
    backends.push(Backend {
        pid: lyric_pid(lyric),
        datname: Some("the_bridge".to_string()),
        usename: Some("linkin".to_string()),
        application_name: Some("linkin park".to_string()),
        client_addr: None,
        backend_start: Some(now - chrono::Duration::minutes(3)),
        xact_start: None,
        query_start: Some(now - chrono::Duration::seconds(7)),
        state_change: Some(now - chrono::Duration::seconds(7)),
        wait_event_type: None,
        wait_event: None,
        state: Some("active".to_string()),
        backend_xid: None,
        backend_xmin: None,
        query: Some(lyric.to_string()),
        backend_type: Some("client backend".to_string()),
    });
}

/// Maybe append a fake top query. Lower rate (~1 in 30) because Top
/// Queries is read more carefully than the live Activity churn.
pub fn maybe_inject_top_query(queries: &mut Vec<TopQuery>) {
    if !enabled() || !one_in(30) {
        return;
    }
    let lyric = pick_lyric();
    // Plausible-looking numbers — not zero, not absurd.
    let calls = 1000 + (pseudo_random_u32() % 9000) as i64;
    let mean = 0.5 + (pseudo_random_u32() % 200) as f64 / 100.0;
    let total = mean * calls as f64;
    queries.push(TopQuery {
        query: lyric.to_string(),
        calls,
        total_exec_time_ms: total,
        mean_exec_time_ms: mean,
        rows: calls,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_op_when_disabled() {
        // Note: ENABLED is process-global and tests share it. We rely
        // on the default-false state set by other tests that don't
        // call init(true).
        let mut backends: Vec<Backend> = vec![];
        for _ in 0..50 {
            maybe_inject_backend(&mut backends);
        }
        assert!(backends.is_empty(), "must not inject while disabled");
    }

    #[test]
    fn lyric_pid_is_stable() {
        let a = lyric_pid("In the end, it doesn't even matter");
        let b = lyric_pid("In the end, it doesn't even matter");
        assert_eq!(a, b);
        assert!((90000..100000).contains(&a));
    }
}
