use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const MAX_FAILURES: usize = 5;
pub const WINDOW: Duration = Duration::from_secs(15 * 60);
const PRUNE_ABOVE: usize = 10_000;

#[derive(Default)]
pub struct FailureLimiter {
    failures: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl FailureLimiter {
    pub fn blocked(&self, keys: &[String]) -> bool {
        let mut failures = self.lock();
        keys.iter().any(|key| {
            failures
                .get_mut(key)
                .is_some_and(|times| recent(times).len() >= MAX_FAILURES)
        })
    }

    pub fn record(&self, keys: &[String]) {
        let mut failures = self.lock();
        if failures.len() > PRUNE_ABOVE {
            failures.retain(|_, times| !recent(times).is_empty());
        }
        for key in keys {
            failures
                .entry(key.clone())
                .or_default()
                .push_back(Instant::now());
        }
    }

    pub fn clear(&self, key: &str) {
        self.lock().remove(key);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, VecDeque<Instant>>> {
        self.failures
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn recent(times: &mut VecDeque<Instant>) -> &mut VecDeque<Instant> {
    while times.front().is_some_and(|time| time.elapsed() > WINDOW) {
        times.pop_front();
    }
    times
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_after_five_failures() {
        let limiter = FailureLimiter::default();
        let keys = ["account".to_string(), "ip".to_string()];
        for _ in 0..MAX_FAILURES {
            assert!(!limiter.blocked(&keys));
            limiter.record(&keys);
        }
        assert!(limiter.blocked(&keys));
        assert!(limiter.blocked(&["ip".to_string()]));
        assert!(!limiter.blocked(&["other".to_string()]));
    }
}
