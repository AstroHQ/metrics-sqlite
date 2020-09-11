//! Metrics DB, to use/query/etc metrics SQLite databases
use super::{models::Metric, setup_db, Result};
use diesel::prelude::*;
use std::path::Path;

/// Calculated metric type from deriv_metrics_for_key()
#[derive(Debug)]
pub struct DerivMetric {
    pub timestamp: f64,
    pub key: String,
    pub value: f64,
}
/// Metrics database, useful for querying stored metrics
pub struct MetricsDb {
    db: SqliteConnection,
}

impl MetricsDb {
    /// Creates a new metrics DB with given path of a SQLite database
    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self> {
        let db = setup_db(path)?;
        Ok(MetricsDb { db })
    }

    /// Returns list of metrics keys stored in the database
    pub fn available_keys(&self) -> Result<Vec<String>> {
        use crate::schema::metrics::dsl::*;
        let r = metrics.select(key).distinct().load::<String>(&self.db)?;
        Ok(r)
    }

    /// Returns all metrics for given key in ascending timestamp order
    pub fn metrics_for_key(&self, key_name: &str) -> Result<Vec<Metric>> {
        use crate::schema::metrics::dsl::*;
        let r = metrics
            .order(timestamp.asc())
            .filter(key.eq(key_name))
            .load::<Metric>(&self.db)?;
        Ok(r)
    }

    /// Returns rate of change, the derivative, of the given metrics key's values
    ///
    /// f(t) = (x(t + 1) - x(t)) / ((t+1) - (t)
    pub fn deriv_metrics_for_key(&self, key_name: &str) -> Result<Vec<DerivMetric>> {
        let m = self.metrics_for_key(key_name)?;
        let new_values: Vec<_> = m
            .windows(2)
            .map(|v| {
                let new_value =
                    (v[1].value - v[0].value) as f64 / (v[1].timestamp - v[0].timestamp);
                DerivMetric {
                    timestamp: v[1].timestamp,
                    key: format!("{}.deriv", key_name),
                    value: new_value,
                }
            })
            .collect();
        Ok(new_values)
    }
}
