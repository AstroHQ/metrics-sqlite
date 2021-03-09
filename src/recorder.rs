use crate::{Event, NewMetric, SqliteExporter};
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
        let timestamp = SystemTime::UNIX_EPOCH.elapsed().unwrap().as_secs_f64();
        let mut counters = self.counters.borrow_mut();
        let key_str = key.name().to_string();
        let entry = counters.entry(key).or_insert(0);
        let value = {
            *entry = *entry + value;
            *entry
        };
        let metric = NewMetric {
            timestamp,
            key: key_str,
            value: value as _,
        };
        if let Err(e) = self.sender.try_send(Event::Metric(metric)) {
            error!("Error sending metric to SQLite thread: {}", e);
        }
    }

    fn update_gauge(&self, key: Key, value: GaugeValue) {
        let timestamp = SystemTime::UNIX_EPOCH.elapsed().unwrap().as_secs_f64();
        let mut last_values = self.last_values.borrow_mut();
        let key_str = key.name().to_string();
        let entry = last_values.entry(key).or_insert(0.0);
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
            timestamp,
            key: key_str,
            value,
        };
        if let Err(e) = self.sender.try_send(Event::Metric(metric)) {
            error!("Error sending metric to SQLite thread: {}", e);
        }
    }

    fn record_histogram(&self, key: Key, value: f64) {
        let timestamp = SystemTime::UNIX_EPOCH.elapsed().unwrap().as_secs_f64();
        let mut last_values = self.last_values.borrow_mut();
        let key_str = key.name().to_string();
        let entry = last_values.entry(key).or_insert(0.0);
        let value = {
            *entry = value;
            *entry
        };

        let metric = NewMetric {
            timestamp,
            key: key_str,
            value,
        };
        if let Err(e) = self.sender.try_send(Event::Metric(metric)) {
            error!("Error sending metric to SQLite thread: {}", e);
        }
    }
}
