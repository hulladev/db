//! Strict portable calendar values. Parsing uses `time`'s Gregorian calendar;
//! timezone-aware values have already been normalized to UTC by the SDK.
use time::{Date, PrimitiveDateTime, macros::format_description};

pub fn date(text: &str) -> Option<Date> {
    if text.len() != 10 || !text.is_ascii() {
        return None;
    }
    let date = Date::parse(text, format_description!("[year]-[month]-[day]")).ok()?;
    (1..=9999).contains(&date.year()).then_some(date)
}

pub fn timestamp(text: &str, precision: u8, with_timezone: bool) -> Option<PrimitiveDateTime> {
    let text = if with_timezone {
        text.strip_suffix('Z')?
    } else {
        text
    };
    if precision > 6 || !text.is_ascii() || text.len() < 19 || text.len() > 29 {
        return None;
    }
    date(text.get(..10)?)?;
    let value = if text.len() == 19 {
        PrimitiveDateTime::parse(
            text,
            format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]"),
        )
        .ok()?
    } else {
        if text.as_bytes()[19] != b'.' || text.len() - 20 > 9 || text.len() == 20 {
            return None;
        }
        PrimitiveDateTime::parse(
            text,
            format_description!(
                "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:1+]"
            ),
        )
        .ok()?
    };
    value
        .nanosecond()
        .is_multiple_of(10u32.pow(9 - u32::from(precision)))
        .then_some(value)
}

pub fn format_timestamp(value: PrimitiveDateTime, with_timezone: bool) -> Option<String> {
    if !(1..=9999).contains(&value.year()) || !value.nanosecond().is_multiple_of(1000) {
        return None;
    }
    let mut text = value
        .format(format_description!(
            "[year]-[month]-[day]T[hour]:[minute]:[second]"
        ))
        .ok()?;
    if value.nanosecond() != 0 {
        let fraction = format!("{:06}", value.microsecond());
        text.push('.');
        text.push_str(fraction.trim_end_matches('0'));
    }
    if with_timezone {
        text.push('Z');
    }
    Some(text)
}
