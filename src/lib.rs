#[macro_use]
extern crate diesel;
#[macro_use]
extern crate diesel_migrations;

use diesel::insert_into;
use diesel::prelude::*;

use hdrhistogram::Histogram;
use metrics_core::{Builder, Drain, Key, Label, Observe, Observer};
use metrics_util::{parse_quantiles, Quantile};
use std::collections::HashMap;
use std::path::Path;
use std::{thread, time::Duration};
use thiserror::Error;

/// Error type for any db/vitals related errors
#[derive(Debug, Error)]
pub enum VitalsError {
    /// Error with database
    #[error("Database error: {0}")]
    DbConnectionError(#[from] diesel::ConnectionError),
    /// Error migrating database
    #[error("Migration error: {0}")]
    MigrationError(#[from] diesel_migrations::RunMigrationsError),
    /// Error querying vitals DB
    #[error("Error querying DB: {0}")]
    QueryError(#[from] diesel::result::Error),
    /// Error if path given is invalid
    #[error("Invalid database path")]
    InvalidDatabasePath,
}
/// Vitals result type
pub type Result<T, E = VitalsError> = std::result::Result<T, E>;

mod models;
mod schema;
pub use models::NewMetric;

embed_migrations!("migrations");

fn setup_db<P: AsRef<Path>>(path: P) -> Result<SqliteConnection> {
    let url = path
        .as_ref()
        .to_str()
        .ok_or(VitalsError::InvalidDatabasePath)?;
    let db = SqliteConnection::establish(url)?;
    embedded_migrations::run(&db)?;

    Ok(db)
}

/// Builder for [`SqliteObserver`].
pub struct SqliteBuilder {
    quantiles: Vec<Quantile>,
}

impl SqliteBuilder {
    /// Creates a new [`SqliteBuilder`] with default values.
    pub fn new() -> Self {
        let quantiles = parse_quantiles(&[0.0, 0.5, 0.9, 0.95, 0.99, 0.999, 1.0]);

        Self { quantiles }
    }

    /// Sets the quantiles to use when rendering histograms.
    ///
    /// Quantiles represent a scale of 0 to 1, where percentiles represent a scale of 1 to 100, so
    /// a quantile of 0.99 is the 99th percentile, and a quantile of 0.99 is the 99.9th percentile.
    ///
    /// By default, the quantiles will be set to: 0.0, 0.5, 0.9, 0.95, 0.99, 0.999, and 1.0.
    pub fn set_quantiles(mut self, quantiles: &[f64]) -> Self {
        self.quantiles = parse_quantiles(quantiles);
        self
    }
}

impl Builder for SqliteBuilder {
    type Output = SqliteObserver;

    fn build(&self) -> Self::Output {
        SqliteObserver {
            quantiles: self.quantiles.clone(),
            contents: HashMap::default(),
            histos: HashMap::new(),
        }
    }
}

impl Default for SqliteBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Observes metrics in Sqlite (Diesel models) format.
pub struct SqliteObserver {
    pub(crate) quantiles: Vec<Quantile>,
    // pub(crate) pretty: bool,
    // pub(crate) tree: MetricsTree,
    pub(crate) contents: HashMap<String, i64>,
    pub(crate) histos: HashMap<Key, Histogram<u64>>,
}

impl Observer for SqliteObserver {
    fn observe_counter(&mut self, key: Key, value: u64) {
        let (levels, name) = key_to_parts(key);
        let levels = levels.join(".");
        // println!("Inserting value to {}.{}", levels, name);
        self.contents
            .insert(format!("{}.{}", levels, name), value as i64);
    }

    fn observe_gauge(&mut self, key: Key, value: i64) {
        let (levels, name) = key_to_parts(key);
        let levels = levels.join(".");
        // println!("Inserting value to {}.{}", levels, name);
        self.contents.insert(format!("{}.{}", levels, name), value);
    }

    fn observe_histogram(&mut self, key: Key, values: &[u64]) {
        let entry = self
            .histos
            .entry(key)
            .or_insert_with(|| Histogram::<u64>::new(3).expect("failed to create histogram"));

        for value in values {
            entry
                .record(*value)
                .expect("failed to observe histogram value");
        }
    }
}
impl Drain<Vec<models::NewMetric>> for SqliteObserver {
    fn drain(&mut self) -> Vec<models::NewMetric> {
        for (key, h) in self.histos.drain() {
            let (levels, name) = key_to_parts(key);
            let levels = levels.join(".");
            let values = hist_to_values(name, h.clone(), &self.quantiles);
            // self.tree.insert_values(levels, values);
            for (name, value) in values {
                self.contents
                    .insert(format!("{}.{}", levels, name), value as i64);
            }
            // println!("{:?}: {:?}", levels, values);
        }
        let timestamp = std::time::SystemTime::UNIX_EPOCH
            .elapsed()
            .unwrap()
            .as_secs_f64();
        let results = self
            .contents
            .iter()
            .map(|(k, v)| models::NewMetric {
                key: k.into(),
                value: *v,
                timestamp,
            })
            .collect();
        self.contents.clear();
        results
    }
}

fn key_to_parts(key: Key) -> (Vec<String>, String) {
    let (name, labels) = key.into_parts();
    let mut parts = name.split('.').map(ToOwned::to_owned).collect::<Vec<_>>();
    let name = parts.pop().expect("name didn't have a single part");

    let labels = labels
        .into_iter()
        .map(Label::into_parts)
        .map(|(k, v)| format!("{}=\"{}\"", k, v))
        .collect::<Vec<_>>()
        .join(",");
    let label = if labels.is_empty() {
        String::new()
    } else {
        format!("{{{}}}", labels)
    };

    let fname = format!("{}{}", name, label);

    (parts, fname)
}

fn hist_to_values(
    name: String,
    hist: Histogram<u64>,
    quantiles: &[Quantile],
) -> Vec<(String, u64)> {
    let mut values = Vec::new();

    values.push((format!("{}_count", name), hist.len()));
    for quantile in quantiles {
        let value = hist.value_at_quantile(quantile.value());
        values.push((format!("{}_{}", name, quantile.label()), value));
    }

    values
}

/// Exports metrics by converting them to a textual representation and logging them.
pub struct SqliteExporter<C, B>
where
    B: Builder,
{
    controller: C,
    observer: B::Output,
    interval: Duration,
    db: SqliteConnection,
}

impl<C, B> SqliteExporter<C, B>
where
    B: Builder,
    B::Output: Drain<Vec<models::NewMetric>> + Observer,
    C: Observe,
{
    /// Creates a new [`SqliteExporter`] that stores metrics in a SQLite database file.
    ///
    /// Observers expose their output by being converted into a diesel model.
    pub fn new<P: AsRef<Path>>(
        controller: C,
        builder: B,
        interval: Duration,
        path: P,
    ) -> Result<Self> {
        let db = setup_db(path)?;
        Ok(SqliteExporter {
            controller,
            observer: builder.build(),
            interval,
            db,
        })
    }

    /// Runs this exporter on the current thread, storing on given interval
    pub fn run(&mut self) {
        loop {
            thread::sleep(self.interval);

            self.turn();
        }
    }

    /// Run this exporter, storing output only once.
    pub fn turn(&mut self) {
        use crate::schema::metrics::dsl::metrics;
        self.controller.observe(&mut self.observer);
        let output = self.observer.drain();
        for rec in output {
            insert_into(metrics).values(&rec).execute(&self.db).unwrap();
        }
    }
}
