#[cfg(feature = "color")]
use colored::*;
use log::{Level, Log, Metadata, Record, SetLoggerError, warn};
use serde::de::Error as SerdeError;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
use time::{OffsetDateTime, format_description::FormatItem, macros::format_description};
use tokio::sync::RwLock;
use tokio::sync::broadcast::{Receiver, Sender};

const TIMESTAMP_FORMAT_LOCAL: &[FormatItem] =
    format_description!("[year]-[month]-[day] [hour]:[minute]:[second].[subsecond digits:3]");
const TIMESTAMP_FORMAT_OFFSET: &[FormatItem] = format_description!(
    "[year]-[month]-[day] [hour]:[minute]:[second].[subsecond digits:3][offset_hour sign:mandatory]:[offset_minute]"
);
const TIMESTAMP_FORMAT_UTC: &[FormatItem] =
    format_description!("[year]-[month]-[day] [hour]:[minute]:[second].[subsecond digits:3]Z");
const TIMESTAMP_FORMAT_SIMPLE: &[FormatItem] =
    format_description!("[year]-[month]-[day] [hour]:[minute]:[second].[subsecond digits:2]");

pub enum TimestampFormat {
    Local,
    Offset,
    UTC,
    Simple,
    Relative,
}

pub struct DruidGardenLogger {
    pub use_colors: bool,
    pub show_thread: bool,
    pub show_timestamp: bool,
    pub show_level: bool,
    pub show_target: bool,
    pub current_level: Level,
    pub timestamp_format: TimestampFormat,
    pub target_levels: Vec<(String, Level)>,
    pub start_instant: Instant,
    pub printed_error: AtomicBool,
    pub buffer: Arc<RwLock<VecDeque<LogEvent>>>,
    pub channel: Sender<LogEvent>,
}

pub struct DruidGardenLoggerBuilder {
    use_colors: bool,
    show_thread: bool,
    show_timestamp: bool,
    show_level: bool,
    show_target: bool,
    current_level: Level,
    timestamp_format: TimestampFormat,
    target_levels: Vec<(String, Level)>,
}

impl Default for DruidGardenLoggerBuilder {
    fn default() -> Self {
        Self {
            use_colors: true,
            show_thread: false,
            show_timestamp: true,
            show_level: true,
            show_target: true,
            timestamp_format: TimestampFormat::Local,
            current_level: Level::Debug,
            target_levels: vec![],
        }
    }
}

impl DruidGardenLoggerBuilder {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn use_colors(mut self, use_colors: bool) -> Self {
        self.use_colors = use_colors;
        self
    }
    pub fn show_thread(mut self, show_thread: bool) -> Self {
        self.show_thread = show_thread;
        self
    }
    pub fn show_timestamp(mut self, show_timestamp: bool) -> Self {
        self.show_timestamp = show_timestamp;
        self
    }
    pub fn show_level(mut self, show_level: bool) -> Self {
        self.show_level = show_level;
        self
    }
    pub fn show_target(mut self, show_target: bool) -> Self {
        self.show_target = show_target;
        self
    }
    pub fn timestamp_format(mut self, timestamp_format: TimestampFormat) -> Self {
        self.timestamp_format = timestamp_format;
        self
    }
    pub fn current_level(mut self, current_level: Level) -> Self {
        self.current_level = current_level;
        self
    }
    pub fn with_target_level(mut self, target: &str, current_level: Level) -> Self {
        self.target_levels.push((target.to_string(), current_level));
        self
    }
    pub fn build(mut self) -> DruidGardenLogger {
        self.target_levels.sort_by(|a, b| a.0.cmp(&b.0));
        DruidGardenLogger {
            use_colors: self.use_colors,
            show_thread: self.show_thread,
            show_timestamp: self.show_timestamp,
            show_level: self.show_level,
            show_target: self.show_target,
            current_level: self.current_level,
            timestamp_format: self.timestamp_format,
            target_levels: self.target_levels,
            start_instant: Instant::now(),
            printed_error: AtomicBool::new(false),
            buffer: Arc::new(Default::default()),
            channel: Sender::new(1024),
        }
    }
    pub fn init(self) -> Result<Arc<DruidGardenLogger>, SetLoggerError> {
        self.build().init()
    }
}

impl DruidGardenLogger {
    pub fn build() -> DruidGardenLoggerBuilder {
        DruidGardenLoggerBuilder::new()
    }
    pub fn init(self) -> Result<Arc<Self>, SetLoggerError> {
        let logger = Arc::new(self);
        // SAFETY: We call `Arc::into_raw(logger.clone())` to create a raw pointer from a cloned Arc.
        // 1. The clone bumps the strong count so that the inner value remains allocated even after converting
        //    the clone to a raw pointer.
        // 2. We then dereference this raw pointer to obtain a &'static reference. This is valid because the
        //    original `logger` is still live and maintained by its Arc, ensuring the data will not be dropped.
        // 3. We intentionally leak the cloned Arc (never converting it back), which is acceptable here since
        //    the global logger is meant to live for the entire duration of the program.
        // Thus, the resulting &'static reference is safe to use with `log::set_logger`.
        let static_logger: &'static Self = unsafe { &*Arc::into_raw(logger.clone()) };
        log::set_logger(static_logger).map(|_| {
            log::set_max_level(logger.current_level.to_level_filter());
            logger
        })
    }
    pub fn subscribe(&self) -> Receiver<LogEvent> {
        self.channel.subscribe()
    }
}

