//! 固定窗口限流。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const WINDOW: Duration = Duration::from_secs(60);

/// 每 device 的 channel 级与全局级限流器。
pub struct RateLimiter {
    per_channel: u32,
    global: u32,
    inner: Mutex<HashMap<String, Window>>,
}

#[derive(Debug, Clone, Copy)]
struct Window {
    started: Instant,
    count: u32,
}

impl RateLimiter {
    /// 创建限流器；全局上限为每 channel 上限的 4 倍。
    pub fn new(per_channel: u32) -> Self {
        Self {
            per_channel,
            global: per_channel.saturating_mul(4).max(per_channel),
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// 检查并记一次请求；超限时返回 `Retry-After` 秒数。
    pub fn check(&self, device_id: &str, channel_id: &str) -> Result<(), u64> {
        let now = Instant::now();
        let mut inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(_) => return Ok(()),
        };
        check_key(&mut inner, &format!("g:{device_id}"), self.global, now)?;
        check_key(
            &mut inner,
            &format!("c:{device_id}:{channel_id}"),
            self.per_channel,
            now,
        )?;
        Ok(())
    }
}

fn check_key(
    inner: &mut HashMap<String, Window>,
    key: &str,
    limit: u32,
    now: Instant,
) -> Result<(), u64> {
    let window = inner.entry(key.to_owned()).or_insert(Window {
        started: now,
        count: 0,
    });
    if now.duration_since(window.started) >= WINDOW {
        window.started = now;
        window.count = 0;
    }
    if window.count >= limit {
        let elapsed = now.duration_since(window.started).as_secs();
        return Err(60u64.saturating_sub(elapsed).max(1));
    }
    window.count += 1;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_channel_limit_is_enforced() {
        let limiter = RateLimiter::new(2);
        assert!(limiter.check("device", "channel").is_ok());
        assert!(limiter.check("device", "channel").is_ok());
        let retry = limiter.check("device", "channel").unwrap_err();
        assert!(retry > 0);
        assert!(limiter.check("device", "other").is_ok());
    }
}
