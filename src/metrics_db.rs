//! Metrics DB, to use/query/etc metrics SQLite databases
use super::{models::Metric, setup_db, Result};
use crate::models::MetricKey;
use crate::MetricsError;
use diesel::prelude::*;
use std::path::Path;
use std::time::Duration;

/// Threshold to separate samples into sessions by
const SESSION_TIME_GAP_THRESHOLD: Duration = Duration::from_secs(30);

/// Calculated metric type from deriv_metrics_for_key()
#[derive(Debug)]
pub struct DerivMetric {
    pub timestamp: f64,
    pub key: String,
    pub value: f64,
}
/// Describes a session, which is a sub-set of metrics data based on time gaps
pub struct Session {
    /// Timestamp session starts at
    pub start_time: f64,
    /// Timestamp session ends at
    pub end_time: f64,
    /// Duration of session
    pub duration: Duration,
}
impl Session {
    /// Creates a new session with given start & end, calculating duration from them
    pub fn new(start_time: f64, end_time: f64) -> Self {
        Session {
            start_time,
            end_time,
            duration: Duration::from_secs_f64(end_time - start_time),
        }
    }
}
/// Metrics database, useful for querying stored metrics
pub struct MetricsDb {
    db: SqliteConnection,
    sessions: Vec<Session>,
}

impl MetricsDb {
    /// Creates a new metrics DB with given path of a SQLite database
    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self> {
        let db = setup_db(path)?;
        let sessions = Self::process_sessions(&db)?;
        Ok(MetricsDb { db, sessions })
    }

    /// Returns sessions in database, based on `SESSION_TIME_GAP_THRESHOLD`
    pub fn sessions(&self) -> &[Session] {
        &self.sessions
    }

    fn process_sessions(db: &SqliteConnection) -> Result<Vec<Session>> {
        use crate::schema::metrics::dsl::*;
        let timestamps = metrics
            .select(timestamp)
            .order(timestamp.asc())
            .load::<f64>(db)?;
        if timestamps.is_empty() {
            return Err(MetricsError::EmptyDatabase);
        }
        let mut sessions: Vec<Session> = Vec::new();
        let mut current_start = timestamps[0];
        for pair in timestamps.windows(2) {
            if pair[1] - pair[0] > SESSION_TIME_GAP_THRESHOLD.as_secs_f64() {
                sessions.push(Session::new(current_start, pair[0]));
                current_start = pair[1];
            }
        }
        if let Some(last) = timestamps.last() {
            if current_start < *last {
                sessions.push(Session::new(current_start, *last));
            }
        }

        Ok(sessions)
    }

    /// Returns list of metrics keys stored in the database
    pub fn available_keys(&self) -> Result<Vec<String>> {
        use crate::schema::metric_keys::dsl::*;
        let r = metric_keys
            .select(key)
            .distinct()
            .load::<String>(&self.db)?;
        Ok(r)
    }

    /// Returns all metrics for given key in ascending timestamp order
    pub fn metrics_for_key(
        &self,
        key_name: &str,
        session: Option<&Session>,
    ) -> Result<Vec<Metric>> {
        use crate::schema::metrics::dsl::*;
        let metric_key = self.metric_key_for_key(key_name)?;
        let query = metrics
            .order(timestamp.asc())
            .filter(metric_key_id.eq(metric_key.id));
        let r = match session {
            Some(session) => query
                .filter(timestamp.ge(session.start_time))
                .filter(timestamp.le(session.end_time))
                .load::<Metric>(&self.db)?,
            None => query.load::<Metric>(&self.db)?,
        };
        Ok(r)
    }

    fn metric_key_for_key(&self, key_name: &str) -> Result<MetricKey> {
        use crate::schema::metric_keys::dsl::*;
        let query = metric_keys.filter(key.eq(key_name));
        let keys = query.load::<MetricKey>(&self.db)?;
        keys.into_iter()
            .next()
            .ok_or_else(|| MetricsError::KeyNotFound(key_name.to_string()))
    }

    /// Returns rate of change, the derivative, of the given metrics key's values
    ///
    /// f(t) = (x(t + 1) - x(t)) / ((t+1) - (t)
    pub fn deriv_metrics_for_key(
        &self,
        key_name: &str,
        session: Option<&Session>,
    ) -> Result<Vec<DerivMetric>> {
        let m = self.metrics_for_key(key_name, session)?;
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

    /// Exports DB contents to CSV file
    #[cfg(feature = "export_csv")]
    pub fn export_to_csv<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        use crate::schema::metrics::dsl::*;
        use std::fs::File;
        let out_file = File::create(path)?;
        let mut csv_writer = csv::Writer::from_writer(out_file);
        let query = metrics.order(timestamp.asc());
        for row in query.load::<Metric>(&self.db)? {
            csv_writer.serialize(row)?;
        }
        csv_writer.flush()?;
        Ok(())
    }
}
