//! Reporter bridge: turns `outh_core`'s `UploadReporter` callbacks into
//! `OuthApp` messages.
//!
//! Core calls the reporter from its own `std::thread` workers, so the
//! reporter holds a `ComponentCompletion<Message>` — the thread-safe
//! (`Send + Sync`) sender Reactor designs for exactly this handoff
//! (`ComponentContext::completion()`, see windows-reactor `component.rs`:
//! "Designed to hand to external async code/callbacks"). Each callback
//! clones the completion handle and completes it with one message; the
//! component then applies the change in `update`, on the UI thread — state
//! is only ever mutated there, per the reactor model.
//!
//! Delivery is fire-and-forget: if the component is gone or the 4,096-deep
//! message queue is full, `complete` returns `false` and the event is
//! dropped. That is acceptable for progress events (the next one follows
//! immediately); terminal state is *also* signalled by
//! `Message::UploadRunEnded`, sent by the upload thread itself when
//! `UploadManager::run` returns, so the UI can never wedge in "running".

use outh_core::types::{
    AlbumStatus, FileResult, PreflightWarning, ThreadStatus, UploadReporter,
};
use windows_reactor::ComponentCompletion;

use crate::Message;

pub struct SenderReporter {
    completion: ComponentCompletion<Message>,
}

impl SenderReporter {
    pub fn new(completion: ComponentCompletion<Message>) -> Self {
        Self { completion }
    }

    fn send(&self, message: Message) {
        // `complete` consumes the handle, so hand it a clone each time.
        let _ = self.completion.clone().complete(message);
    }
}

impl UploadReporter for SenderReporter {
    fn upload_start(&self, total_files: usize) {
        self.send(Message::UploadStarted(total_files));
    }

    fn upload_stop(&self) {
        self.send(Message::UploadStopped);
    }

    fn total_bytes(&self, total: u64) {
        self.send(Message::UploadTotalBytes(total));
    }

    fn total_bytes_delta(&self, delta: i64) {
        self.send(Message::UploadTotalBytesDelta(delta));
    }

    fn warning(&self, warning: PreflightWarning) {
        self.send(Message::UploadWarning(warning));
    }

    fn thread_status(&self, status: ThreadStatus) {
        self.send(Message::UploadThreadStatus(status));
    }

    fn file_result(&self, result: FileResult) {
        self.send(Message::UploadFileResult(result));
    }

    fn album_progress(&self, status: AlbumStatus) {
        self.send(Message::UploadAlbumProgress(status));
    }

    fn album_complete(&self, status: AlbumStatus) {
        self.send(Message::UploadAlbumComplete(status));
    }

    fn album_error(&self, name: String, message: String) {
        self.send(Message::UploadAlbumError(name, message));
    }
}
