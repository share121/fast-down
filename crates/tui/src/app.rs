use crate::client::{Client, ClientId};
use crate::common::ClientOptions;
use crate::state::{
    DownloadErrors, DownloadTask, FDWorkerState, Statistics, TaskId, TaskState, TaskUrlInfo,
};
use crate::{render, worker};
use arboard::Clipboard;
use crossterm::event as term;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use fast_down::file::DownloadOptions;
use fast_down::{Event, UrlInfo};
use ratatui::DefaultTerminal;
use ratatui::prelude::*;
use reqwest::Url;
use smallvec::SmallVec;
use std::collections::BTreeMap;
use std::io;
use std::num::NonZeroUsize;
use std::ops::Bound::Excluded;
use std::ops::Bound::Unbounded;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

#[derive(Debug, PartialEq, Eq)]
pub enum Page {
    Main,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AppFocus {
    Page(Page),
    Overlay,
}

impl Default for AppFocus {
    fn default() -> Self {
        use self::Page::*;

        Self::Page(Main)
    }
}

pub struct App {
    pub exit: bool,

    pub(crate) states: type_map::TypeMap,
    pub(crate) capture: Option<Box<dyn FnMut(KeyEvent)>>,
    pub(crate) focus: AppFocus,
    pub(crate) scroll: Option<usize>,
    pub(crate) selected: Option<TaskId>,
    pub(crate) tasks: BTreeMap<TaskId, DownloadTask>,
    next_task_id: AtomicUsize,

    clients: slab::Slab<Client>,
    worker: flume::Sender<worker::Task>,
    // todo
}

impl Default for App {
    fn default() -> Self {
        let (tx, rx) = flume::bounded(8);
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(worker::main(rx));
        });
        let mut clients: slab::Slab<Client> = Default::default();
        clients.insert(Client::new(ClientOptions::builder().build()));
        Self {
            clients,
            worker: tx,
            states: Default::default(),
            scroll: Default::default(),
            selected: Default::default(),
            tasks: Default::default(),
            next_task_id: Default::default(),
            exit: Default::default(),
            focus: Default::default(),
            capture: Default::default(),
        }
    }
}

