use std::sync::Once;

static INIT: Once = Once::new();

pub fn init_logger(level: Option<log::LevelFilter>) {
    INIT.call_once(|| {
        let log_level = level.unwrap_or_else(|| {
            std::env::var("RUST_LOG")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(log::LevelFilter::Info)
        });

        env_logger::Builder::from_default_env()
            .filter_level(log_level)
            .format_timestamp_millis()
            .format_module_path(true)
            .init();
    });
}

pub fn init_logger_with_level(level: log::LevelFilter) {
    init_logger(Some(level));
}

#[macro_export]
macro_rules! trace {
    ($($arg:tt)+) => {
        log::trace!($($arg)+)
    };
}

#[macro_export]
macro_rules! debug {
    ($($arg:tt)+) => {
        log::debug!($($arg)+)
    };
}

#[macro_export]
macro_rules! info {
    ($($arg:tt)+) => {
        log::info!($($arg)+)
    };
}

#[macro_export]
macro_rules! warn {
    ($($arg:tt)+) => {
        log::warn!($($arg)+)
    };
}

#[macro_export]
macro_rules! error {
    ($($arg:tt)+) => {
        log::error!($($arg)+)
    };
}
