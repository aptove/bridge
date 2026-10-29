use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// Simple rate limiter to prevent abuse
pub struct RateLimiter {
    /// Maximum concurrent connections per IP
    max_connections_per_ip: usize,
    /// Maximum connection attempts per minute per IP
    max_attempts_per_minute: usize,
    /// Current connection counts per IP
    connections: Arc<Mutex<HashMap<IpAddr, usize>>>,
    /// Recent connection attempts per IP (timestamp of each attempt)
    attempts: Arc<Mutex<HashMap<IpAddr, Vec<Instant>>>>,
}

impl RateLimiter {
    pub fn new(max_connections_per_ip: usize, max_attempts_per_minute: usize) -> Self {
        Self {
            max_connections_per_ip,
            max_attempts_per_minute,
            connections: Arc::new(Mutex::new(HashMap::new())),
            attempts: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Check if a new connection is allowed from this IP
    /// Returns Ok(()) if allowed, Err with reason if denied
    pub async fn check_connection(&self, ip: IpAddr) -> Result<(), RateLimitError> {
        // Check rate limit (attempts per minute)
        {
            let mut attempts = self.attempts.lock().await;
            let now = Instant::now();
            let minute_ago = now - Duration::from_secs(60);
            
            // Get or create attempt list for this IP
            let ip_attempts = attempts.entry(ip).or_insert_with(Vec::new);
            
            // Remove old attempts (older than 1 minute)
            ip_attempts.retain(|t| *t > minute_ago);
            
            // Check if we've exceeded the rate limit
            if ip_attempts.len() >= self.max_attempts_per_minute {
                return Err(RateLimitError::TooManyAttempts {
                    attempts: ip_attempts.len(),
                    max: self.max_attempts_per_minute,
                });
            }
            
            // Record this attempt
            ip_attempts.push(now);
        }

        // Check concurrent connection limit
        {
            let connections = self.connections.lock().await;
            if let Some(&count) = connections.get(&ip) {
                if count >= self.max_connections_per_ip {
                    return Err(RateLimitError::TooManyConnections {
                        current: count,
                        max: self.max_connections_per_ip,
                    });
                }
            }
        }

        Ok(())
    }

    /// Register a new active connection from this IP
    pub async fn add_connection(&self, ip: IpAddr) {
        let mut connections = self.connections.lock().await;
        *connections.entry(ip).or_insert(0) += 1;
    }

    /// Remove an active connection from this IP
    pub async fn remove_connection(&self, ip: IpAddr) {
        let mut connections = self.connections.lock().await;
        if let Some(count) = connections.get_mut(&ip) {
            if *count > 0 {
                *count -= 1;
            }
            if *count == 0 {
                connections.remove(&ip);
            }
        }
    }
}

#[derive(Debug)]
pub enum RateLimitError {
    TooManyConnections { current: usize, max: usize },
    TooManyAttempts { attempts: usize, max: usize },
}

impl std::fmt::Display for RateLimitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RateLimitError::TooManyConnections { current, max } => {
                write!(f, "Too many concurrent connections ({}/{})", current, max)
            }
            RateLimitError::TooManyAttempts { attempts, max } => {
                write!(f, "Too many connection attempts ({}/{} per minute)", attempts, max)
            }
        }
    }
}

impl std::error::Error for RateLimitError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::IpAddr;

    fn ip(s: &str) -> IpAddr { s.parse().unwrap() }

    #[tokio::test]
    async fn allows_first_connection() {
        let rl = RateLimiter::new(5, 10);
        assert!(rl.check_connection(ip("127.0.0.1")).await.is_ok());
    }

    #[tokio::test]
    async fn blocks_after_too_many_attempts() {
        let rl = RateLimiter::new(10, 3);
        let addr = ip("10.0.0.1");
        for _ in 0..3 {
            rl.check_connection(addr).await.unwrap();
        }
        let result = rl.check_connection(addr).await;
        assert!(
            matches!(result, Err(RateLimitError::TooManyAttempts { .. })),
            "Expected TooManyAttempts after exceeding per-minute limit"
        );
    }

    #[tokio::test]
    async fn blocks_when_concurrent_connections_at_limit() {
        let rl = RateLimiter::new(2, 100);
        let addr = ip("10.0.0.2");
        rl.add_connection(addr).await;
        rl.add_connection(addr).await;
        let result = rl.check_connection(addr).await;
        assert!(
            matches!(result, Err(RateLimitError::TooManyConnections { .. })),
            "Expected TooManyConnections when concurrent limit reached"
        );
    }

    #[tokio::test]
    async fn remove_connection_frees_slot() {
        let rl = RateLimiter::new(1, 100);
        let addr = ip("10.0.0.3");
        rl.add_connection(addr).await;
        rl.remove_connection(addr).await;
        // Slot freed — a new connection check should pass.
        assert!(rl.check_connection(addr).await.is_ok());
    }

    #[tokio::test]
    async fn different_ips_are_tracked_independently() {
        let rl = RateLimiter::new(1, 3);
        let a = ip("192.168.1.1");
        let b = ip("192.168.1.2");
        rl.add_connection(a).await;
        // IP b is not affected by IP a's active connections.
        assert!(
            rl.check_connection(b).await.is_ok(),
            "Connection from a different IP should be independent"
        );
    }

    #[tokio::test]
    async fn remove_nonexistent_connection_is_noop() {
        let rl = RateLimiter::new(5, 10);
        let addr = ip("10.1.1.1");
        // Should not panic or error.
        rl.remove_connection(addr).await;
        assert!(rl.check_connection(addr).await.is_ok());
    }
}
