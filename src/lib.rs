#![deny(missing_docs)]
//! # Metrics SQLite backend

#[macro_use]
extern crate diesel;
#[macro_use]
extern crate diesel_migrations;
#[macro_use]
extern crate log;

use diesel::prelude::*;
use diesel::{insert_into, sql_query};

use metrics::{GaugeValue, Key, SetRecorderError, Unit};

use diesel_migrations::{EmbeddedMigrations, MigrationHarness};
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
    /// Error if path given is invalid
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
    /// Attempted to query database but found no records
    #[error("Database has no metrics stored in it")]
    EmptyDatabase,
    /// Given metric key name wasn't found in the DB
    #[error("Metric key {0} not found in database")]
    KeyNotFound(String),
}
/// Metrics result type
pub type Result<T, E = MetricsError> = std::result::Result<T, E>;

mod metrics_db;
mod models;
mod recorder;
mod schema;

pub use metrics_db::{MetricsDb, Session};
pub use models::{Metric, MetricKey, NewMetric};

pub(crate) const MIGRATIONS: EmbeddedMigrations = embed_migrations!();

fn setup_db<P: AsRef<Path>>(path: P) -> Result<SqliteConnection> {
    let url = path
        .as_ref()
        .to_str()
        .ok_or(MetricsError::InvalidDatabasePath)?;
    let mut db = SqliteConnection::establish(url)?;
    db.run_pending_migrations(MIGRATIONS)
        .map_err(|e| MetricsError::MigrationError(e))?;

    Ok(db)
}
enum RegisterType {
    Counter,
    Gauge,
    Histogram,
}

enum Event {
    Stop,
    RegisterKey(RegisterType, Key, Option<Unit>, Option<&'static str>),
    IncrementCounter(Duration, Key, u64),
    UpdateGauge(Duration, Key, GaugeValue),
    UpdateHistogram(Duration, Key, f64),
    SetHousekeeping {
        retention_period: Option<Duration>,
        housekeeping_period: Option<Duration>,
        record_limit: Option<usize>,
    },
}

/// Exports metrics by storing them in a SQLite database at a periodic interval
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
            debug!("Flushing due to {}s timeout", self.flush_duration.as_secs());
            true
        } else {
            self.queue.len() >= FLUSH_QUEUE_LIMIT
        }
    }
    fn flush(&mut self) -> Result<(), diesel::result::Error> {
        use crate::schema::metrics::dsl::metrics;
        // trace!("Flushing {} records", self.queue.len());
        let db = &mut self.db;
        let queue = self.queue.drain(..);
        db.transaction::<_, diesel::result::Error, _>(|db| {
            for rec in queue {
                insert_into(metrics).values(&rec).execute(db)?;
            }
            Ok(())
        })?;
        self.last_flush = Instant::now();
        Ok(())
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
                    Ok(Event::RegisterKey(_key_type, key, unit, desc)) => {
                        info!("Registering {:?}", key);
                        if let Err(e) = MetricKey::create_or_update(
                            &key.name().to_string(),
                            unit,
                            desc,
                            &mut state.db,
                        ) {
                            error!("Failed to create key entry: {:?}", e);
                        }
                        (false, false)
                    }
                    Ok(Event::IncrementCounter(timestamp, key, value)) => {
                        let key_str = key.name().to_string();
                        let entry = state.counters.entry(key).or_insert(0);
                        let value = {
                            *entry += value;
                            *entry
                        };
                        if let Err(e) = state.queue_metric(timestamp, &key_str, value as _) {
                            error!("Error queueing metric: {:?}", e);
                        }

                        (state.should_flush(), false)
                    }
                    Ok(Event::UpdateGauge(timestamp, key, value)) => {
                        let key_str = key.name().to_string();
                        let entry = state.last_values.entry(key).or_insert(0.0);
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
                        if let Err(e) = state.queue_metric(timestamp, &key_str, value) {
                            error!("Error queueing metric: {:?}", e);
                        }
                        (state.should_flush(), false)
                    }
                    Ok(Event::UpdateHistogram(timestamp, key, value)) => {
                        let key_str = key.name().to_string();
                        if let Err(e) = state.queue_metric(timestamp, &key_str, value) {
                            error!("Error queueing metric: {:?}", e);
                        }

                        (state.should_flush(), false)
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        debug!("Flushing due to {}s timeout", flush_duration.as_secs());
                        (true, false)
                    }
                    Err(RecvTimeoutError::Disconnected) => {
                        warn!("SQLiteExporter channel disconnected, exiting worker");
                        (true, true)
                    }
                };
                if should_flush {
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
    /// Creates a new `SqliteExporter` that stores metrics in a SQLite database file.
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

    /// Sets optional periodic house keeping, None to disable (disabled by default)
    /// ## Notes
    /// Periodic house keeping can affect metric recording, causing some data to be dropped during house keeping.
    /// Record limit if set will cause anything over limit + 25% of limit to be removed
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
                        let query = format!("DELETE FROM metrics WHERE id IN (SELECT id FROM metrics ORDER BY timestamp ASC LIMIT {});", excess);
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
    pub fn install(self) -> Result<(), SetRecorderError> {
        metrics::set_boxed_recorder(Box::new(self))
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
            .into_iter()
            .map(|_| {
                thread::spawn(move || {
                    let start = Instant::now();
                    loop {
                        metrics::gauge!("rate", 1.0);
                        metrics::increment_counter!("hits");
                        metrics::histogram!("histogram", 5.0);
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
