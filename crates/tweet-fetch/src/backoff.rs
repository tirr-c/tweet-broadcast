use futures_util::future::BoxFuture;

#[non_exhaustive]
pub struct Backoff {
    backoff_fn: Box<dyn FnMut(std::time::Duration) -> BoxFuture<'static, ()> + Send>,
}

impl std::fmt::Debug for Backoff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Backoff").finish_non_exhaustive()
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            backoff_fn: Box::new(Backoff::default_backoff_fn),
        }
    }
}

impl Backoff {
    fn default_backoff_fn(duration: std::time::Duration) -> BoxFuture<'static, ()> {
        log::debug!("Waiting {} ms...", duration.as_millis());
        let sleep = tokio::time::sleep(duration);
        Box::pin(sleep)
    }

    pub fn new() -> Self {
        Self::default()
    }

    pub fn backoff_fn(
        &mut self,
        f: impl FnMut(std::time::Duration) -> BoxFuture<'static, ()> + Send + 'static,
    ) {
        self.backoff_fn = Box::new(f);
    }

    pub async fn run_fn<Ret, Func, Fut>(&mut self, mut f: Func) -> Ret
    where
        Func: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<Ret, BackoffType>>,
    {
        let mut state = BackoffState::None;
        loop {
            if state.should_backoff() {
                let duration = std::time::Duration::from_millis(state.sleep_msecs());
                (self.backoff_fn)(duration).await;
            }

            match f().await {
                Ok(val) => return val,
                Err(BackoffType::Ratelimit) => state.add_ratelimit(),
                Err(BackoffType::Server) => state.add_server(),
                Err(BackoffType::Network) => state.add_network(),
            }
        }
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum BackoffType {
    Ratelimit,
    Server,
    Network,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum BackoffState {
    None,
    Ratelimit(u32),
    Server(u32),
    Network(u32),
}

impl BackoffState {
    fn sleep_msecs(&self) -> u64 {
        match *self {
            Self::None => 0,
            Self::Ratelimit(n) => {
                let mins = if n < 6 {
                    1u64 << (n.saturating_sub(2))
                } else {
                    10u64
                };
                mins * 60 * 1000
            }
            Self::Network(n) => {
                if n < 128 {
                    (n as u64) * 250
                } else {
                    32000
                }
            }
            Self::Server(n) => {
                let secs = if n < 6 {
                    1u64 << n.saturating_sub(1)
                } else {
                    60
                };
                secs * 1000
            }
        }
    }

    fn should_backoff(&self) -> bool {
        !matches!(self, Self::None)
    }

    fn add_ratelimit(&mut self) {
        match self {
            Self::Ratelimit(n) => {
                *n += 1;
            }
            _ => {
                *self = Self::Ratelimit(1);
            }
        }
    }

    fn add_network(&mut self) {
        match self {
            Self::Network(n) => {
                *n += 1;
            }
            _ => {
                *self = Self::Network(1);
            }
        }
    }

    fn add_server(&mut self) {
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
