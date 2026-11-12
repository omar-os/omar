//! Provider limits are waits, not failed attempts. Only error payloads (or a
//! backend's explicit limit screen) reach this parser; ordinary model text does not.
use chrono::{DateTime, Local, NaiveTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error)]
#[error("provider usage limit reached")]
pub struct UsageLimit {
    pub resets_at: Option<i64>,
}

impl UsageLimit {
    pub fn delay(&self, fallback: Duration) -> Duration {
        self.resets_at
            .and_then(|reset| {
                let seconds = reset - Utc::now().timestamp();
                (seconds > 0).then(|| Duration::from_secs(seconds as u64))
            })
            .unwrap_or(fallback)
    }

    pub fn from_error(value: &Value) -> Option<Self> {
        let text = value.to_string().to_lowercase();
        if ![
            "usage_limit_reached",
            "usagelimitreached",
            "usagelimitexceeded",
            "usage limit",
            "rate_limit",
            "rate limit",
            "too many requests",
            "resource_exhausted",
            "quota exceeded",
            "insufficient_quota",
            "quota_exceeded",
            "exhausted your capacity",
            "you've hit your limit",
            "you’ve hit your limit",
        ]
        .iter()
        .any(|signal| text.contains(signal))
            && !contains_status(value, 429)
        {
            return None;
        }
        Some(Self {
            resets_at: reset_time(value).or_else(|| text_reset(&text)),
        })
    }
}

fn contains_status(value: &Value, status: i64) -> bool {
    match value {
        Value::Object(fields) => fields.iter().any(|(key, value)| {
            matches!(
                key.as_str(),
                "status" | "statusCode" | "httpStatusCode" | "code"
            ) && value.as_i64() == Some(status)
                || contains_status(value, status)
        }),
        Value::Array(items) => items.iter().any(|item| contains_status(item, status)),
        _ => false,
    }
}

fn timestamp(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str()?.parse().ok())
        .map(|n| if n > 10_000_000_000 { n / 1000 } else { n })
        .or_else(|| {
            DateTime::parse_from_rfc3339(value.as_str()?)
                .ok()
                .map(|date| date.timestamp())
        })
}

fn reset_time(value: &Value) -> Option<i64> {
    match value {
        Value::Object(fields) => {
            for key in [
                "resets_at",
                "resetsAt",
                "reset_at",
                "resetAt",
                "x-ratelimit-reset",
                "x-ratelimit-reset-requests",
            ] {
                if let Some(time) = fields.get(key).and_then(timestamp) {
                    return Some(time);
                }
            }
            for key in [
                "retry-after",
                "retry_after",
                "retryAfter",
                "resets_in_seconds",
            ] {
                if let Some(value) = fields.get(key) {
                    if let Some(seconds) = value.as_u64().or_else(|| value.as_str()?.parse().ok()) {
                        return Some(
                            Utc::now()
                                .timestamp()
                                .saturating_add(seconds.min(i64::MAX as u64) as i64),
                        );
                    }
                    if let Some(date) = value
                        .as_str()
                        .and_then(|s| DateTime::parse_from_rfc2822(s).ok())
                    {
                        return Some(date.timestamp());
                    }
                }
            }
            // Google providers expose google.rpc.RetryInfo as a protobuf
            // duration, often nested under error.details[].retryDelay.
            if let Some(delay) = fields
                .get("retryDelay")
                .and_then(Value::as_str)
                .and_then(|s| s.strip_suffix('s'))
                .and_then(|s| s.parse::<f64>().ok())
                .filter(|n| n.is_finite() && *n >= 0.0)
            {
                return Some(Utc::now().timestamp().saturating_add(delay.ceil() as i64));
            }
            fields.values().find_map(reset_time)
        }
        Value::Array(items) => items.iter().find_map(reset_time),
        Value::String(text) => serde_json::from_str::<Value>(text)
            .ok()
            .and_then(|v| reset_time(&v))
            .or_else(|| text_reset(text)),
        _ => None,
    }
}

