//! 固定窗口限流。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const WINDOW: Duration = Duration::from_secs(60);
const SWEEP_INTERVAL: Duration = Duration::from_secs(60);
const MAX_IN_FLIGHT: u32 = 4;
const ENROLL_CODE_PER_MIN: u32 = 10;
const ENROLL_IP_PER_MIN: u32 = 30;

/// 每 device 的 channel 级与全局级限流器。
pub struct RateLimiter {
    per_channel: u32,
    global: u32,
    inner: Mutex<HashMap<String, Window>>,
    in_flight: Mutex<HashMap<String, u32>>,
    last_sweep: Mutex<Instant>,
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
            in_flight: Mutex::new(HashMap::new()),
            last_sweep: Mutex::new(Instant::now()),
        }
    }

    /// 检查并记一次请求；超限时返回 `Retry-After` 秒数。
    pub fn check(&self, device_id: &str, channel_id: &str) -> Result<(), u64> {
        let now = Instant::now();
        self.maybe_sweep(now);
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

    /// `/v1/enroll` 的 code/IP 限流。
    pub fn check_enroll(&self, code_hash: &str, ip: Option<&str>) -> Result<(), u64> {
        let now = Instant::now();
        self.maybe_sweep(now);
        let mut inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(_) => return Ok(()),
        };
        check_key(
            &mut inner,
            &format!("e:code:{code_hash}"),
            ENROLL_CODE_PER_MIN,
            now,
        )?;
        if let Some(ip) = ip {
            check_key(&mut inner, &format!("e:ip:{ip}"), ENROLL_IP_PER_MIN, now)?;
        }
        Ok(())
    }

    /// 获取一个 per-token 并发 permit；上限 4。
    pub fn acquire(self: &Arc<Self>, device_id: &str) -> Result<RatePermit, u64> {
        self.maybe_sweep(Instant::now());
        let mut in_flight = match self.in_flight.lock() {
            Ok(guard) => guard,
            Err(_) => return Err(1),
        };
        let count = in_flight.entry(device_id.to_owned()).or_insert(0);
        if *count >= MAX_IN_FLIGHT {
            return Err(1);
        }
        *count += 1;
        Ok(RatePermit {
            limiter: Arc::clone(self),
            device_id: device_id.to_owned(),
        })
    }

    fn maybe_sweep(&self, now: Instant) {
        let mut last = match self.last_sweep.lock() {
            Ok(guard) => guard,
            Err(_) => return,
        };
        if now.duration_since(*last) < SWEEP_INTERVAL {
            return;
        }
        *last = now;
        if let Ok(mut inner) = self.inner.lock() {
            inner.retain(|_, window| now.duration_since(window.started) < WINDOW.saturating_mul(2));
        }
    }
}

/// 持有期间占用一个 per-token 并发槽位。
pub struct RatePermit {
    limiter: Arc<RateLimiter>,
    device_id: String,
}

impl Drop for RatePermit {
    fn drop(&mut self) {
        let mut in_flight = match self.limiter.in_flight.lock() {
            Ok(guard) => guard,
            Err(_) => return,
        };
        let remove = if let Some(count) = in_flight.get_mut(&self.device_id) {
            *count = count.saturating_sub(1);
            *count == 0
        } else {
            false
        };
        if remove {
            in_flight.remove(&self.device_id);
        }
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

    #[test]
    fn in_flight_limit_is_enforced() {
        let limiter = Arc::new(RateLimiter::new(60));
        let mut permits = Vec::new();
        for _ in 0..MAX_IN_FLIGHT {
            permits.push(limiter.acquire("device").unwrap());
        }
        assert!(limiter.acquire("device").is_err());
        permits.pop();
        assert!(limiter.acquire("device").is_ok());
    }

    #[test]
    fn enroll_code_and_ip_limits_are_enforced() {
        let limiter = RateLimiter::new(60);
        for _ in 0..ENROLL_CODE_PER_MIN {
            assert!(limiter.check_enroll("code", Some("203.0.113.1")).is_ok());
        }
        assert!(limiter.check_enroll("code", Some("203.0.113.1")).is_err());

        let limiter = RateLimiter::new(60);
        for index in 0..ENROLL_IP_PER_MIN {
            assert!(limiter
                .check_enroll(&format!("code-{index}"), Some("203.0.113.2"))
                .is_ok());
        }
        assert!(limiter
            .check_enroll("another-code", Some("203.0.113.2"))
            .is_err());
    }

    #[test]
    fn enroll_without_ip_only_applies_code_limit() {
        let limiter = RateLimiter::new(60);
        for _ in 0..ENROLL_CODE_PER_MIN {
            assert!(limiter.check_enroll("code-none", None).is_ok());
        }
        assert!(limiter.check_enroll("code-none", None).is_err());
    }

    #[test]
    fn sweep_removes_stale_windows() {
        let limiter = RateLimiter::new(2);
        limiter.check("device", "channel").unwrap();
        let stale = Instant::now() - WINDOW.saturating_mul(2) - Duration::from_secs(1);
        {
            let mut inner = limiter.inner.lock().unwrap();
            for window in inner.values_mut() {
                window.started = stale;
            }
        }
        *limiter.last_sweep.lock().unwrap() =
            Instant::now() - SWEEP_INTERVAL - Duration::from_secs(1);
        limiter.check("device", "other").unwrap();
        assert!(!limiter
            .inner
            .lock()
            .unwrap()
            .contains_key("c:device:channel"));
    }
}
