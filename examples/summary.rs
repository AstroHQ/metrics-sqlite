use std::time::Duration;

use chrono::{DateTime, Local, TimeZone};
use clap::Parser;
use metrics_sqlite::MetricsDb;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

#[derive(Parser)]
struct Args {
    /// Path to the metrics database file
    db_path: String,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let fmt_layer = fmt::layer();
    let filter_layer = EnvFilter::try_from_default_env().or_else(|_| EnvFilter::try_new("info"))?;
    tracing_subscriber::registry()
        .with(filter_layer)
        .with(fmt_layer)
        .init();

    let mut db = MetricsDb::new(&args.db_path)?;
    let session = db.session_from_signpost("liquid.connected")?;
    let averages = db.average_metrics_from_signpost(
        "liquid.connected",
        &[
            "net_quality.rtt",
            "net_quality.throughput",
            // "rate_control.metered_throughput",
        ],
    )?;

    println!("Last connection:");
    println!("  Start:    {}", format_timestamp(session.start_time));
    println!("  End:      {}", format_timestamp(session.end_time));
    println!("  Duration: {}", format_duration(session.duration));

    if let Some(&rtt) = averages.get("net_quality.rtt") {
        println!("RTT: Average {rtt:.2}ms");
    }
    if let Some(&throughput) = averages.get("net_quality.throughput") {
        println!("Throughput: Average {:.2}KB/s", throughput / 1024.0);
    }
    if let Some(&metered) = averages.get("rate_control.metered_throughput") {
        println!("Metered: Average {:.2}KB/s", metered / 1024.0);
    }

    Ok(())
}

fn format_timestamp(secs: f64) -> String {
    let dt: DateTime<Local> = Local
        .timestamp_opt(secs as i64, ((secs.fract()) * 1_000_000_000.0) as u32)
        .unwrap();
    dt.format("%Y-%m-%d %H:%M:%S").to_string()
}

fn format_duration(d: Duration) -> String {
    let total_secs = d.as_secs();
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;
    let millis = d.subsec_millis();
    if hours > 0 {
        format!("{hours}h {minutes:02}m {seconds:02}.{millis:03}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds:02}.{millis:03}s")
    } else {
        format!("{seconds}.{millis:03}s")
    }
}
