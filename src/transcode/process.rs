use axum::body::Bytes;
use futures_core::Stream;
use std::{
    path::Path,
    pin::Pin,
    process::Stdio,
    task::{Context, Poll},
};
use tokio::{
    process::{Child, ChildStdout, Command},
    sync::OwnedSemaphorePermit,
};
use tokio_util::io::ReaderStream;

use super::{Settings, SETTINGS, SLOTS, TranscodeParams, build_args};


#[derive(Debug)]
pub enum SpawnError {
    Unavailable,
    Busy,
    Io(String),
}

pub(super) fn settings() -> Result<&'static Settings, SpawnError> {
    match SETTINGS.get() {
        Some(s) if s.available => Ok(s),
        _ => Err(SpawnError::Unavailable),
    }
}

pub(super) fn acquire_slot() -> Result<OwnedSemaphorePermit, SpawnError> {
    SLOTS
        .get()
        .ok_or(SpawnError::Unavailable)?
        .clone()
        .try_acquire_owned()
        .map_err(|_| SpawnError::Busy)
}

/// Saída do ffmpeg como stream HTTP. Ao descartar (cliente desconectou) o processo é morto e
/// a vaga do semáforo é liberada.
pub struct ProcessStream {
    inner: ReaderStream<ChildStdout>,
    _child: Child,
    _permit: OwnedSemaphorePermit,
}

impl Stream for ProcessStream {
    type Item = std::io::Result<Bytes>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_next(cx)
    }
}

pub fn spawn_progressive(input: &Path, p: &TranscodeParams) -> Result<ProcessStream, SpawnError> {
    let s = settings()?;
    let permit = acquire_slot()?;
    let args = build_args(&s.encoder, input, p, None);
    let mut child = Command::new(&s.ffmpeg)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| SpawnError::Io(e.to_string()))?;
    let stdout = child.stdout.take().ok_or_else(|| SpawnError::Io("sem stdout".into()))?;
    Ok(ProcessStream {
        inner: ReaderStream::with_capacity(stdout, 256 * 1024),
        _child: child,
        _permit: permit,
    })
}
