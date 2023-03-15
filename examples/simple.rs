use metrics::{counter, gauge};
// use metrics_runtime::Receiver;
use metrics_sqlite::SqliteExporter;
use std::time::Duration;

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
    pretty_env_logger::formatted_builder()
        .filter(None, log::LevelFilter::Trace)
        .init();
    setup_metrics();
    metrics::describe_counter!("video.counter", "video frames or something");
    metrics::describe_gauge!(
        "net.quality.rate",
        metrics::Unit::MegabitsPerSecond,
        "description here"
    );
    loop {
        counter!("video.counter", 1);
        counter!("net.packets", 2);
        gauge!("net.quality.rate", 231.2);
        // let start = std::time::Instant::now();
        std::thread::sleep(Duration::from_millis(100));
        // timing!("net.time.delay", start, std::time::Instant::now());
    }
}