// Claude's notice uses local wall time. Also accept provider JSON embedded in
// an error message (Pi/OpenCode preserve the upstream response body there).
fn text_reset(text: &str) -> Option<i64> {
    let epoch = regex::Regex::new(r#"(?:resets_at|resetsAt)[\\"\s:]+(\d{10,13})"#).unwrap();
    if let Some(capture) = epoch.captures(text) {
        return timestamp(&Value::String(capture[1].into()));
    }
    let iso =
        regex::Regex::new(r"\d{4}-\d\d-\d\d[Tt]\d\d:\d\d:\d\d(?:\.\d+)?(?:[Zz]|[+-]\d\d:\d\d)")
            .unwrap();
    if let Some(time) = iso
        .find(text)
        .and_then(|m| DateTime::parse_from_rfc3339(m.as_str()).ok())
    {
        return Some(time.timestamp());
    }
    let clock = regex::Regex::new(r"(?i)(?:automatically at|try again at|resets? (?:at )?)\s*(\d{1,2})(?::(\d\d))?(?::(\d\d))?\s*(am|pm)?").unwrap();
    let captures = clock.captures(text)?;
    let mut hour: u32 = captures[1].parse().ok()?;
    if let Some(period) = captures.get(4) {
        hour = hour % 12
            + if period.as_str().eq_ignore_ascii_case("pm") {
                12
            } else {
                0
            };
    }
    let minute = captures
        .get(2)
        .map_or(Some(0), |m| m.as_str().parse().ok())?;
    let now = Local::now();
    let time = NaiveTime::from_hms_opt(
        hour,
        minute,
        captures
            .get(3)
            .map_or(Some(0), |m| m.as_str().parse().ok())?,
    )?;
    let mut date = now.date_naive();
    if time < now.time() && (now.time() - time).num_seconds() >= 60 {
        date = date.succ_opt()?;
    }
    Local
        .from_local_datetime(&date.and_time(time))
        .earliest()
        .map(|d| d.timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn retry_metadata_and_claude_wall_time_are_recognized() {
        let before = Utc::now().timestamp();
        for error in [
            json!({"statusCode":429,"responseHeaders":{"retry-after":"8"}}),
            json!({"code":429,"details":[{"retryDelay":"8s"}]}),
        ] {
            let reset = UsageLimit::from_error(&error).unwrap().resets_at.unwrap();
            assert!((before + 8..=Utc::now().timestamp() + 8).contains(&reset));
        }
        let reset = Local::now() + chrono::Duration::minutes(2);
        let screen = format!(
            "Usage limit reached. Continuing automatically at {}",
            reset.format("%I:%M:%S %p")
        );
        assert_eq!(
            UsageLimit::from_error(&json!(screen)).unwrap().resets_at,
            Some(reset.timestamp())
        );
    }

    #[test]
    fn native_errors_keep_the_reset_and_ordinary_failures_are_not_limits() {
        for value in [
            json!({"codexErrorInfo":"UsageLimitReached", "resets_at":1900000000}),
            json!({"error":{"type":"usage_limit_reached", "resets_at":1900000000}}),
            json!({"name":"APIError","data":{"statusCode":429,"responseBody":"{\"error\":{\"resets_at\":1900000000}}"}}),
            json!({"errorMessage":"429 rate limit: {\"resets_at\":1900000000}"}),
        ] {
            assert_eq!(
                UsageLimit::from_error(&value).unwrap().resets_at,
                Some(1900000000),
                "{value}"
            );
        }
        assert!(UsageLimit::from_error(&json!({"message":"service unavailable"})).is_none());
        assert!(
            UsageLimit::from_error(&json!({"message":"rate limit exceeded"}))
                .unwrap()
                .resets_at
                .is_none()
        );
    }
}
