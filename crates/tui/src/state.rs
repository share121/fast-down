use crate::client::ClientId;
use fast_down::file::DownloadOptions;
use fast_down::{
    ConnectErrorKind, DownloadResult, MergeProgress, ProgressEntry, Total, UrlInfo, WorkerId,
};
use reqwest::Url;
use std::cell::UnsafeCell;
use std::collections::{LinkedList, VecDeque};
use std::fmt::{Display, Formatter};
use std::io;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;
use thiserror::Error;

type Failures<T> = VecDeque<T>;

#[derive(Debug, Error)]
pub enum DownloadErrors {
    #[error("[Worker{0}] connect error: {1}")]
    Connect(WorkerId, ConnectErrorKind),
    #[error("[Worker {0}] download error: {1}")]
    Download(WorkerId, reqwest::Error),
    #[error("write error: {0}")]
    Write(WorkerId, io::Error),
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum FDWorkerState {
    None,
    Connecting,
    Downloading,
    Finished,
    Abort,
}

impl Display for FDWorkerState {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            FDWorkerState::None => write!(f, "🫧"),
            FDWorkerState::Connecting => write!(f, "⏳"),
            FDWorkerState::Downloading => write!(f, "⚡"),
            FDWorkerState::Finished => write!(f, "😎"),
            FDWorkerState::Abort => write!(f, "🛑"),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Stat(Vec<ProgressEntry>, LinkedList<(Instant, u64)>);

#[derive(Debug)]
pub struct Statistics {
    pub(crate) state: Box<[FDWorkerState]>,
    pub(crate) write_stat: Box<[Stat]>,
    pub(crate) download_stat: Box<[Stat]>,
    pub(crate) written: u64,
    pub(crate) downloaded: u64,
    pub(crate) total: u64,
}

impl Statistics {
    pub fn new(count: usize, total: u64) -> Statistics {
        Statistics {
            state: vec![FDWorkerState::None; count].into_boxed_slice(),
            write_stat: vec![Default::default(); count].into_boxed_slice(),
            download_stat: vec![Default::default(); count].into_boxed_slice(),
            written: 0,
            downloaded: 0,
            total,
        }
    }

    pub fn write_entries(&self, id: usize) -> &[ProgressEntry] {
        &self.write_stat[id].0
    }

    pub fn download_entries(&self, id: usize) -> &[ProgressEntry] {
        &self.download_stat[id].0
    }

    pub fn worker_state(&mut self, id: usize, state: FDWorkerState) {
        self.state[id] = state;
    }

    pub fn update_write(&mut self, id: usize, entry: ProgressEntry) {
        self.written += entry.total();
        let ent = &mut self.write_stat[id];
        ent.1.push_back((Instant::now(), entry.total()));
        ent.0.merge_progress(entry);
    }

    pub fn write_spans(&mut self, id: usize) -> impl Iterator<Item = &(Instant, u64)> {
        self.write_stat[id].1.iter()
    }

    pub(crate) fn purge_write_spans(&mut self, id: usize, point: &Instant) {
        let spans = &mut self.write_stat[id].1;
        while let Some((instant, _)) = spans.front() {
            if instant < point {
                spans.pop_front();
            } else { break }
        }
    }

    pub fn update_download(&mut self, id: usize, entry: ProgressEntry) {
        self.downloaded += entry.total();
        let ent = &mut self.download_stat[id];
        ent.1.push_back((Instant::now(), entry.total()));
        ent.0.merge_progress(entry);
    }

    pub fn download_spans(&mut self, id: usize) -> impl Iterator<Item = &(Instant, u64)> {
        self.download_stat[id].1.iter()
    }

    pub(crate) fn purge_download_spans(&mut self, id: usize, point: &Instant) {
        let spans = &mut self.download_stat[id].1;
        while let Some((instant, _)) = spans.front() {
            if instant < point {
                spans.pop_front();
            } else { break }
        }
    }
}

#[derive(Debug)]
pub enum TaskState {
    Pending(Failures<reqwest::Error>),
    Request(
        Option<Statistics>,
        oneshot::Receiver<Result<DownloadResult, io::Error>>,
    ),
    Download(Statistics, Failures<DownloadErrors>, DownloadResult),
    Completed,
    IoError(io::Error),
}

impl Default for TaskState {
    fn default() -> Self {
        Self::Pending(Default::default())
    }
}

#[derive(Debug)]
pub enum TaskUrlInfo {
    Pending(flume::Receiver<Result<UrlInfo, reqwest::Error>>),
    Failed,
    Ready(Rc<UrlInfo>),
}

impl TaskUrlInfo {
    /// Unwraps if `Ready`,
    /// otherwise it panics
    ///
    pub fn unwrap(&self) -> Rc<UrlInfo> {
        match self {
            TaskUrlInfo::Pending(_) => {
                panic!("called TaskUrlInfo::unwrap on a Pending TaskUrlInfo")
            }
            TaskUrlInfo::Failed => {
                panic!("called TaskUrlInfo::unwrap on a Failed TaskUrlInfo")
            }
            TaskUrlInfo::Ready(rc) => rc.clone(),
        }
    }

    /// Unwraps a `Ready` variant of TaskUrlInfo
    ///
    /// # Safety
    ///
    /// Calling this method on `Pending` or `Failed` variant is undefined behavior.
    pub unsafe fn unwrap_unchecked(&self) -> Rc<UrlInfo> {
        match self {
            TaskUrlInfo::Failed | TaskUrlInfo::Pending(_) => unsafe {
                std::hint::unreachable_unchecked()
            },
            TaskUrlInfo::Ready(rc) => rc.clone(),
        }
    }
}

impl TaskUrlInfo {
    fn pending(rx: flume::Receiver<Result<UrlInfo, reqwest::Error>>) -> TaskUrlInfo {
        TaskUrlInfo::Pending(rx)
    }
}

#[derive(Debug)]
pub struct DownloadTask {
    pub id: TaskId,
    pub client_id: ClientId,
    pub url: Arc<Url>,
    pub path: Option<PathBuf>,
    /// automatically starts the download after fetch
    pub auto: bool,
    pub(crate) download_options: Option<DownloadOptions>,
    /// `None` -> always retry
    /// `Some(count)` -> retry `count` times
    pub retry: Option<NonZeroUsize>,
    pub state: TaskState,
    pub info: TaskUrlInfo,
}

impl DownloadTask {
    pub(crate) fn state_icon(&self) -> &str {
        match self.state {
            TaskState::Pending(_) => match self.info {
                TaskUrlInfo::Pending(_) => "🔍",
                TaskUrlInfo::Failed => "🛑",
                TaskUrlInfo::Ready(_) => "🫧",
            },
            TaskState::Request(_, _) => "⏳",
            TaskState::Download(_, _, _) => "🚚",
            TaskState::Completed => "✅",
            TaskState::IoError(_) => "💥",
        }
    }
}

impl DownloadTask {
    pub fn pending(
        id: TaskId,
        client_id: ClientId,
        path: Option<PathBuf>,
        options: Option<DownloadOptions>,
        url: Url,
    ) -> (flume::Sender<Result<UrlInfo, reqwest::Error>>, DownloadTask) {
        let (tx, rx) = flume::bounded(1);
        (
            tx,
            DownloadTask {
                id,
                path,
                client_id,
                retry: None,
                url: url.into(),
                state: Default::default(),
                info: TaskUrlInfo::pending(rx),
                auto: options.is_some(),
                download_options: options,
            },
        )
    }
}

pub type TaskId = usize;
