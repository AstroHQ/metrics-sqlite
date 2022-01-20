use crate::{Event, RegisterType, SqliteExporter};
use metrics::{GaugeValue, Key, Recorder, Unit};
use std::time::SystemTime;

impl Recorder for SqliteExporter {
    // in future we could record these to the SQLite database for informational/metadata usage
    fn register_counter(&self, key: Key, unit: Option<Unit>, description: Option<&'static str>) {
        if let Err(e) = self.sender.try_send(Event::RegisterKey(
            RegisterType::Counter,
            key,
            unit,
            description,
        )) {
            error!("Error sending metric registration: {:?}", e);
        }
    }

    fn register_gauge(&self, key: Key, unit: Option<Unit>, description: Option<&'static str>) {
        if let Err(e) = self.sender.try_send(Event::RegisterKey(
            RegisterType::Gauge,
            key,
            unit,
            description,
        )) {
            error!("Error sending metric registration: {:?}", e);
        }
    }

    fn register_histogram(&self, key: Key, unit: Option<Unit>, description: Option<&'static str>) {
        if let Err(e) = self.sender.try_send(Event::RegisterKey(
            RegisterType::Histogram,
            key,
            unit,
            description,
        )) {
            error!("Error sending metric registration: {:?}", e);
        }
    }

    fn increment_counter(&self, key: Key, value: u64) {
        match SystemTime::UNIX_EPOCH.elapsed() {
            Ok(timestamp) => {
                if let Err(_e) = self
                    .sender
                    .try_send(Event::IncrementCounter(timestamp, key, value))
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

    fn update_gauge(&self, key: Key, value: GaugeValue) {
        match SystemTime::UNIX_EPOCH.elapsed() {
            Ok(timestamp) => {
                if let Err(_e) = self
                    .sender
                    .try_send(Event::UpdateGauge(timestamp, key, value))
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

    fn record_histogram(&self, key: Key, value: f64) {
        match SystemTime::UNIX_EPOCH.elapsed() {
            Ok(timestamp) => {
                if let Err(_e) = self
                    .sender
                    .try_send(Event::UpdateHistogram(timestamp, key, value))
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
