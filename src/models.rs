//! Diesel models of metrics sqlite storage
use crate::schema::metrics;

/// A new metric measurement for storing into sqlite database
#[derive(Insertable, Debug)]
#[table_name = "metrics"]
pub struct NewMetric {
    /// Timestamp of sample
    pub timestamp: f64,
    /// Key/name of sample
    pub key: String,
    /// Value of sample
    pub value: f64,
}

/// Metric model for existing entries in sqlite database
#[derive(Queryable, Debug)]
pub struct Metric {
    /// Unique ID of sample
    pub id: i64,
    /// Timestamp of sample
    pub timestamp: f64,
    /// Key/name of sample
    pub key: String,
    /// Value of sample
    pub value: f64,
}