impl App {
    /// runs the application's main loop until the user quits
    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        render::init_state(self);
        while !self.exit {
            terminal.draw(|frame| self.draw(frame))?;
            self.handle_events()?;
            self.poll_events()?;
        }
        Ok(())
    }

    fn draw(&mut self, frame: &mut Frame) {
        use self::Page::*;

        match &self.focus {
            AppFocus::Page(Main) => render::draw_main(self, frame),
            AppFocus::Overlay => todo!(),
        }
    }

    fn exit(&mut self) {
        self.exit = true;
    }

    fn poll_events(&mut self) -> io::Result<()> {
        let mut pending_downloads: SmallVec<[TaskId; 64]> = SmallVec::new();
        let mut pending_completes: SmallVec<[TaskId; 64]> = SmallVec::new();
        for task in self.tasks.values_mut() {
            match &mut task.state {
                TaskState::Pending(failures) => match &task.info {
                    TaskUrlInfo::Pending(receiver) => match receiver.try_recv() {
                        Ok(Ok(value)) => {
                            task.info = TaskUrlInfo::Ready(value.into());
                        }
                        Ok(Err(err)) => {
                            if let Some(retry) = task.retry
                                && failures.len() + 1 > retry.get()
                            {
                                task.info = TaskUrlInfo::Failed;
                            }
                            failures.push_back(err)
                        }
                        Err(flume::TryRecvError::Empty) => {}
                        Err(flume::TryRecvError::Disconnected) => panic!("worker disconnect"),
                    },
                    TaskUrlInfo::Failed => {}
                    TaskUrlInfo::Ready(_) if task.auto => {
                        pending_downloads.push(task.id);
                    }
                    TaskUrlInfo::Ready(_) => {}
                },
                TaskState::Request(statistics, rx) => match rx.try_recv() {
                    Ok(Ok(result)) => {
                        task.state = TaskState::Download(
                            statistics.take().unwrap(),
                            Default::default(),
                            result,
                        )
                    }
                    Ok(Err(err)) => task.state = TaskState::IoError(err),
                    Err(oneshot::TryRecvError::Empty) => {}
                    Err(oneshot::TryRecvError::Disconnected) => panic!("worker disconnect"),
                },
                TaskState::Download(statistics, failures, result) => loop {
                    match result.event_chain.try_recv() {
                        Ok(ev) => match ev {
                            Event::Connecting(id) => {
                                statistics.worker_state(id, FDWorkerState::Connecting)
                            }
                            Event::Downloading(id) => {
                                statistics.worker_state(id, FDWorkerState::Downloading);
                            }
                            Event::DownloadProgress(id, progress) => {
                                statistics.update_download(id, progress);
                            }
                            Event::WriteProgress(id, progress) => {
                                statistics.update_write(id, progress);
                            }
                            Event::Finished(id) => {
                                statistics.worker_state(id, FDWorkerState::Finished);
                            }
                            Event::Abort(id) => {
                                statistics.worker_state(id, FDWorkerState::Abort);
                            }
                            Event::ConnectError(id, err) => {
                                failures.push_back(DownloadErrors::Connect(id, err))
                            }
                            Event::DownloadError(id, err) => {
                                failures.push_back(DownloadErrors::Download(id, err))
                            }
                            Event::WriteError(id, err) => {
                                failures.push_back(DownloadErrors::Write(id, err))
                            }
                        },
                        Err(async_channel::TryRecvError::Empty) => break,
                        Err(async_channel::TryRecvError::Closed) => {
                            pending_completes.push(task.id);
                            break;
                        }
                    }
                },
                TaskState::IoError(_) => {}
                TaskState::Completed => {}
            }
        }
        for task_id in pending_completes {
            match self.tasks.get_mut(&task_id) {
                None => {}
                Some(task) => {
                    task.state = TaskState::Completed;
                }
            }
        }
        for task_id in pending_downloads {
            let task = self.tasks.get_mut(&task_id).unwrap();
            let client = self.clients.get(task.client_id).unwrap().get();
            self.worker
                .send(Self::create_download_command(
                    &task.info.unwrap(),
                    client,
                    task,
                    None,
                    None,
                ))
                .unwrap();
        }

        Ok(())
    }

    fn create_download_command(
        info: &UrlInfo,
        client: reqwest::Client,
        task: &mut DownloadTask,
        maybe_path: Option<PathBuf>,
        maybe_options: Option<DownloadOptions>,
    ) -> worker::Task {
        assert!(
            matches!(task.state, TaskState::Pending(..)),
            "can only transition from Pending"
        );
        let (tx, rx) = oneshot::channel();
        let path = maybe_path
            .or_else(|| task.path.take())
            .unwrap_or_else(|| (&info.file_name).into());
        let mut options = maybe_options
            .or_else(|| task.download_options.take())
            .unwrap();
        if !info.can_fast_download {
            options.concurrent = None;
        }
        task.state = TaskState::Request(
            Some(Statistics::new(
                options.concurrent.map(NonZeroUsize::get).unwrap_or(1),
                info.file_size,
            )),
            rx,
        );
        #[allow(clippy::single_range_in_vec_init)]
        worker::Task::Download(
            client,
            task.url.clone(),
            vec![0..info.file_size],
            info.file_size,
            path,
            options,
            tx,
        )
    }

    pub(crate) fn next_task_id(&mut self) -> usize {
        self.next_task_id.fetch_add(1, Ordering::Relaxed)
    }

    pub fn create_task(
        &mut self,
        client_id: ClientId,
        path: Option<PathBuf>,
        options: Option<DownloadOptions>,
        url: Url,
    ) {
        let id = self.next_task_id();
        let (tx, task) = DownloadTask::pending(id, client_id, path, options, url);
        self.worker
            .send(worker::Task::Fetch(
                self.clients.get(client_id).unwrap().get(),
                task.url.clone(),
                tx,
            ))
            .unwrap();
        self.tasks.insert(id, task);
    }

    // pub fn download_task(&mut self, ) {
    //     let id = self.next_task_id();
    //     let (tx, task) = DownloadTask::pending(id, url);
    //     self.worker.send(worker::Task::Download(..)).unwrap();
    //     self.tasks.insert(id, task);
    // }

    fn handle_key_event(&mut self, key_event: KeyEvent) {
        const SCROLL_SIZE: usize = 5;

        if let Some(callback) = &mut self.capture {
            return callback(key_event);
        }
        match key_event.code {
            KeyCode::PageUp => {
                let scroll = self.scroll.get_or_insert_default();
                *scroll = scroll.saturating_sub(SCROLL_SIZE);
            }
            KeyCode::PageDown => {
                let scroll = self.scroll.get_or_insert_default();
                let (value, overflow) = self.tasks.len().overflowing_sub(*scroll);
                if overflow || value < SCROLL_SIZE {
                    *scroll = self.tasks.len();
                } else {
                    *scroll = scroll.saturating_add(SCROLL_SIZE);
                }
            }
            KeyCode::Up => {
                self.selected = match self.selected {
                    None => self.tasks.keys().next_back().cloned(),
                    Some(id) => self
                        .tasks
                        .range((Unbounded, Excluded(id)))
                        .next_back()
                        .map(|x| *x.0),
                }
            }
            KeyCode::Down => {
                self.selected = match self.selected {
                    None => self.tasks.keys().next().cloned(),
                    Some(id) => self
                        .tasks
                        .range((Excluded(id), Unbounded))
                        .next()
                        .map(|x| *x.0),
                }
            }
            KeyCode::Char('q') => self.exit(),
            KeyCode::Char('p') => {
                let mut clipboard = Clipboard::new().unwrap();
                if let Ok(content) = clipboard.get_text() {
                    if let Ok(url) = Url::parse(&content) {
                        self.create_task(
                            /* todo: show options to user to select client */ 0,
                            None,
                            Some(DownloadOptions {
                                concurrent: NonZeroUsize::new(4),
                                write_buffer_size: 1024,
                                retry_gap: Default::default(),
                                write_channel_size: 4,
                            }),
                            url,
                        );
                    } else {
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_events(&mut self) -> io::Result<()> {
        if term::poll(Duration::from_millis(50))? {
            match term::read()? {
                // it's important to check that the event is a key press event as
                // crossterm also emits key release and repeat events on Windows.
                term::Event::Key(key_event) if key_event.kind == KeyEventKind::Press => {
                    self.handle_key_event(key_event)
                }
                _ => {}
            }
        }
        Ok(())
    }
}
