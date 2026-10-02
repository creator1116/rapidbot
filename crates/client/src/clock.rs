//! `Util.getMillis()` / `Util.getNanos()`: Java's `System.nanoTime()`.
//!
//! nanoTime has an arbitrary origin; on Windows and Linux it tracks time since
//! boot. Anything derived from it that reaches the server (status ping
//! payloads, for one) should look like a machine's uptime, not Unix time.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use rand::Rng;

struct Origin {
    start: Instant,
    /// Pretend uptime at `start`.
    base: Duration,
}

fn origin() -> &'static Origin {
    static ORIGIN: OnceLock<Origin> = OnceLock::new();
    ORIGIN.get_or_init(|| Origin {
        start: Instant::now(),
        // One machine, one clock: every bot in this process shares it.
        base: Duration::from_millis(rand::thread_rng().gen_range(3_600_000..172_800_000)),
    })
}

pub fn nanos() -> i64 {
    let o = origin();
    (o.base + o.start.elapsed()).as_nanos() as i64
}

pub fn millis() -> i64 {
    nanos() / 1_000_000
}
