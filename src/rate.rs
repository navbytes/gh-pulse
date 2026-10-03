//! What we know about GitHub's rate limits, learned for free: `rateLimit { .. }` rides along in our own
//! GraphQL queries and `gh api rate_limit` answers without costing quota. Everything that decides
//! "slow down" (the header chip, pausing background refreshes, backing off after a secondary limit)
//! reads this one shared state; the pure methods take `now` so they test without a clock.
use serde_json::Value;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering::Relaxed};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bucket {
    pub limit: u32,
    pub remaining: u32,
    /// Epoch seconds when the window resets.
    pub reset: u64,
}

impl Bucket {
    /// (remaining, limit) as of `now`: a window that has reset is full again.
    fn at(&self, now: u64) -> (u32, u32) {
        if now >= self.reset {
            (self.limit, self.limit)
        } else {
            (self.remaining, self.limit)
        }
    }

    fn pct(&self, now: u64) -> u32 {
        let (r, l) = self.at(now);
        if l == 0 {
            100
        } else {
            (u64::from(r) * 100 / u64::from(l)) as u32
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RateState {
    pub graphql: Option<Bucket>,
    pub core: Option<Bucket>,
    pub search: Option<Bucket>,
    /// Background work stays quiet until this epoch second (secondary limit / Retry-After).
    pub backoff_until: u64,
}

/// What the header chip shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chip {
    pub remaining: u32,
    pub limit: u32,
    pub reset: u64,
}

impl RateState {
    fn tight(&self, now: u64) -> Option<(&'static str, Bucket)> {
        [("graphql", self.graphql), ("core", self.core)]
            .into_iter()
            .filter_map(|(n, b)| Some((n, b?)))
            .min_by_key(|(_, b)| b.pct(now))
    }

    /// The bucket to show: graphql or core below `low` percent, or search below 5 left.
    pub fn chip(&self, now: u64, low: u8) -> Option<Chip> {
        let mk = |b: Bucket| {
            let (remaining, limit) = b.at(now);
            Chip {
                remaining,
                limit,
                reset: b.reset,
            }
        };
        if let Some((_, b)) = self.tight(now).filter(|(_, b)| b.pct(now) < u32::from(low)) {
            return Some(mk(b));
        }
        self.search.filter(|b| b.at(now).0 < 5).map(mk)
    }

    /// graphql or core below `low` percent (the search bucket alone doesn't count).
    pub fn low(&self, now: u64, low: u8) -> bool {
        self.tight(now)
            .is_some_and(|(_, b)| b.pct(now) < u32::from(low))
    }

    /// Background work should wait: graphql or core below `pause` percent, or backing off.
    pub fn paused(&self, now: u64, pause: u8) -> bool {
        self.backoff_until > now
            || self
                .tight(now)
                .is_some_and(|(_, b)| b.pct(now) < u32::from(pause))
    }

    /// Epoch second the pause ends: the backoff, or the window reset of the tight bucket.
    pub fn resumes_at(&self, now: u64, pause: u8) -> Option<u64> {
        let quota = self
            .tight(now)
            .filter(|(_, b)| b.pct(now) < u32::from(pause))
            .map(|(_, b)| b.reset);
        match (quota, self.backoff_until > now) {
            (Some(r), true) => Some(r.max(self.backoff_until)),
            (Some(r), false) => Some(r),
            (None, true) => Some(self.backoff_until),
            _ => None,
        }
    }
}

static STATE: Mutex<RateState> = Mutex::new(RateState {
    graphql: None,
    core: None,
    search: None,
    backoff_until: 0,
});
// the config's thresholds (percent); set once at startup
static LOW: AtomicU8 = AtomicU8::new(20);
static PAUSE: AtomicU8 = AtomicU8::new(10);

pub fn set_limits(low: u8, pause: u8) {
    LOW.store(low, Relaxed);
    PAUSE.store(pause, Relaxed);
}

pub fn thresholds() -> (u8, u8) {
    (LOW.load(Relaxed), PAUSE.load(Relaxed))
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

pub fn snapshot() -> RateState {
    STATE.lock().map(|s| *s).unwrap_or_default()
}

fn update(f: impl FnOnce(&mut RateState)) {
    if let Ok(mut s) = STATE.lock() {
        f(&mut s);
    }
}

/// For the worker pool: should plain background jobs stand down right now?
pub fn paused_now() -> bool {
    snapshot().paused(now(), PAUSE.load(Relaxed))
}

/// Automatic work (comment pages) should hold off: the quota is low or we are backing off.
pub fn quiet_now() -> bool {
    let (low, pause) = thresholds();
    let (s, now) = (snapshot(), now());
    s.low(now, low) || s.paused(now, pause)
}

#[cfg(test)]
pub fn reset_for_test() {
    update(|s| *s = RateState::default());
}

/// Epoch seconds from an ISO timestamp like 2026-10-03T12:00:00Z; None when it isn't one.
fn iso_epoch(s: &str) -> Option<u64> {
    crate::gh::epoch(s).and_then(|e| u64::try_from(e).ok())
}

/// `data.rateLimit` of one of our GraphQL responses: remember it and log the query's cost.
pub fn note_graphql_response(body: &str) {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return;
    };
    let r = &v["data"]["rateLimit"];
    let (Some(cost), Some(remaining), Some(limit)) = (
        r["cost"].as_u64(),
        r["remaining"].as_u64(),
        r["limit"].as_u64(),
    ) else {
        return;
    };
    // without a reset time the numbers can't be trusted (it would read as "already reset"): skip them
    let (Some(reset), Ok(limit), Ok(remaining)) = (
        iso_epoch(r["resetAt"].as_str().unwrap_or("")),
        u32::try_from(limit),
        u32::try_from(remaining),
    ) else {
        return;
    };
    update(|s| {
        s.graphql = Some(Bucket {
            limit,
            remaining,
            reset,
        })
    });
    crate::gh::log_note(&format!(
        "rate: cost {cost}, graphql {remaining}/{limit} left"
    ));
}

/// The `gh api rate_limit` document (it does not count against the quota).
pub fn parse_rate_limit(body: &str) -> Option<RateState> {
    let v: Value = serde_json::from_str(body).ok()?;
    let b = |k: &str| {
        let r = &v["resources"][k];
        Some(Bucket {
            limit: u32::try_from(r["limit"].as_u64()?).ok()?,
            remaining: u32::try_from(r["remaining"].as_u64()?).ok()?,
            reset: r["reset"].as_u64()?,
        })
    };
    let st = RateState {
        graphql: b("graphql"),
        core: b("core"),
        search: b("search"),
        backoff_until: 0,
    };
    (st.graphql.is_some() || st.core.is_some()).then_some(st)
}

pub fn apply_poll(body: &str) -> bool {
    let Some(p) = parse_rate_limit(body) else {
        return false;
    };
    update(|s| {
        (s.graphql, s.core, s.search) = (p.graphql, p.core, p.search);
    });
    if let Some(g) = p.graphql {
        crate::gh::log_note(&format!(
            "rate: graphql {}/{} left, core {}/{}",
            g.remaining,
            g.limit,
            p.core.map_or(0, |c| c.remaining),
            p.core.map_or(0, |c| c.limit)
        ));
    }
    true
}

/// An error that may be GitHub throttling us: back background work off for as long as it says
/// (default a minute). No-op for every other error.
pub fn note_error(msg: &str) {
    if let Some(secs) = crate::gh::rate_limit_secs(msg) {
        let until = now().saturating_add(secs.min(crate::gh::MAX_BACKOFF_SECS));
        update(|s| s.backoff_until = s.backoff_until.max(until));
    }
}

/// `HH:MM` of an epoch second in local time (UTC if the system `date` can't say).
pub fn clock(epoch: u64) -> String {
    let fmt = |args: &[&str]| {
        std::process::Command::new("date")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| s.len() == 5)
    };
    let e = epoch.to_string();
    fmt(&["-r", &e, "+%H:%M"])
        .or_else(|| fmt(&["-d", &format!("@{e}"), "+%H:%M"]))
        .unwrap_or_else(|| format!("{:02}:{:02} UTC", epoch / 3600 % 24, epoch / 60 % 60))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(graphql: (u32, u32, u64)) -> RateState {
        RateState {
            graphql: Some(Bucket {
                limit: graphql.1,
                remaining: graphql.0,
                reset: graphql.2,
            }),
            core: Some(Bucket {
                limit: 5000,
                remaining: 5000,
                reset: 9999,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn chip_and_pause_follow_the_thresholds_and_recover_at_reset() {
        let now = 1000;
        assert_eq!(st((4000, 5000, 2000)).chip(now, 20), None, "plenty left");
        let low = st((412, 5000, 2000));
        assert_eq!(
            low.chip(now, 20),
            Some(Chip {
                remaining: 412,
                limit: 5000,
                reset: 2000
            })
        );
        assert!(!low.paused(now, 5), "8% is above a 5% pause line");
        assert!(low.paused(now, 10), "8% is under a 10% pause line");
        assert_eq!(low.resumes_at(now, 10), Some(2000));
        // after the window resets the bucket counts as full again
        assert_eq!(low.chip(2000, 20), None);
        assert!(!low.paused(2000, 10));
        assert_eq!(low.resumes_at(2000, 10), None);
        // the search bucket only matters when nearly empty
        let mut s = st((5000, 5000, 2000));
        s.search = Some(Bucket {
            limit: 30,
            remaining: 4,
            reset: 1500,
        });
        assert_eq!(s.chip(now, 20).map(|c| c.limit), Some(30));
        s.search = Some(Bucket {
            limit: 30,
            remaining: 12,
            reset: 1500,
        });
        assert_eq!(s.chip(now, 20), None);
        assert!(!s.paused(now, 10), "search never pauses");
    }

    #[test]
    fn the_tightest_of_graphql_and_core_decides() {
        let mut s = st((4000, 5000, 2000));
        s.core = Some(Bucket {
            limit: 5000,
            remaining: 100,
            reset: 1800,
        });
        assert_eq!(s.chip(1000, 20).map(|c| c.remaining), Some(100));
        assert_eq!(s.resumes_at(1000, 10), Some(1800));
    }

    #[test]
    fn a_secondary_limit_backs_background_work_off_for_the_time_given() {
        let s = RateState {
            backoff_until: 1030,
            ..Default::default()
        };
        assert!(s.paused(1000, 10));
        assert_eq!(s.resumes_at(1000, 10), Some(1030));
        assert!(
            !s.paused(1030, 10),
            "no loop: it resumes once the time has passed"
        );
    }

    #[test]
    fn parses_the_free_rate_limit_document_and_graphql_footers() {
        let doc = r#"{"resources":{"core":{"limit":5000,"used":10,"remaining":4990,"reset":1900},
            "graphql":{"limit":5000,"used":4588,"remaining":412,"reset":1800},
            "search":{"limit":30,"used":0,"remaining":30,"reset":1100}}}"#;
        let p = parse_rate_limit(doc).unwrap();
        assert_eq!(p.graphql.unwrap().remaining, 412);
        assert_eq!(p.search.unwrap().limit, 30);
        assert!(parse_rate_limit("{}").is_none() && parse_rate_limit("nope").is_none());
    }

    #[test]
    fn clock_formats_hh_mm() {
        let c = clock(1_700_000_000);
        assert!(c.len() == 5 || c.ends_with("UTC"), "{c}");
    }

    #[test]
    fn hostile_numbers_never_overflow_or_read_as_a_reset() {
        let _g = crate::testshim::Shim::new(); // owns the shared state
        let body = |extra: &str| {
            format!(r#"{{"data":{{"rateLimit":{{"cost":1,"remaining":5,"limit":5000,{extra}}}}}}}"#)
        };
        // an unparsable or missing resetAt: the bucket is not touched (no pretend-full quota)
        for bad in [
            r#""resetAt":"garbage""#,
            r#""resetAt":null"#,
            r#""resetAt":"1969-01-01T00:00:00Z""#,
            r#""x":1"#,
        ] {
            note_graphql_response(&body(bad));
            assert_eq!(snapshot().graphql, None, "{bad}");
        }
        // numbers beyond u32 are refused too
        note_graphql_response(
            r#"{"data":{"rateLimit":{"cost":1,"remaining":99999999999,"limit":5000,"resetAt":"2030-01-01T00:00:00Z"}}}"#,
        );
        assert_eq!(snapshot().graphql, None);
        note_graphql_response(&body(r#""resetAt":"2030-01-01T00:00:00Z""#));
        assert_eq!(snapshot().graphql.unwrap().remaining, 5);
        // Retry-After is capped, and adding it to the clock saturates
        note_error("secondary rate limit. Retry-After: 99999999999");
        assert!(
            snapshot().backoff_until <= now() + crate::gh::MAX_BACKOFF_SECS,
            "capped at an hour"
        );
        note_error("rate limit. Retry-After: 18446744073709551615");
        assert!(snapshot().backoff_until <= now() + crate::gh::MAX_BACKOFF_SECS);
        assert_eq!(
            crate::gh::rate_limit_secs("rate limit Retry-After: 7200"),
            Some(3600)
        );
        assert_eq!(
            crate::gh::rate_limit_secs("rate limit Retry-After: 30"),
            Some(30)
        );
        // the free document: absurd values are rejected as a whole, not truncated
        assert!(
            parse_rate_limit(
                r#"{"resources":{"graphql":{"limit":5000,"remaining":99999999999,"reset":1}}}"#
            )
            .is_none()
        );
        let s = parse_rate_limit(
            r#"{"resources":{"core":{"limit":0,"remaining":0,"reset":18446744073709551615}}}"#,
        )
        .unwrap();
        assert!(
            !s.paused(1000, 10) && s.chip(1000, 20).is_none(),
            "a zero limit means nothing"
        );
        assert!(
            clock(u64::MAX).len() >= 5,
            "formatting a silly time does not panic"
        );
    }
}
