use metrics::{counter, gauge, timing};
use metrics_runtime::Receiver;
use metrics_sqlite::{SqliteBuilder, SqliteExporter};
use std::time::Duration;

fn setup_metrics() {
    std::thread::spawn(move || {
        let receiver = Receiver::builder()
            .build()
            .expect("failed to create receiver");

        let mut exporter = SqliteExporter::new(
            receiver.controller(),
            SqliteBuilder::new(),
            Duration::from_secs(5),
            Duration::from_secs(60 * 60 * 24 * 7), // 60s * 60min * 24 hours * 7 days
            "metrics.db",
        )
        .expect("Failed to create SqliteExporter");
        receiver.install();
        exporter.run();
    });
}
fn main() {
    pretty_env_logger::formatted_builder()
        .filter(None, log::LevelFilter::Trace)
        .init();
    setup_metrics();
    loop {
        counter!("video.counter", 1);
        counter!("net.packets", 2);
        gauge!("net.quality.rate", 231);
        let start = std::time::Instant::now();
        std::thread::sleep(Duration::from_millis(500));
        timing!("net.time.delay", start, std::time::Instant::now());
    }
}
