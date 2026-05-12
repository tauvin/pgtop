//! Hidden mode that sprinkles Linkin Park lyrics into the Activity and
//! Top Queries snapshots. Off by default; opt-in via the hidden CLI flag
//! `--with-the-bridge` or the `PGTOP_LP=1` environment variable.
//!
//! Marker fields on the injected rows make it possible to spot them on
//! close inspection: `usename = "linkin"`, `application_name = "linkin park"`,
//! `datname = "the_bridge"`, and pids in the 90000–99999 range (well above
//! what Postgres typically hands out).

use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::Utc;

use crate::db::{Backend, TopQuery};

static ENABLED: OnceLock<bool> = OnceLock::new();

/// How long a single lyric stays visible once it appears, in collector
/// ticks. For Activity (1s tick) this is 10 seconds; for Top Queries
/// (10s tick) it's about 100 seconds. Either way: long enough for the
/// careful reader to notice and pause.
const STICKY_TTL: u32 = 10;

/// Currently-displayed lyric and the number of ticks remaining. While
/// the entry is `Some`, every subsequent injection call returns the
/// same lyric; once `remaining` hits zero the slot is cleared and the
/// next call starts rolling for a fresh hit. Separate slots for the
/// two surfaces because their tickers run at different rates.
static STICKY_BACKEND: Mutex<Sticky> = Mutex::new(Sticky::new());
static STICKY_TOP: Mutex<Sticky> = Mutex::new(Sticky::new());

struct Sticky {
    inner: Option<(&'static str, u32)>,
}

impl Sticky {
    const fn new() -> Self {
        Self { inner: None }
    }

    /// One tick. If a lyric is still alive, return it and decrement.
    /// Otherwise call `roll` to maybe start a fresh run with `ttl`
    /// total ticks.
    fn next(
        &mut self,
        roll: impl FnOnce() -> Option<&'static str>,
        ttl: u32,
    ) -> Option<&'static str> {
        if let Some((v, remaining)) = self.inner {
            if remaining > 1 {
                self.inner = Some((v, remaining - 1));
                return Some(v);
            }
            // expired
            self.inner = None;
        }
        if let Some(v) = roll() {
            self.inner = Some((v, ttl));
            Some(v)
        } else {
            None
        }
    }
}

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

/// Maybe append a fake backend with lyrics. Once a lyric "lights up"
/// it stays for `STICKY_TTL` ticks so the reader has time to actually
/// see it. Roll rate is set so total visible share is roughly 20%
/// (10 visible ticks out of 50-tick average cycle).
pub fn maybe_inject_backend(backends: &mut Vec<Backend>) {
    if !enabled() {
        return;
    }
    let lyric = {
        let mut slot = STICKY_BACKEND.lock().expect("sticky lock poisoned");
        slot.next(
            || if one_in(40) { Some(pick_lyric()) } else { None },
            STICKY_TTL,
        )
    };
    let Some(lyric) = lyric else { return };
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

/// Maybe append a fake top query. Same sticky behaviour as
/// `maybe_inject_backend`, lower roll rate because Top Queries is
/// surveyed less often.
pub fn maybe_inject_top_query(queries: &mut Vec<TopQuery>) {
    if !enabled() {
        return;
    }
    let lyric = {
        let mut slot = STICKY_TOP.lock().expect("sticky lock poisoned");
        slot.next(
            || if one_in(50) { Some(pick_lyric()) } else { None },
            STICKY_TTL,
        )
    };
    let Some(lyric) = lyric else { return };
    // Plausible-looking numbers — not zero, not absurd. Computed once
    // per appearance is good enough; we don't need them to evolve over
    // the sticky window.
    let h = lyric_pid(lyric) as u32;
    let calls = 1000 + (h % 9000) as i64;
    let mean = 0.5 + (h % 200) as f64 / 100.0;
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

    #[test]
    fn sticky_holds_for_ttl_ticks_then_clears() {
        let mut s = Sticky::new();

        // First call: roll succeeds → start sticky window.
        let first = s.next(|| Some("L1"), 3);
        assert_eq!(first, Some("L1"));

        // Two more ticks return the same lyric without rolling.
        // The closure must not run; if it does we'd see "L2".
        let mut rolled = false;
        assert_eq!(
            s.next(
                || {
                    rolled = true;
                    Some("L2")
                },
                3
            ),
            Some("L1")
        );
        assert!(!rolled, "should not roll while sticky is alive");

        assert_eq!(s.next(|| Some("L2"), 3), Some("L1"));

        // Window expired. The next call rolls again. None → no lyric.
        assert_eq!(s.next(|| None, 3), None);

        // …and a subsequent successful roll starts a fresh window.
        assert_eq!(s.next(|| Some("L3"), 3), Some("L3"));
        assert_eq!(s.next(|| Some("L4"), 3), Some("L3"));
    }

    #[test]
    fn sticky_skips_when_roll_returns_none() {
        let mut s = Sticky::new();
        for _ in 0..5 {
            assert_eq!(s.next(|| None, 3), None);
        }
    }
}
