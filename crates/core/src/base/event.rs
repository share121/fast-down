use crate::ProgressEntry;

pub type WorkerId = usize;

#[derive(thiserror::Error, Debug)]
pub enum ConnectErrorKind {
    #[error("Reqwest error: {0}")]
    Reqwest(#[from] reqwest::Error),
    /// The server returned a non-206 status code when use Range header.
    #[error("Server returned non-206 status code: {0}")]
    StatusCode(reqwest::StatusCode),
}

/// ### Download Event
///
/// for `single` downloader, WorkerId is always `0`
#[derive(Debug)]
pub enum Event {
    /// connecting to remote
    Connecting(WorkerId),
    /// error while connecting
    ConnectError(WorkerId, ConnectErrorKind),
    /// connected, start downloading
    Downloading(WorkerId),
    /// error while downloading
    DownloadError(WorkerId, reqwest::Error),
    /// download progress
    DownloadProgress(WorkerId, ProgressEntry),
    /// error writing
    WriteError(WorkerId, std::io::Error),
    /// write progress
    WriteProgress(WorkerId, ProgressEntry),
    /// worker finished given task
    Finished(WorkerId),
    /// worker aborted
    Abort(WorkerId),
}
