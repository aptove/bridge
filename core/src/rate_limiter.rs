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
