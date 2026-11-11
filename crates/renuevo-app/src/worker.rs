//! Trabajos en segundo plano: la interfaz nunca se bloquea mientras se procesa.

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use renuevo_core::Progress;

pub enum Poll<T> {
    Pending,
    Done(T),
    /// El hilo terminó sin enviar resultado (pánico).
    Crashed,
}

pub struct Job<T> {
    pub progress: Progress,
    pub label: String,
    /// Si la interfaz debe ofrecer «Cancelar» (exportar no se interrumpe a medias).
    pub cancellable: bool,
    started: Instant,
    rx: Receiver<T>,
}

impl<T: Send + 'static> Job<T> {
    pub fn spawn(
        ctx: &egui::Context,
        label: impl Into<String>,
        work: impl FnOnce(&Progress) -> T + Send + 'static,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let progress = Progress::new();
        let worker_progress = progress.clone();
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name("renuevo-worker".to_owned())
            .spawn(move || {
                let out = work(&worker_progress);
                let _ = tx.send(out);
                ctx.request_repaint();
            })
            .expect("no se pudo crear el hilo de trabajo");
        Self {
            progress,
            label: label.into(),
            cancellable: true,
            started: Instant::now(),
            rx,
        }
    }

    pub fn not_cancellable(mut self) -> Self {
        self.cancellable = false;
        self
    }

    pub fn poll(&self) -> Poll<T> {
        match self.rx.try_recv() {
            Ok(v) => Poll::Done(v),
            Err(TryRecvError::Empty) => Poll::Pending,
            Err(TryRecvError::Disconnected) => Poll::Crashed,
        }
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    pub fn cancel(&self) {
        self.progress.cancel();
    }
}
