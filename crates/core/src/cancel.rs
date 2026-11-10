use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;

#[derive(Clone, Default)]
pub struct CancelHandle {
    inner: Arc<Mutex<CancellationToken>>,
}

impl CancelHandle {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn token(&self) -> CancellationToken {
        self.inner.lock().expect("cancel lock").clone()
    }

    pub fn reset(&self) {
        *self.inner.lock().expect("cancel lock") = CancellationToken::new();
    }

    pub fn cancel(&self) {
        self.inner.lock().expect("cancel lock").cancel();
    }
}
