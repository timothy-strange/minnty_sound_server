use chrono::{SecondsFormat, Utc};

pub fn timestamp_now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => {{
        println!("{} {}", $crate::logging::timestamp_now(), format!($($arg)*));
    }};
}

#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => {{
        eprintln!("{} {}", $crate::logging::timestamp_now(), format!($($arg)*));
    }};
}
