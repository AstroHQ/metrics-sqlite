//! Local load harness for stress-testing the metrics-sqlite worker.
//!
//! Run with:
//!
//!     RUST_LOG=info cargo run --example chaos
//!
//! It writes to `chaos.db` in the working directory and emits metrics from
//! several threads as fast as it can. By itself it just generates load —
//! the database stays healthy and the logs stay quiet.
//!
//! To exercise the failure / recovery paths you induce errors from outside
//! the process. The crate runs SQLite in WAL mode with `busy_timeout=5000`,
//! so the realistic external attacks are:
//!
//! ### 1) Hold a write lock for longer than the 5s busy timeout
//!
//! In another terminal while chaos is running:
//!
//!     ( echo "BEGIN IMMEDIATE; SELECT 1;"; sleep 90 ) | sqlite3 chaos.db
//!
//! Each commit attempt in the worker waits up to 5s, then errors with
//! "database is locked". You should see at most one such ERROR line in the
//! log per minute, the first one accompanied by a throttle suppression
//! count after the second minute. After three consecutive failures the
//! worker logs a "connection is broken, reconnecting" WARN; reconnect
//! attempts are gated at one per 30s while the lock is held. When the
//! lock releases, the next flush succeeds silently.
//!
//! ### 2) Make commits fail by running out of disk
//!
//! Point `chaos.db` at a tiny ramdisk so the worker hits SQLITE_FULL /
//! disk-I/O errors during real commits. On macOS:
//!
//!     DEV=$(hdiutil attach -nomount ram://2048)   # ~1 MiB ramdisk
//!     diskutil eraseVolume APFS small "$DEV"
//!     # then run chaos with the working directory set to /Volumes/small
//!
//! ### What healthy behavior looks like
//!
//! - At most ~1 "Error flushing metrics: ..." line per minute, with a
//!   "(N similar errors suppressed)" suffix once suppression has work to do.
//! - At most ~1 "Error sending metric ..." line per minute if the worker
//!   channel ever fills.
//! - "metrics-sqlite database connection is broken, reconnecting" at most
//!   once per 30s while the problem persists.
//! - "metrics-sqlite queue exceeded 100000 items" if a failure mode keeps
//!   the worker from draining for that long.
//!
//! When the external problem is fixed, the next reconnect restores the
//! pipeline and the log goes quiet.

use metrics::{counter, gauge, histogram};
use metrics_sqlite::SqliteExporter;
use std::time::{Duration, Instant};
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

const DB_PATH: &str = "chaos.db";
const WORKER_THREADS: usize = 4;
const PER_THREAD_RATE_HZ: u64 = 2_000;

fn main() {
    tracing_subscriber::registry()
        .with(
            EnvFilter::try_from_default_env()
                .or_else(|_| EnvFilter::try_new("info,metrics_sqlite=info"))
                .unwrap(),
        )
        .with(fmt::layer())
        .init();

    let exporter = SqliteExporter::new(Duration::from_millis(250), None, DB_PATH)
        .expect("Failed to create SqliteExporter");
    exporter
        .install()
        .expect("Failed to install SqliteExporter");

    tracing::info!(
        "chaos running: writing to {DB_PATH}, {WORKER_THREADS} threads at ~{PER_THREAD_RATE_HZ} Hz each. Break the DB from another terminal to repro; Ctrl-C to stop."
    );

    let started = Instant::now();
    let mut handles = Vec::new();
    for worker_id in 0..WORKER_THREADS {
        handles.push(std::thread::spawn(move || {
            let period = Duration::from_micros(1_000_000 / PER_THREAD_RATE_HZ);
            let mut next = Instant::now();
            loop {
                counter!("chaos.events", "worker" => worker_id.to_string()).increment(1);
                gauge!("chaos.elapsed_secs").set(started.elapsed().as_secs_f64());
                histogram!("chaos.iteration_us").record(period.as_micros() as f64);
                next += period;
                if let Some(remaining) = next.checked_duration_since(Instant::now()) {
                    std::thread::sleep(remaining);
                } else {
                    // Falling behind; reset so we don't busy-spin catching up.
                    next = Instant::now();
                }
            }
        }));
    }
    for h in handles {
        let _ = h.join();
    }
}
