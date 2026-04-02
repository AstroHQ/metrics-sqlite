use metrics::{counter, gauge};
use metrics_sqlite::SqliteExporter;
use std::time::Duration;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, fmt};

fn setup_metrics() {
    let exporter = SqliteExporter::new(
        Duration::from_millis(250),
        Some(Duration::from_secs(60 * 60 * 24 * 7)), // 60 sec * 60 min * 24 hours * 7 days
        "metrics-large.db",
    )
    .expect("Failed to create SqliteExporter");
    exporter.set_periodic_housekeeping(Some(Duration::from_secs(10)), None, Some(1_000_000));
    exporter
        .install()
        .expect("Failed to install SqliteExporter");
}
fn main() {
    let fmt_layer = fmt::layer();
    let filter_layer = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("info"))
        .unwrap();
    tracing_subscriber::registry()
        .with(filter_layer)
        .with(fmt_layer)
        .init();

    setup_metrics();
    metrics::describe_counter!("video.counter", "video frames");
    metrics::describe_gauge!(
        "net.quality.rate",
        metrics::Unit::MegabitsPerSecond,
        "Quality's bit rate"
    );

    loop {
        gauge!("rate_control.previous_metered_throughput").set(2134.0);
        gauge!("rate_control.metered_throughput").set(2134.0);
        gauge!("rate_control.smoothed_rtt").set(2133.0);
        gauge!("rate_control.raw_rtt").set(2341.0);
        counter!("video.counter").increment(1);
        counter!("net.packets").increment(2);
        gauge!("net.quality.rate").set(231.2);
        // let start = std::time::Instant::now();
        std::thread::sleep(Duration::from_micros(1000));
        // timing!("net.time.delay", start, std::time::Instant::now());
    }
}
