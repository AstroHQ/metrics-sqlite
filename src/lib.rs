#![deny(missing_docs)]
//! # Metrics SQLite backend

#[macro_use]
extern crate diesel;
#[macro_use]
extern crate diesel_migrations;
#[macro_use]
extern crate log;

use diesel::insert_into;
use diesel::prelude::*;

use metrics::{GaugeValue, Key, SetRecorderError};

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

/// Error type for any db/vitals related errors
#[derive(Debug, Error)]
pub enum MetricsError {
    /// Error with database
    #[error("Database error: {0}")]
    DbConnectionError(#[from] diesel::ConnectionError),
    /// Error migrating database
    #[error("Migration error: {0}")]
    MigrationError(#[from] diesel_migrations::RunMigrationsError),
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
}
/// Metrics result type
pub type Result<T, E = MetricsError> = std::result::Result<T, E>;

mod metrics_db;
mod models;
mod recorder;
mod schema;

pub use metrics_db::{MetricsDb, Session};
pub use models::{Metric, NewMetric};

embed_migrations!("migrations");

fn setup_db<P: AsRef<Path>>(path: P) -> Result<SqliteConnection> {
    let url = path
        .as_ref()
        .to_str()
        .ok_or(MetricsError::InvalidDatabasePath)?;
    let db = SqliteConnection::establish(url)?;
    embedded_migrations::run(&db)?;

    Ok(db)
}

enum Event {
    Stop,
    // Flush,
    IncrementCounter(Duration, Key, u64),
    UpdateGauge(Duration, Key, GaugeValue),
    UpdateHistogram(Duration, Key, f64),
}

/// Exports metrics by storing them in a SQLite database at a periodic interval
pub struct SqliteExporter {
    thread: Option<JoinHandle<()>>,
    sender: SyncSender<Event>,
}
struct InnerState {
    flush_duration: Duration,
    last_flush: Instant,
    last_values: HashMap<Key, f64>,
    counters: HashMap<Key, u64>,
    queue: VecDeque<NewMetric>,
}
impl InnerState {
    fn new(flush_duration: Duration) -> Self {
        InnerState {
            flush_duration,
            last_flush: Instant::now(),
            last_values: HashMap::new(),
            counters: HashMap::new(),
            queue: VecDeque::with_capacity(FLUSH_QUEUE_LIMIT),
        }
    }
    fn should_flush(&mut self) -> bool {
        if self.last_flush.elapsed() > self.flush_duration {
            debug!("Flushing due to {}s timeout", self.flush_duration.as_secs());
            true
        } else {
            self.queue.len() >= FLUSH_QUEUE_LIMIT
        }
    }
    fn flush(&mut self, db: &SqliteConnection) -> Result<(), diesel::result::Error> {
        use crate::schema::metrics::dsl::metrics;
        trace!("Flushing {} records", self.queue.len());
        db.transaction::<_, diesel::result::Error, _>(|| {
            for rec in self.queue.drain(..) {
                insert_into(metrics).values(&rec).execute(db)?;
            }
            Ok(())
        })?;
        self.last_flush = Instant::now();
        Ok(())
    }
}

fn run_worker(
    db: SqliteConnection,
    receiver: Receiver<Event>,
    flush_duration: Duration,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut state = InnerState::new(flush_duration);

        loop {
            let (should_flush, should_exit) = match receiver.recv_timeout(flush_duration) {
                Ok(Event::Stop) => {
                    info!("Stopping SQLiteExporter worker, flushing & exiting");
                    (true, true)
                }
                // Ok(Event::Flush) => (true, false),
                Ok(Event::IncrementCounter(timestamp, key, value)) => {
                    let key_str = key.name().to_string();
                    let entry = state.counters.entry(key).or_insert(0);
                    let value = {
                        *entry = *entry + value;
                        *entry
                    };
                    let metric = NewMetric {
                        timestamp: timestamp.as_secs_f64(),
                        key: key_str,
                        value: value as _,
                    };
                    state.queue.push_back(metric);
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
                            *entry = *entry + v;
                            *entry
                        }
                        GaugeValue::Decrement(v) => {
                            *entry = *entry - v;
                            *entry
                        }
                    };
                    let metric = NewMetric {
                        timestamp: timestamp.as_secs_f64(),
                        key: key_str,
                        value: value as _,
                    };
                    state.queue.push_back(metric);
                    (state.should_flush(), false)
                }
                Ok(Event::UpdateHistogram(timestamp, key, value)) => {
                    let key_str = key.name().to_string();
                    let metric = NewMetric {
                        timestamp: timestamp.as_secs_f64(),
                        key: key_str,
                        value,
                    };
                    state.queue.push_back(metric);
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
                if let Err(e) = state.flush(&db) {
                    error!("Error flushing metrics: {}", e);
                }
            }
            if should_exit {
                break;
            }
        }
    })
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
        let db = setup_db(path)?;
        Self::housekeeping(&db, keep_duration);
        let (sender, receiver) = std::sync::mpsc::sync_channel(1000);
        let thread = run_worker(db, receiver, flush_interval);
        let exporter = SqliteExporter {
            thread: Some(thread),
            sender,
        };
        Ok(exporter)
    }

    /// Run housekeeping.
    ///
    /// Does nothing if None was given for keep_duration in `new()`
    fn housekeeping(db: &SqliteConnection, keep_duration: Option<Duration>) {
        use crate::schema::metrics::dsl::*;
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
                }
                Err(e) => {
                    error!(
                        "System time error, skipping metrics-sqlite housekeeping: {}",
                        e
                    );
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
