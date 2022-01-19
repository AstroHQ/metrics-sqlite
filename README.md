# SQLite Observer & Exporter for SQLite

[![Rust](https://github.com/AstroHQ/metrics-sqlite/actions/workflows/rust.yml/badge.svg)](https://github.com/AstroHQ/metrics-sqlite/actions/workflows/rust.yml)
[![docs](https://docs.rs/metrics-sqlite/badge.svg)](https://docs.rs/metrics-sqlite/)
![Crates.io](https://img.shields.io/crates/l/metrics-sqlite)


This provides a fairly simple SQLite powered backend for the [metrics](https://crates.io/crates/metrics) crate, useful for offline or desktop applications to gather metrics that can be easily queried afterwards.

## Version 0.3 Notes

- Now vacuums on setup which can incur a delay in being ready to record
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
