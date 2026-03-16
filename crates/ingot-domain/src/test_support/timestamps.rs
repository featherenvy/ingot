use chrono::{DateTime, Utc};

pub const DEFAULT_TEST_TIMESTAMP: &str = "2026-03-12T00:00:00Z";

pub fn parse_timestamp(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .expect("parse timestamp")
        .with_timezone(&Utc)
}

pub fn default_timestamp() -> DateTime<Utc> {
    parse_timestamp(DEFAULT_TEST_TIMESTAMP)
}
