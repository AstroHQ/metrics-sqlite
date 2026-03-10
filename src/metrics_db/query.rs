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
    // Use the most recent signpost occurrence as the session start
    let start_query = metrics
        .order(timestamp.desc())
        .filter(metric_key_id.eq(metric_key.id))
        .limit(1);
    let start = start_query.first::<Metric>(db)?;
    // End is the latest metric in the DB (approximates NOW)
    let end_query = metrics.order(timestamp.desc()).limit(1);
    let end = end_query.first::<Metric>(db)?;
    if end.timestamp <= start.timestamp {
        return Err(MetricsError::ZeroLengthSession(metric.to_string()));
    }
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
    use crate::schema::metrics::dsl::*;
    use diesel::dsl::{avg, count_star};

    let metric_key = metric_key_for_name(db, key_name)?;
    let (average, count): (Option<f64>, i64) = metrics
        .filter(metric_key_id.eq(metric_key.id))
        .filter(timestamp.ge(session.start_time))
        .filter(timestamp.le(session.end_time))
        .select((avg(value), count_star()))
        .first(db)?;
    if count == 0 {
        Err(MetricsError::NoMetricsForKey(key_name.to_string()))
    } else {
        Ok(average.unwrap_or(0.0))
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{MetricKey, NewMetric};

    /// Create a temp DB and return the connection + temp dir (keep alive for DB lifetime)
    fn setup_test_db() -> (SqliteConnection, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = crate::setup_db(&db_path).unwrap();
        (db, dir)
    }

    /// Insert a metric key, returning its id
    fn insert_key(db: &mut SqliteConnection, name: &str) -> i64 {
        let key = MetricKey::key_by_name(name, db).unwrap();
        key.id
    }

    /// Insert a metric row
    fn insert_metric(db: &mut SqliteConnection, key_id: i64, ts: f64, val: f64) {
        use crate::schema::metrics::dsl::*;
        diesel::insert_into(metrics)
            .values(&NewMetric {
                timestamp: ts,
                metric_key_id: key_id,
                value: val,
            })
            .execute(db)
            .unwrap();
    }

    /// Sets up a DB simulating stale signpost data from a previous session:
    /// - "app.started" signpost at t=100.0 (old/stale session)
    /// - "app.started" signpost at t=500.0 (current session)
    /// - "cpu" metrics at t=500, 510, 520 with values 50, 60, 70 (current session)
    /// - "cpu" metrics at t=100, 110 with values 10, 20 (old session)
    /// - latest metric in DB is at t=520
    fn populate_test_db(db: &mut SqliteConnection) {
        let signpost_id = insert_key(db, "app.started");
        let cpu_id = insert_key(db, "cpu");

        // Old session data (stale)
        insert_metric(db, signpost_id, 100.0, 1.0);
        insert_metric(db, cpu_id, 100.0, 10.0);
        insert_metric(db, cpu_id, 110.0, 20.0);

        // Current session data
        insert_metric(db, signpost_id, 500.0, 1.0);
        insert_metric(db, cpu_id, 500.0, 50.0);
        insert_metric(db, cpu_id, 510.0, 60.0);
        insert_metric(db, cpu_id, 520.0, 70.0);
    }

    #[test]
    fn test_session_from_signpost_uses_latest_signpost() {
        // With stale signpost data at t=100 and current at t=500,
        // start should be 500 (latest signpost), not 100 (oldest).
        let (mut db, _dir) = setup_test_db();
        populate_test_db(&mut db);

        let session = session_from_signpost(&mut db, "app.started").unwrap();
        assert_eq!(
            session.start_time, 500.0,
            "start should be the most recent signpost (t=500), not the stale one (t=100)"
        );
        assert_eq!(session.end_time, 520.0);
    }

    #[test]
    fn test_summary_duration_not_inflated_by_stale_signpost() {
        // Duration should be based on the latest signpost, not a stale one from a previous session.
        let (mut db, _dir) = setup_test_db();
        populate_test_db(&mut db);

        let results = metrics_summary_for_signpost_and_keys(
            &mut db,
            "app.started",
            vec!["cpu".to_string()],
        )
        .unwrap();

        // cpu average across t=500..520 (current session only): (50+60+70)/3 = 60
        assert!(
            (results["cpu"] - 60.0).abs() < f64::EPSILON,
            "cpu average should only include current session metrics"
        );
        // session.duration should be 520 - 500 = 20, not 520 - 100 = 420
        assert_eq!(
            results["session.duration"], 20.0,
            "duration should be 20s (t=500..520), not inflated to 420s by stale signpost"
        );
    }

    #[test]
    fn test_average_for_session_with_explicit_range() {
        let (mut db, _dir) = setup_test_db();
        populate_test_db(&mut db);

        // With a manually bounded session t=500..510, avg = (50+60)/2 = 55
        let session = Session::new(500.0, 510.0);
        let avg = average_for_session(&mut db, "cpu", &session).unwrap();
        assert!((avg - 55.0).abs() < f64::EPSILON);
    }
}
