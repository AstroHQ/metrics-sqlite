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
use std::sync::Mutex;
use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
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
/// Hard cap on metrics buffered in memory by the worker. If flushing to SQLite
/// keeps failing, the oldest metrics beyond this limit are dropped so a broken
/// database can never grow the queue without bound.
const QUEUE_HARD_LIMIT: usize = 100_000;
/// Number of consecutive failed flushes after which the worker rebuilds its
/// database connection, even if the error didn't look connection-fatal.
const RECONNECT_AFTER_FAILURES: u64 = 3;
/// Minimum delay between worker database reconnection attempts.
const RECONNECT_BACKOFF: Duration = Duration::from_secs(30);
/// Minimum delay between repeated error log lines of the same kind.
const ERROR_LOG_INTERVAL: Duration = Duration::from_secs(60);

/// Rate limiter for repetitive error logs. Emits at most one log per interval
/// and reports how many were suppressed in between.
struct LogThrottle {
    interval: Duration,
    last_logged: Option<Instant>,
    suppressed: u64,
}
impl LogThrottle {
    const fn new(interval: Duration) -> Self {
        LogThrottle {
            interval,
            last_logged: None,
            suppressed: 0,
        }
    }
    /// Returns `Some(suppressed_since_last_log)` when a log line should be
    /// emitted now, or `None` when it should be suppressed.
    fn allow(&mut self) -> Option<u64> {
        let now = Instant::now();
        let due = match self.last_logged {
            Some(last) => now.duration_since(last) >= self.interval,
            None => true,
        };
        if due {
            self.last_logged = Some(now);
            Some(std::mem::take(&mut self.suppressed))
        } else {
            self.suppressed += 1;
            None
        }
    }

    /// Invokes `emit` with the count of previously-suppressed lines, but only
    /// if enough time has passed since the last emission. Otherwise the call
    /// is silently dropped and the suppression counter is incremented.
    fn log_if_due(&mut self, emit: impl FnOnce(u64)) {
        if let Some(suppressed) = self.allow() {
            emit(suppressed);
        }
    }
}

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

impl MetricsError {
    /// Check if this error indicates a malformed/corrupt database
    fn is_malformed_db(&self) -> bool {
        self.to_string().contains("malformed")
    }
}

/// Metrics result type
pub type Result<T, E = MetricsError> = std::result::Result<T, E>;

mod metrics_db;
mod models;
mod recorder;
mod schema;

use crate::metrics_db::query;
pub use metrics_db::{MetricsDb, Session};
pub use models::{Metric, MetricKey, NewMetric};

pub(crate) const MIGRATIONS: EmbeddedMigrations = embed_migrations!();

#[derive(QueryableByName)]
struct PragmaCheckResult {
    #[diesel(sql_type = diesel::sql_types::Text)]
    #[diesel(column_name = quick_check)]
    result: String,
}

/// Remove database file and its WAL/SHM sidecar files
fn remove_db_files(path: &Path) {
    let db_path = PathBuf::from(path);
    for suffix in &["", "-wal", "-shm"] {
        let mut file_path = db_path.clone().into_os_string();
        file_path.push(suffix);
        let file_path = PathBuf::from(file_path);
        if file_path.exists() {
            if let Err(e) = std::fs::remove_file(&file_path) {
                error!("Failed to remove {}: {}", file_path.display(), e);
            } else {
                info!("Removed corrupt database file: {}", file_path.display());
            }
        }
    }
}

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

    // Check for corruption that may not surface until queries run
    let check: String = sql_query("PRAGMA quick_check;")
        .get_result::<PragmaCheckResult>(&mut db)?
        .result;
    if check != "ok" {
        return Err(MetricsError::QueryError(
            diesel::result::Error::DatabaseError(
                diesel::result::DatabaseErrorKind::Unknown,
                Box::new(format!("database disk image is malformed: {check}")),
            ),
        ));
    }

    Ok(db)
}

