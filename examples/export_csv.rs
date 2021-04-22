fn main() {
    let db = metrics_sqlite::MetricsDb::new("metrics.db").expect("Failed to open DB");
    db.export_to_csv("metrics.csv")
        .expect("Failed to export to CSV");
}
