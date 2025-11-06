#![deny(missing_docs)]
//! # Metrics SQLite backend

#[macro_use]
extern crate diesel;
#[macro_use]
extern crate diesel_migrations;
use tracing::{debug, error, info, trace, warn};

use diesel::prelude::*;
use diesel::{insert_into, sql_query};

use metrics::{GaugeValue, Key, KeyName, SetRecorderError, SharedString, Unit};

use diesel_migrations::{EmbeddedMigrations, MigrationHarness};
use std::sync::Arc;
use std::{
    collections::{HashMap, VecDeque},
    path::Path,
    sync::mpsc::{Receiver, RecvTimeoutError, SyncSender},
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime},
};
use thiserror::Error;

/// Max number of items allowed in worker's queue before flushing regardless of flush duration
const FLUSH_QUEUE_LIMIT: usize = 1000;
const BACKGROUND_CHANNEL_LIMIT: usize = 8000;
const SQLITE_DEFAULT_MAX_VARIABLES: usize = 999;
const METRIC_FIELDS_PER_ROW: usize = 3;
const INSERT_BATCH_SIZE: usize = SQLITE_DEFAULT_MAX_VARIABLES / METRIC_FIELDS_PER_ROW;

/// Error type for any db/vitals related errors
#[derive(Debug, Error)]
pub enum MetricsError {
    /// Error with database
    #[error("Database error: {0}")]
    DbConnectionError(#[from] ConnectionError),
    /// Error migrating database
    #[error("Migration error: {0}")]
    MigrationError(Box<dyn std::error::Error + Send + Sync>),
    /// Error querying metrics DB
    #[error("Error querying DB: {0}")]
    QueryError(#[from] diesel::result::Error),
    /// Error if the path given is invalid
    #[error("Invalid database path")]
    InvalidDatabasePath,
    /// IO Error with reader/writer
    #[cfg(feature = "csv")]
    #[error("IO Error: {0}")]
    IoError(#[from] std::io::Error),
    /// Error writing CSV
    #[cfg(feature = "csv")]
    #[error("CSV Error: {0}")]
    CsvError(#[from] csv::Error),
    /// Attempted to query the database but found no records
    #[error("Database has no metrics stored in it")]
    EmptyDatabase,
    /// Given metric key name wasn't found in the DB
    #[error("Metric key {0} not found in database")]
    KeyNotFound(String),
    /// Attempting to communicate with exporter but it's gone away
    #[error("Exporter task has been stopped or crashed")]
    ExporterUnavailable,
    /// Session derived from the signpost has zero duration
    #[error("Session for signpost `{0}` has zero duration")]
    ZeroLengthSession(String),
    /// No metrics available for the requested key inside the derived session
    #[error("No metrics recorded for `{0}` in requested session")]
    NoMetricsForKey(String),
}
/// Metrics result type
pub type Result<T, E = MetricsError> = std::result::Result<T, E>;

mod metrics_db;
mod models;
mod recorder;
mod schema;

use crate::metrics_db::query;
use crate::recorder::Handle;
pub use metrics_db::{MetricsDb, Session};
pub use models::{Metric, MetricKey, NewMetric};

pub(crate) const MIGRATIONS: EmbeddedMigrations = embed_migrations!();

fn setup_db<P: AsRef<Path>>(path: P) -> Result<SqliteConnection> {
    let url = path
        .as_ref()
        .to_str()
        .ok_or(MetricsError::InvalidDatabasePath)?;
    let mut db = SqliteConnection::establish(url)?;

    // Enable WAL mode for better concurrent access
    sql_query("PRAGMA journal_mode=WAL;").execute(&mut db)?;

    // Set busy timeout to 5 seconds to handle lock contention gracefully
    sql_query("PRAGMA busy_timeout = 5000;").execute(&mut db)?;

    db.run_pending_migrations(MIGRATIONS)
        .map_err(MetricsError::MigrationError)?;

    Ok(db)
}
enum RegisterType {
    Counter,
    Gauge,
    Histogram,
}

enum Event {
    Stop,
    DescribeKey(RegisterType, KeyName, Option<Unit>, SharedString),
    RegisterKey(RegisterType, Key, Arc<Handle>),
    IncrementCounter(Duration, Key, u64),
    AbsoluteCounter(Duration, Key, u64),
    UpdateGauge(Duration, Key, GaugeValue),
    UpdateHistogram(Duration, Key, f64),
    SetHousekeeping {
        retention_period: Option<Duration>,
        housekeeping_period: Option<Duration>,
        record_limit: Option<usize>,
    },
    RequestSummaryFromSignpost {
        signpost_key: String,
        keys: Vec<String>,
        tx: tokio::sync::oneshot::Sender<Result<HashMap<String, f64>>>,
    },
}

/// Handle for continued communication with sqlite exporter
pub struct SqliteExporterHandle {
    sender: SyncSender<Event>,
}
impl SqliteExporterHandle {
    /// Request average metrics from a signpost to latest from exporter's DB
    pub fn request_average_metrics(
        &self,
        from_signpost: &str,
        with_keys: &[&str],
    ) -> Result<HashMap<String, f64>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.sender
            .send(Event::RequestSummaryFromSignpost {
                signpost_key: from_signpost.to_string(),
                keys: with_keys.iter().map(|s| s.to_string()).collect(),
                tx,
            })
            .map_err(|_| MetricsError::ExporterUnavailable)?;
        match rx.blocking_recv() {
            Ok(metrics) => Ok(metrics?),
            Err(_) => Err(MetricsError::ExporterUnavailable),
        }
    }
}

/// Exports metrics by storing them in an SQLite database at a periodic interval
pub struct SqliteExporter {
    thread: Option<JoinHandle<()>>,
    sender: SyncSender<Event>,
}
struct InnerState {
    db: SqliteConnection,
    last_housekeeping: Instant,
    housekeeping: Option<Duration>,
    retention: Option<Duration>,
    record_limit: Option<usize>,
    flush_duration: Duration,
    last_flush: Instant,
    last_values: HashMap<Key, f64>,
    counters: HashMap<Key, u64>,
    key_ids: HashMap<String, i64>,
    queue: VecDeque<NewMetric>,
}
impl InnerState {
    fn new(flush_duration: Duration, db: SqliteConnection) -> Self {
        InnerState {
            db,
            last_housekeeping: Instant::now(),
            housekeeping: None,
            retention: None,
            record_limit: None,
            flush_duration,
            last_flush: Instant::now(),
            last_values: HashMap::new(),
            counters: HashMap::new(),
            key_ids: HashMap::new(),
            queue: VecDeque::with_capacity(FLUSH_QUEUE_LIMIT),
        }
    }
    fn set_housekeeping(
        &mut self,
        retention: Option<Duration>,
        housekeeping_duration: Option<Duration>,
        record_limit: Option<usize>,
    ) {
        self.retention = retention;
        self.housekeeping = housekeeping_duration;
        self.last_housekeeping = Instant::now();
        self.record_limit = record_limit;
    }
    fn should_housekeep(&self) -> bool {
        match self.housekeeping {
            Some(duration) => self.last_housekeeping.elapsed() > duration,
            None => false,
        }
    }
    fn housekeep(&mut self) -> Result<(), diesel::result::Error> {
        SqliteExporter::housekeeping(&mut self.db, self.retention, self.record_limit, false);
        self.last_housekeeping = Instant::now();
        Ok(())
    }
    fn should_flush(&self) -> bool {
        if self.last_flush.elapsed() > self.flush_duration {
            true
        } else if self.queue.len() >= FLUSH_QUEUE_LIMIT {
            debug!("Flushing due to queue size ({} items)", self.queue.len());
            true
        } else {
            false
        }
    }
    fn flush(&mut self) -> Result<(), diesel::result::Error> {
        use crate::schema::metrics::dsl::metrics;
        if self.queue.is_empty() {
            self.last_flush = Instant::now();
            return Ok(());
        }
        let drain_buffer: Vec<NewMetric> = self.queue.drain(..).collect();
        let db = &mut self.db;
        let transaction_result = db.transaction::<_, diesel::result::Error, _>(|db| {
            let chunk_size = INSERT_BATCH_SIZE.max(1);
            for chunk in drain_buffer.chunks(chunk_size) {
                insert_into(metrics).values(chunk).execute(db)?;
            }
            Ok(())
        });
        match transaction_result {
            Ok(()) => {
                self.last_flush = Instant::now();
                Ok(())
            }
            Err(e) => {
                self.queue.extend(drain_buffer);
                Err(e)
            }
        }
    }
    fn queue_metric(&mut self, timestamp: Duration, key: &str, value: f64) -> Result<()> {
        let metric_key_id = match self.key_ids.get(key) {
            Some(key) => *key,
            None => {
                debug!("Looking up {}", key);
                let key_id = MetricKey::key_by_name(key, &mut self.db)?.id;
                self.key_ids.insert(key.to_string(), key_id);
                key_id
            }
        };
        let metric = NewMetric {
            timestamp: timestamp.as_secs_f64(),
            metric_key_id,
            value: value as _,
        };
        self.queue.push_back(metric);
        Ok(())
    }

    // --- Summary/Average additions

    pub fn metrics_summary_for_signpost_and_keys(
        &mut self,
        signpost: String,
        metrics: Vec<String>,
    ) -> Result<HashMap<String, f64>> {
        query::metrics_summary_for_signpost_and_keys(&mut self.db, &signpost, metrics)
    }
}

fn run_worker(
    db: SqliteConnection,
    receiver: Receiver<Event>,
    flush_duration: Duration,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("metrics-sqlite: worker".to_string())
        .spawn(move || {
            let mut state = InnerState::new(flush_duration, db);
            info!("SQLite worker started");
            loop {
                // Check if we need to flush based on elapsed time
                let time_based_flush = state.last_flush.elapsed() >= flush_duration;

                let (should_flush, should_exit) = match receiver.recv_timeout(flush_duration) {
                    Ok(Event::Stop) => {
                        info!("Stopping SQLiteExporter worker, flushing & exiting");
                        (true, true)
                    }
                    Ok(Event::SetHousekeeping {
                        retention_period,
                        housekeeping_period,
                        record_limit,
                    }) => {
                        state.set_housekeeping(retention_period, housekeeping_period, record_limit);
                        (false, false)
                    }
                    Ok(Event::DescribeKey(_key_type, key, unit, desc)) => {
                        info!("Describing key {:?}", key);
                        if let Err(e) = MetricKey::create_or_update(
                            key.as_str(),
                            unit,
                            Some(desc.as_ref()),
                            &mut state.db,
                        ) {
                            error!("Failed to create key entry: {:?}", e);
                        }
                        (false, false)
                    }
                    Ok(Event::RegisterKey(_key_type, _key, _handle)) => {
                        // we currently don't do anything with register...
                        (false, false)
                    }
                    Ok(Event::IncrementCounter(timestamp, key, value)) => {
                        let key_name = key.name();
                        let entry = state.counters.entry(key.clone()).or_insert(0);
                        let value = {
                            *entry += value;
                            *entry
                        };
                        if let Err(e) = state.queue_metric(timestamp, key_name, value as _) {
                            error!("Error queueing metric: {:?}", e);
                        }

                        (state.should_flush(), false)
                    }
                    Ok(Event::AbsoluteCounter(timestamp, key, value)) => {
                        let key_name = key.name();
                        state.counters.insert(key.clone(), value);
                        if let Err(e) = state.queue_metric(timestamp, key_name, value as _) {
                            error!("Error queueing metric: {:?}", e);
                        }
                        (state.should_flush(), false)
                    }
                    Ok(Event::UpdateGauge(timestamp, key, value)) => {
                        let key_name = key.name();
                        let entry = state.last_values.entry(key.clone()).or_insert(0.0);
                        let value = match value {
                            GaugeValue::Absolute(v) => {
                                *entry = v;
                                *entry
                            }
                            GaugeValue::Increment(v) => {
                                *entry += v;
                                *entry
                            }
                            GaugeValue::Decrement(v) => {
                                *entry -= v;
                                *entry
                            }
                        };
                        if let Err(e) = state.queue_metric(timestamp, key_name, value) {
                            error!("Error queueing metric: {:?}", e);
                        }
                        (state.should_flush(), false)
                    }
                    Ok(Event::UpdateHistogram(timestamp, key, value)) => {
                        let key_name = key.name();
                        if let Err(e) = state.queue_metric(timestamp, key_name, value) {
                            error!("Error queueing metric: {:?}", e);
                        }

                        (state.should_flush(), false)
                    }
                    Ok(Event::RequestSummaryFromSignpost {
                        signpost_key,
                        keys,
                        tx,
                    }) => {
                        match state.flush() {
                            Ok(()) => match state
                                .metrics_summary_for_signpost_and_keys(signpost_key, keys)
                            {
                                Ok(metrics) => {
                                    if tx.send(Ok(metrics)).is_err() {
                                        error!(
                                            "Failed to respond with metrics results, discarding"
                                        );
                                    }
                                }
                                Err(e) => {
                                    if let Err(e) = tx.send(Err(e)) {
                                        error!(
                                            "Failed to respond with metrics error result, discarding: {e:?}"
                                        );
                                    }
                                }
                            },
                            Err(e) => {
                                let err = MetricsError::from(e);
                                error!(
                                    "Failed to flush pending metrics before summary request: {err:?}"
                                );
                                if let Err(send_err) = tx.send(Err(err)) {
                                    error!(
                                        "Failed to respond with metrics flush error result, discarding: {send_err:?}"
                                    );
                                }
                            }
                        }
                        (false, false)
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        (true, false)
                    }
                    Err(RecvTimeoutError::Disconnected) => {
                        warn!("SQLiteExporter channel disconnected, exiting worker");
                        (true, true)
                    }
                };

                // Flush if time-based flush is triggered OR if event-based flush is triggered
                if time_based_flush || should_flush {
                    if time_based_flush {
                        debug!("Flushing due to elapsed time ({}s)", flush_duration.as_secs());
                    }
                    if let Err(e) = state.flush() {
                        error!("Error flushing metrics: {}", e);
                    }
                }
                if state.should_housekeep() {
                    if let Err(e) = state.housekeep() {
                        error!("Failed running house keeping: {:?}", e);
                    }
                }
                if should_exit {
                    break;
                }
            }
        })
        .unwrap()
}

impl SqliteExporter {
    /// Creates a new `SqliteExporter` that stores metrics in an SQLite database file.
    ///
    /// `flush_interval` specifies how often metrics are flushed to SQLite/disk
    ///
    /// `keep_duration` specifies how long data is kept before deleting, performed new()
    pub fn new<P: AsRef<Path>>(
        flush_interval: Duration,
        keep_duration: Option<Duration>,
        path: P,
    ) -> Result<Self> {
        let mut db = setup_db(path)?;
        Self::housekeeping(&mut db, keep_duration, None, true);
        let (sender, receiver) = std::sync::mpsc::sync_channel(BACKGROUND_CHANNEL_LIMIT);
        let thread = run_worker(db, receiver, flush_interval);
        let exporter = SqliteExporter {
            thread: Some(thread),
            sender,
        };
        Ok(exporter)
    }

    /// Sets optional periodic housekeeping, None to disable (disabled by default)
    /// ## Notes
    /// Periodic housekeeping can affect metric recording, causing some data to be dropped during housekeeping.
    /// Record limit if set will cause anything over limit + 25% of the limit to be removed
    pub fn set_periodic_housekeeping(
        &self,
        periodic_duration: Option<Duration>,
        retention: Option<Duration>,
        record_limit: Option<usize>,
    ) {
        if let Err(e) = self.sender.send(Event::SetHousekeeping {
            retention_period: retention,
            housekeeping_period: periodic_duration,
            record_limit,
        }) {
            error!("Failed to set house keeping settings: {:?}", e);
        }
    }

    /// Run housekeeping.
    ///
    /// Does nothing if None was given for keep_duration in `new()`
    fn housekeeping(
        db: &mut SqliteConnection,
        keep_duration: Option<Duration>,
        record_limit: Option<usize>,
        vacuum: bool,
    ) {
        use crate::schema::metrics::dsl::*;
        use diesel::dsl::count;
        if let Some(keep_duration) = keep_duration {
            match SystemTime::UNIX_EPOCH.elapsed() {
                Ok(now) => {
                    let cutoff = now - keep_duration;
                    trace!("Deleting data {}s old", keep_duration.as_secs());
                    if let Err(e) =
                        diesel::delete(metrics.filter(timestamp.le(cutoff.as_secs_f64())))
                            .execute(db)
                    {
                        error!("Failed to remove old metrics data: {}", e);
                    }
                    if vacuum {
                        if let Err(e) = sql_query("VACUUM").execute(db) {
                            error!("Failed to vacuum SQLite DB: {:?}", e);
                        }
                    }
                }
                Err(e) => {
                    error!(
                        "System time error, skipping metrics-sqlite housekeeping: {}",
                        e
                    );
                }
            }
        }
        if let Some(record_limit) = record_limit {
            trace!("Checking for records over {} limit", record_limit);
            match metrics.select(count(id)).first::<i64>(db) {
                Ok(records) => {
                    let records = records as usize;
                    if records > record_limit {
                        let excess = records - record_limit + (record_limit / 4); // delete excess + 25% of limit
                        trace!(
                            "Exceeded limit! {} > {}, deleting {} oldest",
                            records,
                            record_limit,
                            excess
                        );
                        let query = format!("DELETE FROM metrics WHERE id IN (SELECT id FROM metrics ORDER BY timestamp ASC LIMIT {excess});");
                        if let Err(e) = sql_query(query).execute(db) {
                            error!("Failed to delete excessive records: {:?}", e);
                        }
                    }
                }
                Err(e) => {
                    error!("Failed to get record count: {:?}", e);
                }
            }
        }
    }

    /// Install recorder as `metrics` crate's Recorder
    pub fn install(self) -> Result<SqliteExporterHandle, SetRecorderError<Self>> {
        let handle = SqliteExporterHandle {
            sender: self.sender.clone(),
        };
        metrics::set_global_recorder(self)?;
        Ok(handle)
    }
}
impl Drop for SqliteExporter {
    fn drop(&mut self) {
        let _ = self.sender.send(Event::Stop);
        let _ = self.thread.take().unwrap().join();
    }
}

#[cfg(test)]
mod tests {
    use crate::SqliteExporter;
    use std::time::{Duration, Instant};

    #[test]
    fn test_threading() {
        use std::thread;
        SqliteExporter::new(Duration::from_millis(500), None, "metrics.db")
            .unwrap()
            .install()
            .unwrap();
        let joins: Vec<thread::JoinHandle<()>> = (0..5)
            .map(|_| {
                thread::spawn(move || {
                    let start = Instant::now();
                    loop {
                        metrics::gauge!("rate").set(1.0);
                        metrics::counter!("hits").increment(1);
                        metrics::histogram!("histogram").record(5.0);
                        if start.elapsed().as_secs() >= 5 {
                            break;
                        }
                    }
                })
            })
            .collect();
        for j in joins {
            j.join().unwrap();
        }
    }
}
