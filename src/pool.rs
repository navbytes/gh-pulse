//! The one place `gh` work runs: a few worker threads draining two queues, user-initiated work first.
//! This caps how many `gh` processes are in flight, lets stale queued jobs be dropped before they
//! start, and lets background jobs stand down while the API quota is low.
use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Prio {
    /// Something the user asked for: the selected item's detail, a list, an action, a form.
    User,
    /// Counts, automatic comment pages, the badge poll: waits for user work and for quota.
    Background,
    /// Background, but allowed while paused (the quota check that ends the pause).
    Probe,
}

struct Job {
    prio: Prio,
    /// True when the job's result is no longer wanted.
    stale: Box<dyn Fn() -> bool + Send>,
    run: Box<dyn FnOnce() + Send>,
    /// Called instead of `run` when the job is dropped (stale, paused, queue full) or after `run` panicked.
    dropped: Box<dyn FnOnce() + Send>,
}

#[derive(Default)]
struct Queues {
    user: VecDeque<Job>,
    bg: VecDeque<Job>,
}

pub struct Pool {
    q: Arc<(Mutex<Queues>, Condvar)>,
    /// Jobs taken off the queue and not finished (for `idle`).
    #[cfg(test)]
    busy: Arc<std::sync::atomic::AtomicUsize>,
    /// Background queue bound; user work is bounded too, far higher.
    bg_cap: usize,
    user_cap: usize,
}

impl Pool {
    /// `paused` is asked before a background job starts.
    pub fn new(workers: usize, paused: fn() -> bool) -> Pool {
        let q: Arc<(Mutex<Queues>, Condvar)> = Arc::default();
        let busy: Arc<std::sync::atomic::AtomicUsize> = Arc::default();
        for _ in 0..workers.clamp(1, 8) {
            let (q, busy) = (q.clone(), busy.clone());
            std::thread::spawn(move || worker(&q, &busy, paused));
        }
        Pool {
            q,
            #[cfg(test)]
            busy,
            bg_cap: 64,
            user_cap: 512,
        }
    }

    pub fn submit(
        &self,
        prio: Prio,
        stale: impl Fn() -> bool + Send + 'static,
        run: impl FnOnce() + Send + 'static,
        dropped: impl FnOnce() + Send + 'static,
    ) {
        let job = Job {
            prio,
            stale: Box::new(stale),
            run: Box::new(run),
            dropped: Box::new(dropped),
        };
        let (m, cv) = &*self.q;
        let rejected = {
            let mut g = m.lock().unwrap_or_else(|e| e.into_inner());
            let (queue, cap) = match prio {
                Prio::User => (&mut g.user, self.user_cap),
                _ => (&mut g.bg, self.bg_cap),
            };
            if queue.len() >= cap {
                Some(job)
            } else {
                queue.push_back(job);
                None
            }
        };
        match rejected {
            Some(j) => (j.dropped)(),
            None => cv.notify_one(),
        }
    }

    /// Nothing queued and nothing running (tests wait for this between cases that share the pool).
    #[cfg(test)]
    pub fn idle(&self) -> bool {
        let g = self.q.0.lock().unwrap_or_else(|e| e.into_inner());
        g.user.is_empty()
            && g.bg.is_empty()
            && self.busy.load(std::sync::atomic::Ordering::SeqCst) == 0
    }

    #[cfg(test)]
    fn queued(&self) -> (usize, usize) {
        let g = self.q.0.lock().unwrap();
        (g.user.len(), g.bg.len())
    }
}

fn worker(
    q: &(Mutex<Queues>, Condvar),
    busy: &std::sync::atomic::AtomicUsize,
    paused: fn() -> bool,
) {
    use std::sync::atomic::Ordering::SeqCst;
    loop {
        let job = {
            let mut g = q.0.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if let Some(j) = g.user.pop_front().or_else(|| g.bg.pop_front()) {
                    // counted while the queue lock is held, so "empty and not busy" is never a gap
                    busy.fetch_add(1, SeqCst);
                    break j;
                }
                g = q.1.wait(g).unwrap_or_else(|e| e.into_inner());
            }
        };
        if (job.stale)() || (job.prio == Prio::Background && paused()) {
            (job.dropped)();
        } else {
            // a panicking job must not take its worker with it, and whoever waits on it hears about it
            let run = job.run;
            if catch_unwind(AssertUnwindSafe(run)).is_err() {
                (job.dropped)();
            }
        }
        busy.fetch_sub(1, SeqCst);
    }
}

static POOL: OnceLock<Pool> = OnceLock::new();

/// Sets the worker count (first call wins; later calls are ignored).
pub fn init(workers: usize) {
    POOL.get_or_init(|| Pool::new(workers, crate::rate::paused_now));
}

