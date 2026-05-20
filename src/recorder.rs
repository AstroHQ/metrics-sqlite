use crate::{Event, RegisterType, SqliteExporter};
use metrics::{
    Counter, CounterFn, Gauge, GaugeFn, GaugeValue, Histogram, HistogramFn, Key, KeyName, Metadata,
    Recorder, SharedString, Unit,
};
use std::{
    sync::{Arc, mpsc::SyncSender},
    time::SystemTime,
};
#[cfg(feature = "log_dropped_metrics")]
use tracing::error;

pub(crate) struct Handle {
    sender: SyncSender<Event>,
    key: Key,
}
impl CounterFn for Handle {
    fn increment(&self, value: u64) {
        match SystemTime::UNIX_EPOCH.elapsed() {
            Ok(timestamp) => {
                if let Err(_e) = self.sender.try_send(Event::IncrementCounter(
                    timestamp,
                    self.key.clone(),
                    value,
                )) {
                    #[cfg(feature = "log_dropped_metrics")]
                    error!(
                        "Error sending metric to SQLite thread: {}, dropping metric",
                        _e
                    );
                }
            }
            Err(_e) => {
                #[cfg(feature = "log_dropped_metrics")]
                error!("Failed to get system time: {}, dropping metric", _e);
            }
        }
    }

    fn absolute(&self, value: u64) {
        match SystemTime::UNIX_EPOCH.elapsed() {
            Ok(timestamp) => {
                if let Err(_e) =
                    self.sender
                        .try_send(Event::AbsoluteCounter(timestamp, self.key.clone(), value))
                {
                    #[cfg(feature = "log_dropped_metrics")]
                    error!(
                        "Error sending metric to SQLite thread: {}, dropping metric",
                        _e
                    );
                }
            }
            Err(_e) => {
                #[cfg(feature = "log_dropped_metrics")]
                error!("Failed to get system time: {}, dropping metric", _e);
            }
        }
    }
}
impl GaugeFn for Handle {
    fn increment(&self, value: f64) {
        match SystemTime::UNIX_EPOCH.elapsed() {
            Ok(timestamp) => {
                if let Err(_e) = self.sender.try_send(Event::UpdateGauge(
                    timestamp,
                    self.key.clone(),
                    GaugeValue::Increment(value),
                )) {
                    #[cfg(feature = "log_dropped_metrics")]
                    error!(
                        "Error sending metric to SQLite thread: {}, dropping metric",
                        _e
                    );
                }
            }
            Err(_e) => {
                #[cfg(feature = "log_dropped_metrics")]
                error!("Failed to get system time: {}, dropping metric", _e);
            }
        }
    }

    fn decrement(&self, value: f64) {
        match SystemTime::UNIX_EPOCH.elapsed() {
            Ok(timestamp) => {
                if let Err(_e) = self.sender.try_send(Event::UpdateGauge(
                    timestamp,
                    self.key.clone(),
                    GaugeValue::Decrement(value),
                )) {
                    #[cfg(feature = "log_dropped_metrics")]
                    error!(
                        "Error sending metric to SQLite thread: {}, dropping metric",
                        _e
                    );
                }
            }
            Err(_e) => {
                #[cfg(feature = "log_dropped_metrics")]
                error!("Failed to get system time: {}, dropping metric", _e);
            }
        }
    }

    fn set(&self, value: f64) {
        match SystemTime::UNIX_EPOCH.elapsed() {
            Ok(timestamp) => {
                if let Err(_e) = self.sender.try_send(Event::UpdateGauge(
                    timestamp,
                    self.key.clone(),
                    GaugeValue::Absolute(value),
                )) {
                    #[cfg(feature = "log_dropped_metrics")]
                    error!(
                        "Error sending metric to SQLite thread: {}, dropping metric",
                        _e
                    );
                }
            }
            Err(_e) => {
                #[cfg(feature = "log_dropped_metrics")]
                error!("Failed to get system time: {}, dropping metric", _e);
            }
        }
    }
}
impl HistogramFn for Handle {
    fn record(&self, value: f64) {
        match SystemTime::UNIX_EPOCH.elapsed() {
            Ok(timestamp) => {
                if let Err(_e) =
                    self.sender
                        .try_send(Event::UpdateHistogram(timestamp, self.key.clone(), value))
                {
                    #[cfg(feature = "log_dropped_metrics")]
                    error!(
                        "Error sending metric to SQLite thread: {}, dropping metric",
                        _e
                    );
                }
            }
            Err(_e) => {
                #[cfg(feature = "log_dropped_metrics")]
                error!("Failed to get system time: {}, dropping metric", _e);
            }
        }
    }
}
impl Recorder for SqliteExporter {
    fn describe_counter(&self, key: KeyName, unit: Option<Unit>, description: SharedString) {
        if let Err(e) = self.sender.try_send(Event::DescribeKey(
            RegisterType::Counter,
            key,
            unit,
            description,
        )) {
            self.log_send_failure("description", &e);
        }
    }

    fn describe_gauge(&self, key: KeyName, unit: Option<Unit>, description: SharedString) {
        if let Err(e) = self.sender.try_send(Event::DescribeKey(
            RegisterType::Gauge,
            key,
            unit,
            description,
        )) {
            self.log_send_failure("description", &e);
        }
    }

    fn describe_histogram(&self, key: KeyName, unit: Option<Unit>, description: SharedString) {
        if let Err(e) = self.sender.try_send(Event::DescribeKey(
            RegisterType::Histogram,
            key,
            unit,
            description,
        )) {
            self.log_send_failure("description", &e);
        }
    }

    // Registration doesn't touch the worker channel: the previous implementation
    // sent a `RegisterKey` event that the worker ignored, which under load
    // produced the dominant share of "TrySendError::Full" spam.
    fn register_counter(&self, key: &Key, _metadata: &Metadata) -> Counter {
        Counter::from_arc(Arc::new(Handle {
            sender: self.sender.clone(),
            key: key.clone(),
        }))
    }

    fn register_gauge(&self, key: &Key, _metadata: &Metadata) -> Gauge {
        Gauge::from_arc(Arc::new(Handle {
            sender: self.sender.clone(),
            key: key.clone(),
        }))
    }

    fn register_histogram(&self, key: &Key, _metadata: &Metadata) -> Histogram {
        Histogram::from_arc(Arc::new(Handle {
            sender: self.sender.clone(),
            key: key.clone(),
        }))
    }
}
