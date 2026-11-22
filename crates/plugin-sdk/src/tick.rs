//! Wall-clock aligned timers: threads that sleep until the next whole second/minute,
//! so displayed times change exactly on the boundary and never drift.

use crate::Tx;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn until_next(period: Duration) -> Duration {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let p = period.as_millis().max(1);
    Duration::from_millis((p - now.as_millis() % p) as u64)
}

/// Send `f()` every `period`, aligned to multiples of `period` since the epoch.
pub fn every<M: Send + 'static>(period: Duration, tx: Tx<M>, f: impl Fn() -> M + Send + 'static) {
    std::thread::spawn(move || loop {
        std::thread::sleep(until_next(period) + Duration::from_millis(5));
        if !tx.send(f()) {
            return;
        }
    });
}

pub fn every_second<M: Send + 'static>(tx: Tx<M>, f: impl Fn() -> M + Send + 'static) {
    every(Duration::from_secs(1), tx, f)
}

pub fn every_minute<M: Send + 'static>(tx: Tx<M>, f: impl Fn() -> M + Send + 'static) {
    every(Duration::from_secs(60), tx, f)
}
