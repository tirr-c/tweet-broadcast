#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum BackoffType {
    None,
    Ratelimit(u32),
    Server(u32),
    Network(u32),
}

impl BackoffType {
    pub fn sleep_msecs(&self) -> u64 {
        match self {
            &Self::None => 0,
            &Self::Ratelimit(n) => {
                let mins = if n < 6 {
                    1u64 << (n.saturating_sub(2))
                } else {
                    10u64
                };
                mins * 60 * 1000
            }
            &Self::Network(n) => {
                if n < 128 {
                    (n as u64) * 250
                } else {
                    32000
                }
            }
            &Self::Server(n) => {
                let secs = if n < 6 {
                    1u64 << n.saturating_sub(1)
                } else {
                    60
                };
                secs * 1000
            }
        }
    }

    pub fn should_backoff(&self) -> bool {
        !matches!(self, Self::None)
    }

    pub fn add_ratelimit(&mut self) {
        match self {
            Self::Ratelimit(n) => {
                *n += 1;
            }
            _ => {
                *self = Self::Ratelimit(1);
            }
        }
    }

    pub fn add_network(&mut self) {
        match self {
            Self::Network(n) => {
                *n += 1;
            }
            _ => {
                *self = Self::Network(1);
            }
        }
    }

    pub fn add_server(&mut self) {
        match self {
            Self::Server(n) => {
                *n += 1;
            }
            _ => {
                *self = Self::Server(1);
            }
        }
    }
}
