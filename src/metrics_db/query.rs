use super::{Metric, MetricKey, MetricsError, Result, Session, SqliteConnection};
use diesel::prelude::*;
use std::collections::HashMap;

pub(crate) fn metric_key_for_name<'a>(
    db: &'a mut SqliteConnection,
    key_name: &str,
) -> Result<MetricKey<'a>> {
    use crate::schema::metric_keys::dsl::*;
    let query = metric_keys.filter(key.eq(key_name));
    let keys = query.load::<MetricKey>(db)?;
    keys.into_iter()
        .next()
        .ok_or_else(|| MetricsError::KeyNotFound(key_name.to_string()))
}

pub(crate) fn session_from_signpost(db: &mut SqliteConnection, metric: &str) -> Result<Session> {
    use crate::schema::metrics::dsl::*;
    let metric_key = metric_key_for_name(db, metric)?;
    let query = metrics
        .order(timestamp.desc())
        .filter(metric_key_id.eq(metric_key.id))
        .limit(1);
    let start = query.first::<Metric>(db)?;
    let end_query = metrics.order(timestamp.desc()).limit(1);
    let end = end_query.first::<Metric>(db)?;
    Ok(Session::new(start.timestamp, end.timestamp))
}

pub(crate) fn metrics_for_key(
    db: &mut SqliteConnection,
    key_name: &str,
    session: Option<&Session>,
) -> Result<Vec<Metric>> {
    use crate::schema::metrics::dsl::*;
    let metric_key = metric_key_for_name(db, key_name)?;
    let query = metrics
        .order(timestamp.asc())
        .filter(metric_key_id.eq(metric_key.id));
    let rows = match session {
        Some(session) => query
            .filter(timestamp.ge(session.start_time))
            .filter(timestamp.le(session.end_time))
            .load::<Metric>(db)?,
        None => query.load::<Metric>(db)?,
    };
    Ok(rows)
}

pub(crate) fn average_for_session(
    db: &mut SqliteConnection,
    key_name: &str,
    session: &Session,
) -> Result<f64> {
    let metrics = metrics_for_key(db, key_name, Some(session))?;
    let sum: f64 = metrics.iter().map(|m| m.value).sum();
    let samples = metrics.len();
    let average = sum / samples as f64;
    Ok(average)
}

pub(crate) fn metrics_summary_for_signpost_and_keys(
    db: &mut SqliteConnection,
    signpost: &str,
    keys: Vec<String>,
) -> Result<HashMap<String, f64>> {
    let session = session_from_signpost(db, signpost)?;
    let mut results = HashMap::new();
    for key in keys {
        let value = average_for_session(db, &key, &session)?;
        results.insert(key, value);
    }
    let duration_secs = session.end_time - session.start_time;
    results.insert("session.duration".to_string(), duration_secs);
    Ok(results)
}