pub fn global() -> &'static Pool {
    POOL.get_or_init(|| Pool::new(4, crate::rate::paused_now))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    use std::time::Duration;

    fn never() -> bool {
        false
    }

    fn wait_for(f: impl Fn() -> bool) {
        for _ in 0..3000 {
            if f() {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("timed out");
    }

    #[test]
    fn never_more_than_the_cap_in_flight() {
        let p = Pool::new(2, never);
        let (now, peak, done) = (
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
        );
        for _ in 0..8 {
            let (now, peak, done) = (now.clone(), peak.clone(), done.clone());
            p.submit(
                Prio::User,
                never,
                move || {
                    let n = now.fetch_add(1, SeqCst) + 1;
                    peak.fetch_max(n, SeqCst);
                    std::thread::sleep(Duration::from_millis(15));
                    now.fetch_sub(1, SeqCst);
                    done.fetch_add(1, SeqCst);
                },
                || {},
            );
        }
        wait_for(|| done.load(SeqCst) == 8);
        assert_eq!(peak.load(SeqCst), 2);
    }

    #[test]
    fn user_work_goes_before_background_and_stale_jobs_never_start() {
        let p = Pool::new(1, never);
        let order = Arc::new(Mutex::new(vec![]));
        let gate = Arc::new(Mutex::new(()));
        let held = gate.lock().unwrap();
        // the single worker is busy until the gate opens, so everything below queues up
        let g2 = gate.clone();
        let started = Arc::new(AtomicUsize::new(0));
        let st = started.clone();
        p.submit(
            Prio::User,
            never,
            move || {
                st.store(1, SeqCst);
                drop(g2.lock());
            },
            || {},
        );
        wait_for(|| started.load(SeqCst) == 1);
        let stale = Arc::new(Mutex::new(false));
        for (name, prio) in [
            ("bg1", Prio::Background),
            ("bg2", Prio::Background),
            ("user", Prio::User),
        ] {
            let o = order.clone();
            p.submit(prio, never, move || o.lock().unwrap().push(name), || {});
        }
        let (o, d, s) = (order.clone(), order.clone(), stale.clone());
        p.submit(
            Prio::User,
            move || *s.lock().unwrap(),
            move || o.lock().unwrap().push("stale-ran"),
            move || d.lock().unwrap().push("stale-dropped"),
        );
        *stale.lock().unwrap() = true;
        assert_eq!(p.queued(), (2, 2));
        drop(held);
        wait_for(|| order.lock().unwrap().len() == 4);
        assert_eq!(
            *order.lock().unwrap(),
            ["user", "stale-dropped", "bg1", "bg2"]
        );
    }

    #[test]
    fn the_background_queue_is_bounded_and_paused_background_work_stands_down() {
        let p = Pool::new(1, never);
        let gate = Arc::new(Mutex::new(()));
        let held = gate.lock().unwrap();
        let g2 = gate.clone();
        let started = Arc::new(AtomicUsize::new(0));
        let st = started.clone();
        p.submit(
            Prio::User,
            never,
            move || {
                st.store(1, SeqCst);
                drop(g2.lock());
            },
            || {},
        );
        wait_for(|| started.load(SeqCst) == 1);
        let dropped = Arc::new(AtomicUsize::new(0));
        for _ in 0..70 {
            let d = dropped.clone();
            p.submit(
                Prio::Background,
                never,
                || {},
                move || {
                    d.fetch_add(1, SeqCst);
                },
            );
        }
        assert_eq!(p.queued().1, 64);
        assert_eq!(dropped.load(SeqCst), 6, "overflow is dropped, not queued");
        drop(held);

        fn yes() -> bool {
            true
        }
        let p = Pool::new(1, yes);
        let ran = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicUsize::new(0));
        for prio in [Prio::Background, Prio::User, Prio::Probe] {
            let (r, d) = (ran.clone(), dropped.clone());
            p.submit(
                prio,
                never,
                move || {
                    r.fetch_add(1, SeqCst);
                },
                move || {
                    d.fetch_add(1, SeqCst);
                },
            );
        }
        wait_for(|| ran.load(SeqCst) + dropped.load(SeqCst) == 3);
        assert_eq!(
            (ran.load(SeqCst), dropped.load(SeqCst)),
            (2, 1),
            "only plain background work pauses"
        );
    }

    #[test]
    fn a_panicking_job_reports_failure_and_leaves_the_pool_whole() {
        let p = Pool::new(1, never);
        let (failed, ran) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        for _ in 0..3 {
            let f = failed.clone();
            p.submit(
                Prio::User,
                never,
                || panic!("boom"),
                move || {
                    f.fetch_add(1, SeqCst);
                },
            );
        }
        let r = ran.clone();
        p.submit(
            Prio::User,
            never,
            move || {
                r.fetch_add(1, SeqCst);
            },
            || {},
        );
        wait_for(|| ran.load(SeqCst) == 1);
        assert_eq!(
            failed.load(SeqCst),
            3,
            "each panic is reported through the job's failure path"
        );
        // still one worker, still working after three panics
        let r = ran.clone();
        p.submit(
            Prio::Background,
            never,
            move || {
                r.fetch_add(1, SeqCst);
            },
            || {},
        );
        wait_for(|| ran.load(SeqCst) == 2);
    }
}
