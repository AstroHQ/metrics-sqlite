//! Bounded retention work and conservative disk-space reclamation.
use diesel::{
    prelude::*,
    sql_query,
    sql_types::{BigInt, Double},
};
use std::time::{Duration, Instant, SystemTime};

pub(crate) const DELETE_BATCH_SIZE: usize = 1_000;
pub(crate) const STEP_INTERVAL: Duration = Duration::from_millis(10);
pub(crate) const VACUUM_INTERVAL: Duration = Duration::from_secs(60 * 60);
const MIN_VACUUM_BYTES: i64 = 64 * 1024 * 1024;

pub(crate) struct Maintenance {
    cutoff: Option<f64>,
    record_limit: Option<usize>,
    remaining: Option<usize>,
}

impl Maintenance {
    pub(crate) fn new(retention: Option<Duration>, record_limit: Option<usize>) -> Self {
        Self {
            cutoff: retention.and_then(|keep| {
                SystemTime::UNIX_EPOCH
                    .elapsed()
                    .ok()?
                    .checked_sub(keep)
                    .map(|cutoff| cutoff.as_secs_f64())
            }),
            record_limit,
            remaining: None,
        }
    }

    /// Commit at most one bounded delete. Return true when cleanup is complete.
    pub(crate) fn step(&mut self, db: &mut SqliteConnection) -> QueryResult<bool> {
        if let Some(cutoff) = self.cutoff {
            let deleted = sql_query("DELETE FROM metrics WHERE id IN (SELECT id FROM metrics WHERE timestamp <= ? ORDER BY timestamp, id LIMIT ?)")
                .bind::<Double, _>(cutoff)
                .bind::<BigInt, _>(DELETE_BATCH_SIZE as i64)
                .execute(db)?;
            if deleted < DELETE_BATCH_SIZE {
                self.cutoff = None;
            }
            return Ok(false);
        }
        if self.remaining.is_none() {
            self.remaining = Some(if let Some(limit) = self.record_limit {
                use crate::schema::metrics::dsl::*;
                let records = metrics.count().get_result::<i64>(db)? as usize;
                if records > limit {
                    records - limit + limit / 4
                } else {
                    0
                }
            } else {
                0
            });
        }
        let remaining = self.remaining.as_mut().unwrap();
        if *remaining > 0 {
            let deleted = sql_query("DELETE FROM metrics WHERE id IN (SELECT id FROM metrics ORDER BY timestamp, id LIMIT ?)")
                .bind::<BigInt, _>((*remaining).min(DELETE_BATCH_SIZE) as i64)
                .execute(db)?;
            *remaining = remaining.saturating_sub(deleted);
            if deleted == 0 {
                *remaining = 0;
            }
        }
        Ok(*remaining == 0)
    }
}

#[derive(QueryableByName)]
struct Space {
    #[diesel(sql_type = BigInt)]
    free: i64,
    #[diesel(sql_type = BigInt)]
    pages: i64,
    #[diesel(sql_type = BigInt)]
    page_size: i64,
}

