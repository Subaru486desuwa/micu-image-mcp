use std::time::{Duration, SystemTime};

use reqwest::header::HeaderMap;

pub const MAX_RETRY_AFTER: Duration = Duration::from_secs(120);
pub const NETWORK_RETRY_DELAY: Duration = Duration::from_secs(2);
pub const SMALL_RETRY_DELAYS: [Duration; 2] = [Duration::from_secs(4), Duration::from_secs(8)];
pub const BIG_RETRY_DELAY: Duration = Duration::from_secs(60);
pub const RETRY_JITTER_MAX: Duration = Duration::from_secs(2);

pub fn parse_retry_after(_headers: &HeaderMap, _now: SystemTime) -> Option<Duration> {
    let value = _headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim();
    if let Ok(seconds) = value.parse::<f64>() {
        if !seconds.is_finite() {
            return None;
        }
        if seconds <= 0.0 {
            return Some(Duration::ZERO);
        }
        return Some(Duration::from_secs_f64(seconds).min(MAX_RETRY_AFTER));
    }
    let date = httpdate::parse_http_date(value).ok()?;
    Some(
        date.duration_since(_now)
            .unwrap_or(Duration::ZERO)
            .min(MAX_RETRY_AFTER),
    )
}

pub fn effective_retry_status(status: u16, detail: &str) -> u16 {
    if status != 400 {
        return status;
    }
    let normalized = detail.trim().to_ascii_lowercase();
    if ["too many requests", "rate limit", "rate_limit", "ratelimit"]
        .iter()
        .any(|marker| normalized.contains(marker))
    {
        429
    } else {
        status
    }
}

pub fn retry_delay(
    status: u16,
    headers: &HeaderMap,
    attempt_index: usize,
    big_size: bool,
    now: SystemTime,
    jitter: Duration,
) -> Option<Duration> {
    if !retryable_status(status) || (big_size && status == 524) {
        return None;
    }
    if (big_size && attempt_index >= 1) || (!big_size && attempt_index >= SMALL_RETRY_DELAYS.len())
    {
        return None;
    }
    if matches!(status, 408 | 409 | 425 | 429 | 500 | 502 | 503 | 504)
        && let Some(delay) = parse_retry_after(headers, now)
    {
        return Some(delay);
    }
    if big_size {
        Some(BIG_RETRY_DELAY)
    } else {
        Some(SMALL_RETRY_DELAYS[attempt_index] + jitter.min(RETRY_JITTER_MAX))
    }
}

pub fn retryable_status(status: u16) -> bool {
    matches!(
        status,
        0 | 408 | 409 | 425 | 429 | 500 | 502 | 503 | 504 | 520 | 521 | 522 | 523 | 524 | 525 | 527
    )
}

#[cfg(test)]
mod tests {
    use reqwest::header::{HeaderMap, HeaderValue, RETRY_AFTER};

    use super::*;

    fn headers(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            RETRY_AFTER,
            HeaderValue::from_str(value).unwrap_or_else(|error| panic!("{error}")),
        );
        headers
    }

    #[test]
    fn retry_after_supports_seconds_dates_and_clamps() {
        let epoch = SystemTime::UNIX_EPOCH;
        assert_eq!(
            parse_retry_after(&headers("5"), epoch),
            Some(Duration::from_secs(5))
        );
        assert_eq!(
            parse_retry_after(&headers("0.5"), epoch),
            Some(Duration::from_millis(500))
        );
        assert_eq!(
            parse_retry_after(&headers("9999"), epoch),
            Some(MAX_RETRY_AFTER)
        );
        assert_eq!(
            parse_retry_after(&headers("Thu, 01 Jan 1970 00:00:30 GMT"), epoch),
            Some(Duration::from_secs(30))
        );
        assert_eq!(
            parse_retry_after(&headers("Thu, 01 Jan 1970 00:00:00 GMT"), epoch),
            Some(Duration::ZERO)
        );
        assert_eq!(parse_retry_after(&headers("not a date"), epoch), None);
    }

    #[test]
    fn retry_schedule_matches_small_big_and_524_contracts() {
        let empty = HeaderMap::new();
        let now = SystemTime::UNIX_EPOCH;
        assert_eq!(
            retry_delay(500, &empty, 0, false, now, Duration::from_millis(250)),
            Some(Duration::from_millis(4_250))
        );
        assert_eq!(
            retry_delay(500, &empty, 1, false, now, Duration::from_millis(250)),
            Some(Duration::from_millis(8_250))
        );
        assert_eq!(
            retry_delay(500, &empty, 2, false, now, Duration::ZERO),
            None
        );
        assert_eq!(
            retry_delay(503, &empty, 0, true, now, Duration::ZERO),
            Some(BIG_RETRY_DELAY)
        );
        assert_eq!(retry_delay(503, &empty, 1, true, now, Duration::ZERO), None);
        assert_eq!(retry_delay(524, &empty, 0, true, now, Duration::ZERO), None);
        assert!(retry_delay(524, &empty, 0, false, now, Duration::ZERO).is_some());
    }

    #[test]
    fn proxy_400_rate_limit_is_normalized_without_normalizing_other_400s() {
        assert_eq!(effective_retry_status(400, "Too Many Requests"), 429);
        assert_eq!(effective_retry_status(400, "rate_limit_exceeded"), 429);
        assert_eq!(effective_retry_status(400, "invalid size"), 400);
        assert_eq!(effective_retry_status(503, "Too Many Requests"), 503);
    }
}
