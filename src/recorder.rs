use crate::{Event, SqliteExporter};
use metrics::{GaugeValue, Key, Recorder, Unit};
use std::time::SystemTime;

impl Recorder for SqliteExporter {
    // in future we could record these to the SQLite database for informational/metadata usage
    fn register_counter(&self, _key: Key, _unit: Option<Unit>, _description: Option<&'static str>) {
    }

    fn register_gauge(&self, _key: Key, _unit: Option<Unit>, _description: Option<&'static str>) {}

    fn register_histogram(
        &self,
        _key: Key,
        _unit: Option<Unit>,
        _description: Option<&'static str>,
    ) {
    }

    fn increment_counter(&self, key: Key, value: u64) {
        match SystemTime::UNIX_EPOCH.elapsed() {
            Ok(timestamp) => {
                if let Err(e) = self.sender.try_send(Event::IncrementCounter(timestamp, key, value)) {
                    error!("Error sending metric to SQLite thread: {}, dropping metric", e);
                }
            }
            Err(e) => {
                error!("Failed to get system time: {}, dropping metric", e);
            }
        }
    }

    fn update_gauge(&self, key: Key, value: GaugeValue) {
        match SystemTime::UNIX_EPOCH.elapsed() {
            Ok(timestamp) => {
                if let Err(e) = self.sender.try_send(Event::UpdateGauge(timestamp, key, value)) {
                    error!("Error sending metric to SQLite thread: {}, dropping metric", e);
                }
            }
            Err(e) => {
                error!("Failed to get system time: {}, dropping metric", e);
            }
        }
    }

    fn record_histogram(&self, key: Key, value: f64) {
        match SystemTime::UNIX_EPOCH.elapsed() {
            Ok(timestamp) => {
                if let Err(e) = self.sender.try_send(Event::UpdateHistogram(timestamp, key, value)) {
                    error!("Error sending metric to SQLite thread: {}, dropping metric", e);
                }
            }
            Err(e) => {
                error!("Failed to get system time: {}, dropping metric", e);
            }
        }
    }
}
