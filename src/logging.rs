use chrono::{SecondsFormat, Utc};

pub fn timestamp_now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub fn info_enabled() -> bool {
    cfg!(debug_assertions) || std::env::var_os("MINNTY_VERBOSE").is_some()
}

#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => {{
        if $crate::logging::info_enabled() {
            println!("{} {}", $crate::logging::timestamp_now(), format!($($arg)*));
        }
    }};
}

#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => {{
        eprintln!("{} {}", $crate::logging::timestamp_now(), format!($($arg)*));
    }};
}