/// Avoid a full rewrite unless at least 64 MiB and 25% of the file is free.
pub(crate) fn reclaim(
    db: &mut SqliteConnection,
    last_attempt: &mut Option<Instant>,
) -> QueryResult<bool> {
    if last_attempt.is_some_and(|last| last.elapsed() < VACUUM_INTERVAL) {
        return Ok(false);
    }
    let space = sql_query("SELECT freelist_count AS free, page_count AS pages, page_size FROM pragma_freelist_count(), pragma_page_count(), pragma_page_size()")
        .get_result::<Space>(db)?;
    if space.free * space.page_size >= MIN_VACUUM_BYTES && space.free >= space.pages / 4 {
        // Failed rewrites can be expensive too, especially on a full disk.
        *last_attempt = Some(Instant::now());
        sql_query("VACUUM").execute(db)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Do not wait for readers: a busy checkpoint can be retried after later work.
pub(crate) fn checkpoint(db: &mut SqliteConnection) -> QueryResult<()> {
    sql_query("PRAGMA busy_timeout = 0").execute(db)?;
    let result = sql_query("PRAGMA wal_checkpoint(TRUNCATE)").execute(db);
    let restore = sql_query("PRAGMA busy_timeout = 5000").execute(db);
    result?;
    restore?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database() -> (SqliteConnection, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (crate::setup_db(dir.path().join("test.db")).unwrap(), dir)
    }

    fn populate(db: &mut SqliteConnection, rows: usize) {
        let samples: Vec<_> = (0..rows)
            .map(|n| crate::NewMetric {
                timestamp: n as f64,
                metric_key_id: 1,
                value: n as f64,
            })
            .collect();
        crate::InnerState::insert_metrics(db, [samples.as_slice()]).unwrap();
    }

    fn count(db: &mut SqliteConnection) -> i64 {
        crate::schema::metrics::table
            .count()
            .get_result(db)
            .unwrap()
    }

    #[test]
    fn retention_is_bounded_and_preserves_cutoff_boundary() {
        let (mut db, _dir) = database();
        populate(&mut db, 2500);
        let mut work = Maintenance {
            cutoff: Some(1499.0),
            record_limit: None,
            remaining: None,
        };
        assert!(!work.step(&mut db).unwrap());
        assert_eq!(count(&mut db), 1500);
        assert!(!work.step(&mut db).unwrap());
        assert!(work.step(&mut db).unwrap());
        use crate::schema::metrics::dsl::*;
        assert_eq!(
            metrics
                .select(timestamp)
                .order(timestamp)
                .first::<f64>(&mut db)
                .unwrap(),
            1500.0
        );
        assert_eq!(count(&mut db), 1000);
    }

    #[test]
    fn record_limit_deletes_oldest_in_batches_with_existing_headroom() {
        let (mut db, _dir) = database();
        populate(&mut db, 3500);
        // Insertion order need not match sample time (e.g. multiple producers).
        sql_query("UPDATE metrics SET timestamp = 3499 - timestamp")
            .execute(&mut db)
            .unwrap();
        let mut work = Maintenance::new(None, Some(2000));
        assert!(!work.step(&mut db).unwrap());
        assert_eq!(count(&mut db), 2500);
        assert!(work.step(&mut db).unwrap());
        assert_eq!(count(&mut db), 1500);
        use crate::schema::metrics::dsl::*;
        assert_eq!(
            metrics
                .select(timestamp)
                .order(timestamp)
                .first::<f64>(&mut db)
                .unwrap(),
            2000.0
        );
    }

    #[test]
    fn zero_limit_and_retention_larger_than_epoch_are_safe() {
        let (mut db, _dir) = database();
        populate(&mut db, 10);
        let mut work = Maintenance::new(Some(Duration::MAX), None);
        assert!(work.step(&mut db).unwrap());
        assert_eq!(count(&mut db), 10);
        assert!(Maintenance::new(None, Some(0)).step(&mut db).unwrap());
        assert_eq!(count(&mut db), 0);
    }

    #[test]
    fn reclaim_skips_small_databases_and_shrinks_large_free_files() {
        let (mut db, dir) = database();
        assert!(!reclaim(&mut db, &mut None).unwrap());
        sql_query("CREATE TABLE ballast (data BLOB)")
            .execute(&mut db)
            .unwrap();
        sql_query("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1100) INSERT INTO ballast SELECT zeroblob(65536) FROM n")
            .execute(&mut db).unwrap();
        checkpoint(&mut db).unwrap();
        let before = std::fs::metadata(dir.path().join("test.db")).unwrap().len();
        sql_query("DELETE FROM ballast").execute(&mut db).unwrap();
        assert!(reclaim(&mut db, &mut None).unwrap());
        checkpoint(&mut db).unwrap();
        let after = std::fs::metadata(dir.path().join("test.db")).unwrap().len();
        assert!(before > 64 * 1024 * 1024);
        assert!(after < before / 4, "before={before}, after={after}");
        assert_eq!(
            std::fs::metadata(dir.path().join("test.db-wal"))
                .unwrap()
                .len(),
            0
        );
    }

    #[test]
    fn checkpoint_does_not_wait_for_a_reader_and_can_retry() {
        let (mut db, dir) = database();
        populate(&mut db, 10);
        let mut reader = crate::setup_db(dir.path().join("test.db")).unwrap();
        sql_query("BEGIN").execute(&mut reader).unwrap();
        assert_eq!(count(&mut reader), 10);
        populate(&mut db, 10);
        let start = std::time::Instant::now();
        checkpoint(&mut db).unwrap();
        assert!(start.elapsed() < Duration::from_secs(1));
        assert_eq!(count(&mut reader), 10);
        sql_query("ROLLBACK").execute(&mut reader).unwrap();
        checkpoint(&mut db).unwrap();
        assert_eq!(count(&mut db), 20);
        assert_eq!(
            std::fs::metadata(dir.path().join("test.db-wal"))
                .unwrap()
                .len(),
            0
        );
    }
}
