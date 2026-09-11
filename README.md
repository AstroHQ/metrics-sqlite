# SQLite Observer & Exporter for SQLite

[![Rust](https://github.com/AstroHQ/metrics-sqlite/actions/workflows/rust.yml/badge.svg)](https://github.com/AstroHQ/metrics-sqlite/actions/workflows/rust.yml)
[![docs](https://docs.rs/metrics-sqlite/badge.svg)](https://docs.rs/metrics-sqlite/)
![Crates.io](https://img.shields.io/crates/l/metrics-sqlite)


This provides a fairly simple SQLite powered backend for the [metrics](https://crates.io/crates/metrics) crate, useful for offline or desktop applications to gather metrics that can be easily queried afterwards.

## Storage and housekeeping

Startup retention cleanup runs on the background worker. Construction still
opens and migrates the database and runs `quick_check`, so opening a large
database can still take time. Cleanup is asynchronous: old rows can remain
visible until it finishes, and shutdown does not wait for pending cleanup.

Periodic housekeeping is disabled by default. Enable it to enforce retention
throughout a long-running session:

```rust,ignore
exporter.set_periodic_housekeeping(
    Some(Duration::from_secs(30 * 60)),
    None, // inherit keep_duration from SqliteExporter::new
    Some(1_000_000),
);
```

An explicit retention overrides the constructor's value. To use only a record
limit, also pass `None` for `keep_duration` when constructing the exporter.
Passing `None` for the periodic duration disables periodic and row-triggered
housekeeping and cancels any remaining batches of an active periodic pass.
Pending startup cleanup still completes using the constructor's retention
policy. When enabled, housekeeping also starts after 100,000 successful
inserts, or one quarter of the record limit clamped to 1,000–100,000 inserts,
with at least one second between row-triggered starts. Limits are cleanup
targets, not hard caps: queued writes and incoming traffic can exceed them.
When the record limit is exceeded, cleanup removes the excess plus 25% of the
limit, preserving the existing headroom policy.

Deletes commit at most 1,000 rows per batch, with at least 10 ms after each batch
so the worker can process incoming events. After cleanup, a full VACUUM runs
only if at least 64 MiB and 25% of the database consists of free pages, and no
more than once per hour per worker, including failed attempts. A failed VACUUM
does not prevent a checkpoint of completed deletes. Smaller amounts of free space remain
available for future inserts. VACUUM still pauses the worker and requires
temporary disk space; heavy incoming traffic can fill the channel during it.
No database-format conversion is required for existing files.

Connections set a 4 MiB `journal_size_limit`. Cleanup and graceful shutdown
also attempt a WAL truncate checkpoint without waiting for readers. As
[SQLite documents](https://www.sqlite.org/pragma.html#pragma_journal_size_limit),
the size limit applies when the WAL resets; it is not a hard disk-usage cap.
Long-running readers can keep the WAL larger until their transactions end.

All gauge samples and both metric indexes are retained. Gauge sampling would
change recorded history and sample-weighted averages; index and insertion-path
changes need representative release-build benchmarks before choosing a tradeoff.

## Version 0.4 Notes

- Now works with metrics 0.20.x
- _register!() macros aren't required & don't do anything with this exporter currently
- Unit/description now available via _describe!() macros metrics provides

## Version 0.3 Notes

- Historically vacuumed on setup; see the current housekeeping behavior above
- Migration of database blows away 0.2 data unfortunately

## Example

```Rust
    let exporter = SqliteExporter::new(
        Duration::from_secs(30), // flush to sqlite on disk every 30s (or internal buffer limit)
        Some(Duration::from_secs(60 * 60 * 24 * 7)), // 60 sec * 60 min * 24 hours * 7 days
        "metrics.db",
    )
    .expect("Failed to create SqliteExporter");
    exporter
        .install()
        .expect("Failed to install SqliteExporter");

// use metrics macros etc.
metrics::gauge!("mykey", 1.0);
```
