use metrics::{counter, gauge};
use metrics_sqlite::SqliteExporter;
use std::time::Duration;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

fn setup_metrics() {
    let exporter = SqliteExporter::new(
        Duration::from_secs(30),
        Some(Duration::from_secs(60 * 60 * 24 * 7)), // 60 sec * 60 min * 24 hours * 7 days
        "metrics.db",
    )
    .expect("Failed to create SqliteExporter");
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
    metrics::describe_counter!("video.counter", "video frames or something");
    metrics::describe_gauge!(
        "net.quality.rate",
        metrics::Unit::MegabitsPerSecond,
        "description here"
    );
    loop {
        counter!("video.counter").increment(1);
        counter!("net.packets").increment(2);
        gauge!("net.quality.rate").set(231.2);
        // let start = std::time::Instant::now();
        std::thread::sleep(Duration::from_millis(100));
        // timing!("net.time.delay", start, std::time::Instant::now());
    }
}