fn serialize_level<S>(level: &Level, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&level.to_string().to_lowercase())
}

fn deserialize_level<'de, D>(deserializer: D) -> Result<Level, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    match s.to_lowercase().as_str() {
        "error" => Ok(Level::Error),
        "warn" | "warning" => Ok(Level::Warn),
        "info" => Ok(Level::Info),
        "debug" => Ok(Level::Debug),
        "trace" => Ok(Level::Trace),
        _ => Err(D::Error::custom(format!("Unknown log level: {}", s))),
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct LogEvent {
    #[serde(
        serialize_with = "serialize_level",
        deserialize_with = "deserialize_level"
    )]
    pub level: Level,
    pub target: String,
    pub message: String,
    pub timestamp: OffsetDateTime,
}

impl Log for DruidGardenLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        match self
            .target_levels
            .iter()
            .find(|(pattern, _)| metadata.target().starts_with(pattern))
        {
            Some((_, level)) => metadata.level().to_level_filter() <= level.to_level_filter(),
            None => metadata.level().to_level_filter() <= self.current_level.to_level_filter(),
        }
    }

    fn log(&self, record: &Record) {
        let log_event = LogEvent {
            level: record.level(),
            target: if record.target().is_empty() {
                record.module_path().unwrap_or_default()
            } else {
                record.target()
            }
            .to_string(),
            message: record.args().to_string(),
            timestamp: match self.timestamp_format {
                TimestampFormat::Local | TimestampFormat::Offset => {
                    match OffsetDateTime::now_local() {
                        Ok(local) => local,
                        Err(_) => {
                            if !self.printed_error.load(Ordering::SeqCst) {
                                self.printed_error.store(true, Ordering::SeqCst);
                                warn!("Failed to detect Local Offset, Defaulting to UTC")
                            }
                            OffsetDateTime::now_utc()
                        }
                    }
                }
                _ => OffsetDateTime::now_utc(),
            },
        };
        if self.enabled(record.metadata()) {
            let timestamp = if self.show_timestamp {
                match self.timestamp_format {
                    TimestampFormat::Offset => log_event
                        .timestamp
                        .format(&TIMESTAMP_FORMAT_OFFSET)
                        .expect("Expected TIMESTAMP_FORMAT_OFFSET to be valid"),
                    TimestampFormat::Local => log_event
                        .timestamp
                        .format(&TIMESTAMP_FORMAT_LOCAL)
                        .expect("Expected TIMESTAMP_FORMAT_LOCAL to be valid"),
                    TimestampFormat::UTC => log_event
                        .timestamp
                        .format(&TIMESTAMP_FORMAT_UTC)
                        .expect("Expected TIMESTAMP_FORMAT_UTC to be valid"),
                    TimestampFormat::Simple => log_event
                        .timestamp
                        .format(&TIMESTAMP_FORMAT_SIMPLE)
                        .expect("Expected TIMESTAMP_FORMAT_SIMPLE to be valid"),
                    TimestampFormat::Relative => {
                        let duration = Instant::now().duration_since(self.start_instant);
                        let total_seconds = duration.as_secs();
                        let hours = total_seconds / 3600;
                        let minutes = (total_seconds % 3600) / 60;
                        let seconds = total_seconds % 60;
                        let millis = duration.subsec_millis();
                        format!("{:02}:{:02}:{:02}.{:03} ", hours, minutes, seconds, millis)
                    }
                }
            } else {
                String::new()
            };
            let level_str = format!("{:<5}", log_event.level.to_string());
            let level_prefix = if self.use_colors {
                match record.level() {
                    Level::Error => level_str.red().to_string(),
                    Level::Warn => level_str.yellow().to_string(),
                    Level::Info => level_str.cyan().to_string(),
                    Level::Debug => level_str.purple().to_string(),
                    Level::Trace => level_str.magenta().to_string(),
                }
            } else {
                level_str
            };
            let target_module = if self.show_target {
                &log_event.target
            } else {
                ""
            };
            let thread = if self.show_thread {
                let cur = std::thread::current();
                format!("({}) ", cur.name().unwrap_or(&format!("{:?}", cur.id())))
            } else {
                String::new()
            };
            println!(
                "{} {}{}[{}] {}",
                timestamp,
                level_prefix,
                thread,
                target_module,
                record.args()
            );
        }
        let _ = self.channel.send(log_event);
    }

    fn flush(&self) {}
}
