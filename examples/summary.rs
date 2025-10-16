use metrics_sqlite::{MetricsDb, Session};
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

fn main() -> anyhow::Result<()> {
    let fmt_layer = fmt::layer();
    let filter_layer = EnvFilter::try_from_default_env().or_else(|_| EnvFilter::try_new("info"))?;
    tracing_subscriber::registry()
        .with(filter_layer)
        .with(fmt_layer)
        .init();
    let mut db = MetricsDb::new("metrics.db")?;
    let session = db.session_from_signpost("liquid.connected")?;
    let rtt = average(&mut db, "net_quality.rtt", &session)?;
    let throughput = average(&mut db, "net_quality.throughput", &session)? / 1024.0; // KB/s
    let metered_throughput =
        average(&mut db, "rate_control.metered_throughput", &session)? / 1024.0; // KB/s
    println!("Last connection: {session:#?}");
    println!("RTT: Average {rtt:.2}ms");
    println!("Throughput: Average {throughput:.2}KB/s");
    println!("Metered: Average {metered_throughput:.2}KB/s");

    Ok(())
}
fn average(db: &mut MetricsDb, key: &str, session: &Session) -> anyhow::Result<f64> {
    let metrics = db.metrics_for_key(key, Some(session))?;
    let sum: f64 = metrics.iter().map(|m| m.value).sum();
    let samples = metrics.len();
    let average = sum / samples as f64;
    Ok(average)
}
