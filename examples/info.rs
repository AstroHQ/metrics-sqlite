use metrics_sqlite::MetricsDb;

fn main() {
    let db = MetricsDb::new("metrics.db").unwrap();
    println!("Keys: {:?}", db.available_keys().unwrap());
    let m = db.deriv_metrics_for_key("net.packets").unwrap();
    println!("{} metrics for net.packets", m.len());
}
