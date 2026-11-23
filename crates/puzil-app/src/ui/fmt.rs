//! Dates and times in the local time zone.

use chrono::{DateTime, Datelike, Local, TimeZone};

fn local(ts: i64) -> DateTime<Local> {
    Local.timestamp_opt(ts, 0).single().unwrap_or_else(Local::now)
}

/// `3:04 PM`
pub fn time(ts: i64) -> String {
    local(ts).format(if crate::settings::clock_24h() { "%H:%M" } else { "%-I:%M %p" }).to_string()
}

/// `Today`, `Yesterday`, `Friday, November 15` (this year), or with the year.
pub fn day(ts: i64) -> String {
    let d = local(ts).date_naive();
    let today = Local::now().date_naive();
    match (today - d).num_days() {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        _ if d.year() == today.year() => d.format("%A, %B %-d").to_string(),
        _ => d.format("%A, %B %-d, %Y").to_string(),
    }
}

pub fn same_day(a: i64, b: i64) -> bool {
    local(a).date_naive() == local(b).date_naive()
}

/// Discord's message stamp: `3:04 PM` today, `Yesterday at 3:04 PM`, else `11/15/2026 3:04 PM`.
pub fn datetime(ts: i64) -> String {
    let d = local(ts);
    match (Local::now().date_naive() - d.date_naive()).num_days() {
        0 => time(ts),
        1 => format!("Yesterday at {}", time(ts)),
        _ => format!("{} {}", d.format("%-m/%-d/%Y"), time(ts)),
    }
}
