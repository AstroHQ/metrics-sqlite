## v0.7.0 (unreleased)

### Compatibility notes

- Declare Rust 1.86 as the minimum supported version, matching the existing
  Diesel 2.3 dependency requirements.
- Public method signatures are unchanged, but housekeeping behavior changes.
- `set_periodic_housekeeping` with `retention: None` now inherits the
  `keep_duration` passed to `SqliteExporter::new`. Previously it disabled
  periodic time-based deletion. For record-limit-only cleanup, pass `None`
  for the constructor's `keep_duration` as well.
- Startup retention cleanup now runs asynchronously on the worker. Old rows
  may remain visible after construction, and shutdown does not wait for
  pending cleanup.
- Enabled housekeeping can also run based on successful insert counts,
  enforcing configured limits before the periodic timer expires.

### Storage and performance

- Delete in batches of at most 1,000 rows, allowing worker event processing
  between batches.
- Replace unconditional startup VACUUM with worker-side reclamation requiring
  at least 64 MiB and 25% free space, with successful and failed attempts limited
  to once per hour per worker. Checkpoint completed deletes even if VACUUM fails.
- Start the interval between cleanup batches after SQLite work completes,
  including lock waits. Disabling periodic housekeeping cancels its pending
  batches while preserving startup cleanup under the constructor's policy.
- Set a 4 MiB retained WAL size limit and attempt nonwaiting truncate
  checkpoints after cleanup and graceful shutdown. Active readers can still
  prevent WAL shrinking; this is not a hard size cap.
- Preserve synchronous database integrity checks and all gauge samples.

<a name="v0.2.1"></a>
## v0.2.1 (2021-03-10)


#### Bug Fixes

*   fixes #5, avoids panic from RefCell by moving logic to worker ([96384308](96384308))

<a name="v0.2.0"></a>
## v0.2.0 (2021-03-09)

#### Features

* metrics 0.14 
* ability to flush to disk less frequently