/// Like `setup_db`, but if the database is malformed, removes it and retries
/// once. The boolean in the success result is `true` when a reset actually
/// happened, so callers can drop any cached state that referenced the old
/// database (notably `metric_keys` row ids).
fn setup_db_or_reset<P: AsRef<Path>>(path: P) -> Result<(SqliteConnection, bool)> {
    let path = path.as_ref();
    match setup_db(path) {
        Ok(db) => Ok((db, false)),
        Err(err) if err.is_malformed_db() => {
            warn!(
                "Database is malformed, removing and recreating: {}",
                path.display()
            );
            remove_db_files(path);
            setup_db(path).map(|db| (db, true))
        }
        Err(err) => Err(err),
    }
}
enum RegisterType {
    Counter,
    Gauge,
    Histogram,
}

enum Event {
    Stop,
    DescribeKey(RegisterType, KeyName, Option<Unit>, SharedString),
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
    send_error_throttle: Mutex<LogThrottle>,
}
struct InnerState {
    db: SqliteConnection,
    db_path: PathBuf,
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
    consecutive_flush_failures: u64,
    last_reconnect: Option<Instant>,
}
impl InnerState {
    fn new(flush_duration: Duration, db: SqliteConnection, db_path: PathBuf) -> Self {
        InnerState {
            db,
            db_path,
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
            consecutive_flush_failures: 0,
            last_reconnect: None,
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
        if self.queue.is_empty() {
            self.last_flush = Instant::now();
            return Ok(());
        }
        // Operate on the queue in-place: pass the deque's two backing slices
        // directly to the insert. On failure we leave the queue alone, so a
        // broken database stays at zero memcpy cost per attempt — the cascade
        // that filled the channel in the original incident is what made each
        // failed flush O(queue_size) by draining and re-extending.
        let (front, back) = self.queue.as_slices();
        match Self::insert_metrics(&mut self.db, [front, back]) {
            Ok(()) => {
                self.queue.clear();
                self.last_flush = Instant::now();
                self.consecutive_flush_failures = 0;
                Ok(())
            }
            Err(e) => {
                self.consecutive_flush_failures += 1;
                // Queue is intact; just cap memory.
                self.enforce_queue_cap();
                // A broken transaction manager never heals on its own, so the
                // connection has to be rebuilt; also rebuild after repeated
                // failures of any kind as a backstop.
                if Self::is_connection_fatal(&e)
                    || self.consecutive_flush_failures >= RECONNECT_AFTER_FAILURES
                {
                    self.reconnect();
                }
                Err(e)
            }
        }
    }

    /// Inserts every metric in a single transaction, batched to stay under
    /// SQLite's bound-variable limit. Accepts multiple slices so a `VecDeque`
    /// can be inserted in-place without copying into a contiguous buffer.
    fn insert_metrics<'a, S>(
        db: &mut SqliteConnection,
        slabs: S,
    ) -> Result<(), diesel::result::Error>
    where
        S: IntoIterator<Item = &'a [NewMetric]>,
    {
        use crate::schema::metrics::dsl::metrics;
        db.transaction::<_, diesel::result::Error, _>(|db| {
            let chunk_size = INSERT_BATCH_SIZE.max(1);
            for slab in slabs {
                for chunk in slab.chunks(chunk_size) {
                    insert_into(metrics).values(chunk).execute(db)?;
                }
            }
            Ok(())
        })
    }

    /// Drops the oldest queued metrics if the queue has grown past its hard
    /// limit, so a database that stays unreachable can't exhaust memory.
    fn enforce_queue_cap(&mut self) {
        if self.queue.len() > QUEUE_HARD_LIMIT {
            let overflow = self.queue.len() - QUEUE_HARD_LIMIT;
            self.queue.drain(..overflow);
            warn!(
                "metrics-sqlite queue exceeded {} items while flushing kept failing, dropped {} oldest metrics",
                QUEUE_HARD_LIMIT, overflow
            );
        }
    }

    /// True for errors that leave the connection permanently unusable, where
    /// the only recovery is to rebuild it.
    fn is_connection_fatal(e: &diesel::result::Error) -> bool {
        matches!(e, diesel::result::Error::BrokenTransactionManager)
    }

