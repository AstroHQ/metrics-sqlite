//! Metrics DB, to use/query/etc metrics SQLite databases
use super::{models::Metric, setup_db, Result};
use crate::models::MetricKey;
use crate::MetricsError;
use diesel::prelude::*;
#[cfg(feature = "import_csv")]
use serde::Deserialize;
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
#[derive(Debug, Copy, Clone)]
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
        let mut db = setup_db(path)?;
        let sessions = Self::process_sessions(&mut db)?;
        Ok(MetricsDb { db, sessions })
    }

    /// Returns sessions in database, based on `SESSION_TIME_GAP_THRESHOLD`
    pub fn sessions(&self) -> Vec<Session> {
        self.sessions.clone()
    }

    fn process_sessions(db: &mut SqliteConnection) -> Result<Vec<Session>> {
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
    pub fn available_keys(&mut self) -> Result<Vec<String>> {
        use crate::schema::metric_keys::dsl::*;
        let r = metric_keys
            .select(key)
            .distinct()
            .load::<String>(&mut self.db)?;
        Ok(r)
    }

    /// Returns all metrics for given key in ascending timestamp order
    pub fn metrics_for_key(
        &mut self,
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
                .load::<Metric>(&mut self.db)?,
            None => query.load::<Metric>(&mut self.db)?,
        };
        Ok(r)
    }

    fn metric_key_for_key(&mut self, key_name: &str) -> Result<MetricKey> {
        use crate::schema::metric_keys::dsl::*;
        let query = metric_keys.filter(key.eq(key_name));
        let keys = query.load::<MetricKey>(&mut self.db)?;
        keys.into_iter()
            .next()
            .ok_or_else(|| MetricsError::KeyNotFound(key_name.to_string()))
    }

    /// Returns rate of change, the derivative, of the given metrics key's values
    ///
    /// f(t) = (x(t + 1) - x(t)) / ((t+1) - (t)
    pub fn deriv_metrics_for_key(
        &mut self,
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
    pub fn export_to_csv<P: AsRef<Path>>(&mut self, path: P) -> Result<()> {
        use crate::schema::metric_keys::dsl::key;
        use crate::schema::metrics::dsl::*;
        use std::fs::File;
        let out_file = File::create(path)?;
        let mut csv_writer = csv::Writer::from_writer(out_file);
        // join the 2 tables so we get a flat CSV with the actual key names
        let query = crate::schema::metrics::table.inner_join(crate::schema::metric_keys::table);
        let query = query
            .order(timestamp.asc())
            .select((id, timestamp, key, value));
        for row in query.load::<JoinedMetric>(&mut self.db)? {
            csv_writer.serialize(row)?;
        }
        csv_writer.flush()?;
        Ok(())
    }
    /// Imports CSV file into a MetricsDb file
    #[cfg(feature = "import_csv")]
    pub fn import_from_csv<S: AsRef<Path>, D: AsRef<Path>>(path: S, destination: D) -> Result<()> {
        use crate::InnerState;
        use csv::ReaderBuilder;
        let db = setup_db(destination)?;
        let mut reader = ReaderBuilder::new().from_path(path)?;
        let mut inner = InnerState::new(Duration::from_secs(5), db);
        let header = reader.headers()?.to_owned();
        let mut flush_counter = 0u64;
        for record in reader.records() {
            match record {
                Ok(record) => match record.deserialize::<MetricCsvRow>(Some(&header)) {
                    Ok(r) => {
                        if let Err(e) =
                            inner.queue_metric(Duration::from_secs_f64(r.timestamp), r.key, r.value)
                        {
                            error!(
                                "Skipping record due to error recording metric into DB: {:?}",
                                e
                            );
                        }
                        flush_counter += 1;
                    }
                    Err(e) => {
                        error!("Skipping record due to error parsing CSV row: {:?}", e);
                    }
                },
                Err(e) => {
                    error!("Skipping record due to error reading CSV record: {:?}", e);
                }
            }
            if flush_counter % 200 == 0 {
                trace!("Flushing");
                inner.flush()?;
            }
        }
        inner.flush()?;
        Ok(())
    }
}
#[cfg(feature = "import_csv")]
#[derive(Deserialize)]
struct MetricCsvRow<'a> {
    #[allow(unused)]
    id: u64,
    timestamp: f64,
    key: &'a str,
    value: f64,
}
/// Metric model for CSV export
#[cfg(feature = "export_csv")]
#[derive(Queryable, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
struct JoinedMetric {
    /// Unique ID of sample
    pub id: i64,
    /// Timestamp of sample
    pub timestamp: f64,
    /// Key/name of sample
    pub key: String,
    /// Value of sample
    pub value: f64,
}
