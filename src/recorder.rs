use crate::{Event, RegisterType, SqliteExporter};
use metrics::{
    Counter, CounterFn, Gauge, GaugeFn, GaugeValue, Histogram, HistogramFn, Key, KeyName, Recorder,
    SharedString, Unit,
};
use std::sync::mpsc::SyncSender;
use std::sync::Arc;
use std::time::SystemTime;

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
            error!("Error sending metric description: {:?}", e);
        }
    }

    fn describe_gauge(&self, key: KeyName, unit: Option<Unit>, description: SharedString) {
        if let Err(e) = self.sender.try_send(Event::DescribeKey(
            RegisterType::Gauge,
            key,
            unit,
            description,
        )) {
            error!("Error sending metric description: {:?}", e);
        }
    }

    fn describe_histogram(&self, key: KeyName, unit: Option<Unit>, description: SharedString) {
        if let Err(e) = self.sender.try_send(Event::DescribeKey(
            RegisterType::Histogram,
            key,
            unit,
            description,
        )) {
            error!("Error sending metric description: {:?}", e);
        }
    }

    // in future we could record these to the SQLite database for informational/metadata usage
    fn register_counter(&self, key: &Key) -> Counter {
        let sender = self.sender.clone();
        let handle = Arc::new(Handle {
            sender,
            key: key.clone(),
        });
        if let Err(e) = self.sender.try_send(Event::RegisterKey(
            RegisterType::Counter,
            key.clone(),
            handle.clone(),
        )) {
            error!("Error sending metric registration: {:?}", e);
        }
        Counter::from_arc(handle)
    }

    fn register_gauge(&self, key: &Key) -> Gauge {
        let sender = self.sender.clone();
        let handle = Arc::new(Handle {
            sender,
            key: key.clone(),
        });
        if let Err(e) = self.sender.try_send(Event::RegisterKey(
            RegisterType::Gauge,
            key.clone(),
            handle.clone(),
        )) {
            error!("Error sending metric registration: {:?}", e);
        }
        Gauge::from_arc(handle)
    }

    fn register_histogram(&self, key: &Key) -> Histogram {
        let sender = self.sender.clone();
        let handle = Arc::new(Handle {
            sender,
            key: key.clone(),
        });
        if let Err(e) = self.sender.try_send(Event::RegisterKey(
            RegisterType::Histogram,
            key.clone(),
            handle.clone(),
        )) {
            error!("Error sending metric registration: {:?}", e);
        }
        Histogram::from_arc(handle)
    }
}
