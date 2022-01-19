//! Diesel models of metrics sqlite storage
use crate::schema::{metric_keys, metrics};
use crate::{MetricsError, Result};
use diesel::prelude::*;
use std::borrow::Cow;

/// A new metric measurement for storing into sqlite database
#[derive(Insertable, Debug)]
#[table_name = "metrics"]
pub struct NewMetric {
    /// Timestamp of sample
    pub timestamp: f64,
    /// Key/name of sample
    pub metric_key_id: i64,
    /// Value of sample
    pub value: f64,
}

/// New metric key entry
#[derive(Insertable, Debug)]
#[table_name = "metric_keys"]
pub struct NewMetricKey<'a> {
    /// Actual key
    pub key: Cow<'a, str>,
    /// Unit if any
    pub unit: Cow<'a, str>,
}

/// Metric key
#[derive(Queryable, Debug, Identifiable)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct MetricKey<'a> {
    /// primary key of metric key
    pub id: i64,
    /// Actual key
    pub key: Cow<'a, str>,
    /// Unit if any
    pub unit: Cow<'a, str>,
}
impl<'a> MetricKey<'a> {
    pub(crate) fn key_by_name(key_name: &str, db: &SqliteConnection) -> Result<MetricKey<'a>> {
        use crate::schema::metric_keys::dsl::metric_keys;
        match Self::key_by_name_inner(key_name, db) {
            Ok(key) => Ok(key),
            Err(MetricsError::KeyNotFound(_)) => {
                // not stored yet so create an entry
                let new_key = NewMetricKey {
                    key: Cow::Borrowed(key_name),
                    unit: Cow::Borrowed(""),
                };
                new_key.insert_into(metric_keys).execute(db)?;
                // fetch it back out to get the ID
                Self::key_by_name_inner(key_name, db)
            }
            Err(e) => Err(e),
        }
    }
    fn key_by_name_inner(key_name: &str, db: &SqliteConnection) -> Result<MetricKey<'a>> {
        use crate::schema::metric_keys::dsl::*;
        let query = metric_keys.filter(key.eq(key_name));
        let keys = query.load::<MetricKey>(db)?;
        keys.into_iter()
            .next()
            .ok_or(MetricsError::KeyNotFound(key_name.to_string()))
    }
}

/// Metric model for existing entries in sqlite database
#[derive(Queryable, Debug, Identifiable, Associations)]
#[belongs_to(MetricKey<'_>)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Metric {
    /// Unique ID of sample
    pub id: i64,
    /// Timestamp of sample
    pub timestamp: f64,
    /// Key/name of sample
    pub metric_key_id: i64,
    /// Value of sample
    pub value: f64,
}
