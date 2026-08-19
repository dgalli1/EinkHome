//! Download queue + worker thread (C eh_downloads.c).
//!
//! The e2e suite requires downloads to run OFF the UI thread: a slow file
//! must not freeze the event loop (the top-bar sync glyph keeps animating,
//! the framebuffer changes mid-fetch), and the progress popup must be modal.
//!
//! A single worker thread consumes the queue and writes each file to disk,
//! reporting completion over an mpsc channel the app drains on its next
//! event.  The thread reconstructs its own [`ApiClient`] from the job's
//! base/token so the worker never shares the app's client.

use std::sync::mpsc;

/// One queued file fetch.
struct Job {
    base: String,
    token: String,
    id: String,
    path: String,
}

/// One completed (or failed) fetch.
pub struct Done {
    pub id: String,
    pub path: String,
    pub ok: bool,
}

/// The app-side handle: enqueue jobs, drain completions.
pub struct Downloader {
    tx: mpsc::Sender<Job>,
    rx: mpsc::Receiver<Done>,
    /// Books still queued or in flight.
    pub pending: usize,
}

impl Downloader {
    pub fn new() -> Self {
        let (tx, jrx) = mpsc::channel::<Job>();
        let (dtx, rx) = mpsc::channel::<Done>();
        std::thread::spawn(move || {
            crate::log("[eh_app] dl worker: started");
            for job in jrx {
                crate::log(&format!("[eh_app] dl worker: fetch id={}", job.id));
                let client = crate::client::ApiClient::new(&job.base, &job.token);
                let ok = match client.file(&job.id) {
                    Ok(bytes) => {
                        crate::log(&format!("[eh_app] dl worker: got {} bytes", bytes.len()));
                        std::fs::write(&job.path, &bytes).is_ok()
                    }
                    Err(e) => {
                        crate::log(&format!("[eh_app] download worker FAILED id={}: {e}", job.id));
                        false
                    }
                };
                crate::log(&format!("[eh_app] dl worker: write ok={ok}"));
                let _ = dtx.send(Done { id: job.id, path: job.path, ok });
            }
            crate::log("[eh_app] dl worker: exiting");
        });
        Self { tx, rx, pending: 0 }
    }

    /// Queue a book file fetch.  The client is captured by value (base +
    /// token) so the worker owns a private client for its lifetime.
    pub fn enqueue(&mut self, base: &str, token: &str, id: &str, path: &str) {
        if self.tx
            .send(Job {
                base: base.to_string(),
                token: token.to_string(),
                id: id.to_string(),
                path: path.to_string(),
            })
            .is_ok()
        {
            self.pending += 1;
        }
    }

    /// Drain one completed fetch, if any.
    pub fn try_next(&self) -> Option<Done> {
        self.rx.try_recv().ok()
    }
}

impl Default for Downloader {
    fn default() -> Self {
        Self::new()
    }
}