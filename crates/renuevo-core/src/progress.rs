//! Progreso y cancelación compartidos entre el hilo de trabajo y la interfaz.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::{Error, Result};

/// Handle barato de clonar: la GUI lo lee mientras el worker lo actualiza.
#[derive(Clone, Default, Debug)]
pub struct Progress {
    inner: Arc<Inner>,
}

#[derive(Default, Debug)]
struct Inner {
    cancelled: AtomicBool,
    done: AtomicU64,
    total: AtomicU64,
    stage: Mutex<String>,
}

impl Progress {
    pub fn new() -> Self {
        Self::default()
    }

    /// Pide cancelar el trabajo en curso; las etapas lo comprueban con [`Progress::check`].
    pub fn cancel(&self) {
        self.inner.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::Relaxed)
    }

    /// Devuelve `Err(Error::Cancelled)` si se pidió cancelar.
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }

    /// Empieza una etapa nueva con `total` unidades de trabajo.
    pub fn begin(&self, stage: impl Into<String>, total: u64) {
        *self.inner.stage.lock().unwrap_or_else(|e| e.into_inner()) = stage.into();
        self.inner.done.store(0, Ordering::Relaxed);
        self.inner.total.store(total.max(1), Ordering::Relaxed);
    }

    pub fn advance(&self, n: u64) {
        self.inner.done.fetch_add(n, Ordering::Relaxed);
    }

    /// Fracción completada de la etapa actual, entre 0 y 1.
    pub fn fraction(&self) -> f32 {
        let total = self.inner.total.load(Ordering::Relaxed).max(1);
        let done = self.inner.done.load(Ordering::Relaxed).min(total);
        done as f32 / total as f32
    }

    pub fn stage(&self) -> String {
        self.inner
            .stage
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}
