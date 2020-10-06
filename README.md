# SQLite Observer & Exporter for SQLite

This provides a fairly simple SQLite powered backend for the [metrics](https://crates.io/crates/metrics) crate, useful for offline or desktop applications to gather metrics that can be easily queried afterwards.

## Example

Example using `metrics_runtime` crate & sampling the metrics every 1s:

```Rust
let receiver = Receiver::builder()
    .build()
    .expect("failed to create receiver");

let mut exporter = SqliteExporter::new(
    receiver.controller(),
    SqliteBuilder::new(),
    Duration::from_secs(1),
    "metrics.db",
)
.expect("Failed to create SqliteExporter");
receiver.install();
exporter.run();
```
