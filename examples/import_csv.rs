use metrics_sqlite::MetricsDb;

fn main() {
    pretty_env_logger::formatted_builder()
        .filter(None, log::LevelFilter::Debug)
        .init();
    MetricsDb::import_from_csv("metrics.csv", "imported.db").unwrap();
    println!("Success");
}