    /// Rebuilds the SQLite connection after a fatal error, subject to a backoff
    /// so a permanently broken database can't cause a reconnect storm.
    fn reconnect(&mut self) {
        if let Some(last) = self.last_reconnect {
            if last.elapsed() < RECONNECT_BACKOFF {
                return;
            }
        }
        self.last_reconnect = Some(Instant::now());
        warn!(
            "metrics-sqlite database connection is broken, reconnecting to {}",
            self.db_path.display()
        );
        match setup_db_or_reset(&self.db_path) {
            Ok((db, was_reset)) => {
                self.db = db;
                // Cached key ids may be stale if the database was recreated.
                self.key_ids.clear();
                self.consecutive_flush_failures = 0;
                if was_reset {
                    // The database was malformed and recreated, so any rows we
                    // had queued reference `metric_key_id` values from the old
                    // `metric_keys` table. Inserting them now would orphan or
                    // misattribute them in the join, so drop the queue.
                    let dropped = self.queue.len();
                    if dropped > 0 {
                        warn!(
                            "metrics-sqlite database was recreated; dropping {dropped} queued metrics with stale key ids"
                        );
                        self.queue.clear();
                    }
                }
                info!("metrics-sqlite database connection re-established");
            }
            Err(e) => {
                error!("metrics-sqlite failed to reconnect to database: {:?}", e);
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
    db_path: PathBuf,
    receiver: Receiver<Event>,
    flush_duration: Duration,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("metrics-sqlite: worker".to_string())
        .spawn(move || {
            let mut state = InnerState::new(flush_duration, db, db_path);
            let mut flush_error_throttle = LogThrottle::new(ERROR_LOG_INTERVAL);
            let mut queue_error_throttle = LogThrottle::new(ERROR_LOG_INTERVAL);
            info!("SQLite worker started");
            loop {
                // Check if we need to flush based on elapsed time
                let time_based_flush = state.last_flush.elapsed() >= flush_duration;

                let mut should_flush = false;
                let mut should_exit = false;
                match receiver.recv_timeout(flush_duration) {
                    Ok(Event::Stop) => {
                        info!("Stopping SQLiteExporter worker, flushing & exiting");
                        should_flush = true;
                        should_exit = true;
                    }
                    Ok(Event::SetHousekeeping {
                        retention_period,
                        housekeeping_period,
                        record_limit,
                    }) => {
                        state.set_housekeeping(retention_period, housekeeping_period, record_limit);
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
                    }
                    Ok(Event::IncrementCounter(timestamp, key, value)) => {
                        let key_name = key.name();
                        let entry = state.counters.entry(key.clone()).or_insert(0);
                        let value = {
                            *entry += value;
                            *entry
                        };
                        if let Err(e) = state.queue_metric(timestamp, key_name, value as _) {
                            queue_error_throttle.log_if_due(|suppressed| {
                                if suppressed > 0 {
                                    error!(
                                        "Error queueing metric: {:?} ({} similar errors suppressed in the last {}s)",
                                        e,
                                        suppressed,
                                        ERROR_LOG_INTERVAL.as_secs()
                                    );
                                } else {
                                    error!("Error queueing metric: {:?}", e);
                                }
                            });
                        }
                        should_flush = state.should_flush();
                    }
                    Ok(Event::AbsoluteCounter(timestamp, key, value)) => {
                        let key_name = key.name();
                        state.counters.insert(key.clone(), value);
                        if let Err(e) = state.queue_metric(timestamp, key_name, value as _) {
                            queue_error_throttle.log_if_due(|suppressed| {
                                if suppressed > 0 {
                                    error!(
                                        "Error queueing metric: {:?} ({} similar errors suppressed in the last {}s)",
                                        e,
                                        suppressed,
                                        ERROR_LOG_INTERVAL.as_secs()
                                    );
                                } else {
                                    error!("Error queueing metric: {:?}", e);
                                }
                            });
                        }
                        should_flush = state.should_flush();
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
                            queue_error_throttle.log_if_due(|suppressed| {
                                if suppressed > 0 {
                                    error!(
                                        "Error queueing metric: {:?} ({} similar errors suppressed in the last {}s)",
                                        e,
                                        suppressed,
                                        ERROR_LOG_INTERVAL.as_secs()
                                    );
                                } else {
                                    error!("Error queueing metric: {:?}", e);
                                }
                            });
                        }
                        should_flush = state.should_flush();
                    }
                    Ok(Event::UpdateHistogram(timestamp, key, value)) => {
                        let key_name = key.name();
                        if let Err(e) = state.queue_metric(timestamp, key_name, value) {
                            queue_error_throttle.log_if_due(|suppressed| {
                                if suppressed > 0 {
                                    error!(
                                        "Error queueing metric: {:?} ({} similar errors suppressed in the last {}s)",
                                        e,
                                        suppressed,
                                        ERROR_LOG_INTERVAL.as_secs()
                                    );
                                } else {
                                    error!("Error queueing metric: {:?}", e);
                                }
                            });
                        }
                        should_flush = state.should_flush();
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
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        should_flush = true;
                    }
                    Err(RecvTimeoutError::Disconnected) => {
                        warn!("SQLiteExporter channel disconnected, exiting worker");
                        should_flush = true;
                        should_exit = true;
                    }
                }

                // Flush if time-based flush is triggered OR if event-based flush is triggered
                if time_based_flush || should_flush {
                    if time_based_flush {
                        debug!("Flushing due to elapsed time ({}s)", flush_duration.as_secs());
                    }
                    if let Err(e) = state.flush() {
                        if let Some(suppressed) = flush_error_throttle.allow() {
                            if suppressed > 0 {
                                error!(
                                    "Error flushing metrics: {} ({} similar errors suppressed in the last {}s)",
                                    e,
                                    suppressed,
                                    ERROR_LOG_INTERVAL.as_secs()
                                );
                            } else {
                                error!("Error flushing metrics: {}", e);
                            }
                        }
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
        let path = path.as_ref().to_path_buf();
        let (mut db, _was_reset) = setup_db_or_reset(&path)?;
        Self::housekeeping(&mut db, keep_duration, None, true);
        let (sender, receiver) = std::sync::mpsc::sync_channel(BACKGROUND_CHANNEL_LIMIT);
        let thread = run_worker(db, path, receiver, flush_interval);
        let exporter = SqliteExporter {
            thread: Some(thread),
            sender,
            send_error_throttle: Mutex::new(LogThrottle::new(ERROR_LOG_INTERVAL)),
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
                            records, record_limit, excess
                        );
                        let query = format!(
                            "DELETE FROM metrics WHERE id IN (SELECT id FROM metrics ORDER BY timestamp ASC LIMIT {excess});"
                        );
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

    /// Logs a failure to hand an event to the worker, rate-limited so that a
    /// full channel (sustained backpressure) can't flood the logs.
    fn log_send_failure(&self, context: &str, err: &dyn std::fmt::Debug) {
        if let Ok(mut throttle) = self.send_error_throttle.lock() {
            if let Some(suppressed) = throttle.allow() {
                if suppressed > 0 {
                    error!(
                        "Error sending metric {} to SQLite worker: {:?} ({} similar errors suppressed in the last {}s)",
                        context,
                        err,
                        suppressed,
                        ERROR_LOG_INTERVAL.as_secs()
                    );
                } else {
                    error!(
                        "Error sending metric {} to SQLite worker: {:?}",
                        context, err
                    );
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
    use crate::{
        InnerState, LogThrottle, NewMetric, QUEUE_HARD_LIMIT, SqliteExporter, setup_db_or_reset,
    };
    use std::time::{Duration, Instant};

    fn test_state() -> (InnerState, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metrics.db");
        let (db, _was_reset) = setup_db_or_reset(&path).unwrap();
        (InnerState::new(Duration::from_secs(5), db, path), dir)
    }

    #[test]
    fn enforce_queue_cap_drops_oldest_when_over_limit() {
        let (mut state, _dir) = test_state();
        // Queue more than the hard limit; values are tagged 0..N so we can tell
        // which survived.
        let total = QUEUE_HARD_LIMIT + 250;
        for i in 0..total {
            state.queue.push_back(NewMetric {
                timestamp: 0.0,
                metric_key_id: 1,
                value: i as f64,
            });
        }
        state.enforce_queue_cap();
        // Queue is clamped to the limit and it's the oldest that were dropped.
        assert_eq!(state.queue.len(), QUEUE_HARD_LIMIT);
        assert_eq!(state.queue.front().unwrap().value, 250.0);
        assert_eq!(state.queue.back().unwrap().value, (total - 1) as f64);
        // A queue at or under the limit is left untouched.
        state.enforce_queue_cap();
        assert_eq!(state.queue.len(), QUEUE_HARD_LIMIT);
    }

    #[test]
    fn failed_flush_leaves_queue_intact() {
        use diesel::connection::SimpleConnection;
        let (mut state, _dir) = test_state();
        // Drop the metrics table so every insert in the next flush fails. We
        // do this from the test, not via a production API, to keep the test
        // hook out of the public crate surface.
        state
            .db
            .batch_execute("DROP TABLE metrics")
            .expect("setup: drop metrics table");
        for i in 0..5_000 {
            state.queue.push_back(NewMetric {
                timestamp: 0.0,
                metric_key_id: 1,
                value: i as f64,
            });
        }
        let before = state.queue.len();
        assert!(state.flush().is_err(), "flush should fail");
        // The whole point of the in-place flush: the queue is intact.
        assert_eq!(state.queue.len(), before);
        assert_eq!(state.queue.front().unwrap().value, 0.0);
        assert_eq!(state.queue.back().unwrap().value, 4_999.0);
        assert_eq!(state.consecutive_flush_failures, 1);
    }

    #[test]
    fn reconnect_drops_queue_when_database_is_recreated() {
        use crate::setup_db;
        use diesel::connection::SimpleConnection;
        use std::io::{Seek, SeekFrom, Write};
        let (mut state, _dir) = test_state();
        for i in 0..100 {
            state.queue.push_back(NewMetric {
                timestamp: 0.0,
                metric_key_id: 1,
                value: i as f64,
            });
        }
        // Force pending writes into the main DB file and truncate the WAL so
        // the corruption below isn't masked by WAL contents.
        state
            .db
            .batch_execute("PRAGMA wal_checkpoint(TRUNCATE)")
            .expect("setup: wal checkpoint");
        // Release the real DB's file handle before we corrupt and reset. On
        // Windows the malformed-reset path can't `DeleteFile` a file that
        // still has an open handle, so without this swap the reset silently
        // fails and the queue never gets cleared (unlike POSIX, where unlink
        // works through open handles). The in-memory placeholder just keeps
        // `InnerState` in a valid shape until `reconnect` replaces it.
        state.db = setup_db(":memory:").expect("setup: in-memory placeholder");
        // Keep a valid SQLite header (first 100 bytes) but overwrite a later
        // page so the next `PRAGMA quick_check` trips the malformed path. We
        // overwrite a long enough region to clobber whichever page holds the
        // schema, regardless of page size.
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .open(&state.db_path)
            .expect("setup: open db file");
        f.seek(SeekFrom::Start(100))
            .expect("setup: seek past header");
        f.write_all(&[0xffu8; 16 * 1024])
            .expect("setup: write garbage pages");
        f.sync_all().expect("setup: sync garbage to disk");
        drop(f);

        state.reconnect();

        // Reset happened: the rebuilt DB has fresh `metric_keys` ids, so the
        // queued rows must be dropped to avoid orphaning them.
        assert!(
            state.queue.is_empty(),
            "queue should be cleared after reset"
        );
        assert_eq!(state.consecutive_flush_failures, 0);
        // And the new connection is usable.
        assert!(state.flush().is_ok());
    }

    #[test]
    fn reconnect_rebuilds_connection_with_backoff() {
        let (mut state, _dir) = test_state();
        state.consecutive_flush_failures = 5;
        state.reconnect();
        // A successful reconnect clears the failure counter and records when it
        // happened.
        assert_eq!(state.consecutive_flush_failures, 0);
        let first_attempt = state.last_reconnect.expect("reconnect should run");
        // The rebuilt connection is usable.
        assert!(state.flush().is_ok());

        // A second reconnect right away is suppressed by the backoff, so it
        // neither re-attempts nor touches state.
        state.consecutive_flush_failures = 5;
        state.reconnect();
        assert_eq!(state.consecutive_flush_failures, 5);
        assert_eq!(state.last_reconnect, Some(first_attempt));
    }

    #[test]
    fn log_throttle_suppresses_and_reports_count() {
        let mut throttle = LogThrottle::new(Duration::from_millis(50));
        // First call is always allowed, with nothing suppressed yet.
        assert_eq!(throttle.allow(), Some(0));
        // Rapid follow-ups are suppressed.
        for _ in 0..7 {
            assert_eq!(throttle.allow(), None);
        }
        // Once the interval elapses, the next call is allowed and reports how
        // many were suppressed in between.
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(throttle.allow(), Some(7));
        // Counter resets after reporting.
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(throttle.allow(), Some(0));
    }

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
