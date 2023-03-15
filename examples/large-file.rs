use metrics::{counter, gauge};
use metrics_sqlite::SqliteExporter;
use std::time::Duration;

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
    pretty_env_logger::formatted_builder()
        .filter(None, log::LevelFilter::Trace)
        .init();
    setup_metrics();
    metrics::describe_counter!("video.counter", "video frames");
    metrics::describe_gauge!(
        "net.quality.rate",
        metrics::Unit::MegabitsPerSecond,
        "Quality's bit rate"
    );

    loop {
        gauge!("rate_control.previous_metered_throughput", 2134.0);
        gauge!("rate_control.metered_throughput", 2134.0);
        gauge!("rate_control.smoothed_rtt", 2133.0);
        gauge!("rate_control.raw_rtt", 2341.0);
        counter!("video.counter", 1);
        counter!("net.packets", 2);
        gauge!("net.quality.rate", 231.2);
        // let start = std::time::Instant::now();
        std::thread::sleep(Duration::from_micros(1000));
        // timing!("net.time.delay", start, std::time::Instant::now());
    }
}
