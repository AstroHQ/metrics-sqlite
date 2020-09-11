use metrics_sqlite::MetricsDb;

fn main() {
    let db = MetricsDb::new("metrics.db").unwrap();
    println!("Keys: {}", db.available_keys().unwrap().join(", "));
    let sessions = db.sessions();
    for (i, s) in sessions.into_iter().enumerate() {
        println!(
            "Session {}: {:.2}s long ({:.2} - {:.2})",
            i + 1,
            s.duration.as_secs_f64(),
            s.start_time,
            s.end_time
        );
        let m = db.metrics_for_key("net.packets", Some(s)).unwrap();
        println!("{} metrics for net.packets", m.len());
    }
    let m = db.metrics_for_key("net.packets", None).unwrap();
    println!("{} total metrics for net.packets", m.len());
}
