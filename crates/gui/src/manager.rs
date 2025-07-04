use crate::{args::DownloadArgs, fmt, home_page::ProgressData, path, persist::Database, progress};
use color_eyre::Result;
use crossbeam_channel::{Receiver, Sender};
use fast_down::{
    file::DownloadOptions, DownloadResult, Event, MergeProgress, ProgressEntry, Total,
};
use reqwest::{
    blocking::Client,
    header::{self, HeaderValue},
    Proxy,
};
use std::{
    env,
    path::Path,
    sync::{Arc, RwLock},
    thread::{self, JoinHandle},
    time::Instant,
};
use url::Url;

#[derive(Debug, Clone)]
pub enum Action {
    Stop(usize),
    Resume(usize),
    AddTask(String),
    RemoveTask(usize),
}

#[derive(Debug, Clone)]
pub enum Message {
    ProgressUpdate(usize, Vec<ProgressData>),
    Stopped(usize),
    Started(usize),
    FileName(usize, String),
}

#[derive(Debug)]
pub struct Manager {
    tx_ctrl: Sender<Action>,
    pub rx_recv: Receiver<Message>,
    pub handle: JoinHandle<()>,
}

#[derive(Clone)]
pub struct ManagerData {
    pub result: Option<DownloadResult>,
    pub url: String,
    pub file_path: Option<String>,
}

fn download(
    args: &DownloadArgs,
    url: &str,
    tx: Sender<Message>,
    manager_data: Arc<RwLock<ManagerData>>,
    list: Arc<RwLock<Vec<Arc<RwLock<ManagerData>>>>>,
    is_resume: bool,
) -> Result<()> {
    let mut headers = args.headers.clone();
    if args.browser {
        {
            let url = Url::parse(&url)?;
            headers
                .entry(header::ORIGIN)
                .or_insert(HeaderValue::from_str(
                    url.origin().ascii_serialization().as_str(),
                )?);
        }
        headers
            .entry(header::REFERER)
            .or_insert(HeaderValue::from_str(&url)?);
    }
    dbg!(&args);
    dbg!(&headers);
    let mut client = Client::builder().default_headers(headers);
    if let Some(ref proxy) = args.proxy {
        client = client.proxy(Proxy::all(proxy)?);
    }
    let client = client.build()?;
    let db = Database::new()?;

    let info = fast_down::get_url_info(&url, &client)?;

    let threads = if info.can_fast_download {
        args.threads
    } else {
        1
    };
    let mut save_path = Path::new(&args.save_folder).join(&info.file_name);
    if save_path.is_relative() {
        if let Ok(current_dir) = env::current_dir() {
            save_path = current_dir.join(save_path);
        }
    }
    save_path = path_clean::clean(save_path);
    let mut save_path_str = save_path.to_str().unwrap().to_string();

    let mut download_chunks = vec![0..info.file_size];
    let mut write_progress: Vec<ProgressEntry> = Vec::with_capacity(threads);

    if save_path.try_exists()? {
        if is_resume {
            if info.can_fast_download {
                if let Ok(Some(progress)) = db.get_entry(&save_path_str) {
                    let downloaded = progress.progress.total();
                    if downloaded < info.file_size {
                        download_chunks = progress::invert(&progress.progress, info.file_size);
                        write_progress = progress.progress.clone();
                        println!("发现未完成的下载，将继续下载剩余部分");
                        println!(
                            "已下载: {} / {} ({}%)",
                            fmt::format_size(downloaded as f64),
                            fmt::format_size(info.file_size as f64),
                            downloaded * 100 / info.file_size
                        );
                    }
                }
            }
        } else {
            save_path = path::find_available_path(&save_path)?;
            save_path_str = save_path.to_str().unwrap().to_string();
        }
    }

    let file_name = save_path.file_name().unwrap().to_str().unwrap();

    tx.send(Message::FileName(
        list.read()
            .unwrap()
            .iter()
            .position(|e| Arc::ptr_eq(&e, &manager_data))
            .unwrap(),
        file_name.into(),
    ))?;

    println!(
        "文件名: {}\n文件大小: {} ({} 字节) \n文件路径: {}\n线程数量: {}\nETag: {:?}\nLast-Modified: {:?}\n",
        info.file_name,
        fmt::format_size(info.file_size as f64),
        info.file_size,
        save_path_str,
        threads,
        info.etag,
        info.last_modified
    );

    let result = fast_down::file::download(
        &info.final_url,
        &save_path,
        DownloadOptions {
            threads,
            can_fast_download: info.can_fast_download,
            file_size: info.file_size,
            client,
            download_buffer_size: args.download_buffer_size,
            download_chunks,
            retry_gap: args.retry_gap,
            write_buffer_size: args.write_buffer_size,
        },
    )?;
    manager_data.write().unwrap().result.replace(result.clone());

    let mut last_db_update = Instant::now();

    if !is_resume {
        db.init_entry(
            &save_path_str,
            info.file_size,
            info.etag,
            info.last_modified,
            &file_name,
            &info.final_url,
        )?;
    }

    let start = Instant::now();
    for e in &result {
        match e {
            Event::DownloadProgress(p) => {
                write_progress.merge_progress(p);
                if last_db_update.elapsed().as_secs() >= 1 {
                    db.update_entry(
                        &save_path_str,
                        &write_progress,
                        start.elapsed().as_millis() as u64,
                    )?;
                    last_db_update = Instant::now();
                }
                tx.send(Message::ProgressUpdate(
                    list.read()
                        .unwrap()
                        .iter()
                        .position(|e| Arc::ptr_eq(&e, &manager_data))
                        .unwrap(),
                    progress::add_blank(&write_progress, info.file_size),
                ))?;
            }
            _ => {}
        }
    }
    db.update_entry(
        &save_path_str,
        &write_progress,
        start.elapsed().as_millis() as u64,
    )?;
    tx.send(Message::ProgressUpdate(
        list.read()
            .unwrap()
            .iter()
            .position(|e| Arc::ptr_eq(&e, &manager_data))
            .unwrap(),
        progress::add_blank(&write_progress, info.file_size),
    ))?;
    result.join().unwrap();
    tx.send(Message::Stopped(
        list.read()
            .unwrap()
            .iter()
            .position(|e| Arc::ptr_eq(&e, &manager_data))
            .unwrap(),
    ))?;
    Ok(())
}

impl Manager {
    pub fn new(args: Arc<DownloadArgs>, init: Vec<Arc<RwLock<ManagerData>>>) -> Self {
        let (tx_ctrl, rx_ctrl) = crossbeam_channel::unbounded();
        let (tx_recv, rx_recv) = crossbeam_channel::unbounded();
        let handle = thread::spawn(move || {
            let list = Arc::new(RwLock::new(init));
            let db = Database::new().unwrap();
            for event in rx_ctrl {
                let list = list.clone();
                match event {
                    Action::Stop(index) => {
                        let list = list.read().unwrap();
                        let mut res = list[index].write().unwrap();
                        if let Some(res) = res.result.take() {
                            res.cancel();
                        }
                    }
                    Action::Resume(index) => {
                        let mut list_inner = list.write().unwrap();
                        let origin_data = list_inner.remove(index);
                        let mut origin_data = origin_data.write().unwrap();
                        if let Some(res) = origin_data.result.take() {
                            res.cancel();
                        }
                        let manager_data = Arc::new(RwLock::new(ManagerData {
                            result: None,
                            url: origin_data.url.clone(),
                            file_path: origin_data.file_path.clone(),
                        }));
                        list_inner.insert(index, manager_data.clone());
                        let args = args.clone();
                        let tx_recv = tx_recv.clone();
                        let list = list.clone();
                        let url = origin_data.url.clone();
                        thread::spawn(move || {
                            let tx_clone = tx_recv.clone();
                            let manager_data_clone = manager_data.clone();
                            let list_clone = list.clone();
                            download(&args, &url, tx_recv, manager_data, list, true).map_err(
                                move |e| {
                                    eprintln!("Error: {e:#?}");
                                    tx_clone
                                        .send(Message::Stopped(
                                            list_clone
                                                .read()
                                                .unwrap()
                                                .iter()
                                                .position(|e| Arc::ptr_eq(&e, &manager_data_clone))
                                                .unwrap(),
                                        ))
                                        .unwrap();
                                },
                            )
                        });
                    }
                    Action::AddTask(url) => {
                        let manager_data = Arc::new(RwLock::new(ManagerData {
                            result: None,
                            url: url.clone(),
                            file_path: None,
                        }));
                        list.write().unwrap().insert(0, manager_data.clone());
                        let args = args.clone();
                        let tx_recv = tx_recv.clone();
                        thread::spawn(move || {
                            let tx_clone = tx_recv.clone();
                            let manager_data_clone = manager_data.clone();
                            let list_clone = list.clone();
                            download(&args, &url, tx_recv, manager_data, list, false).map_err(
                                move |e| {
                                    eprintln!("Error: {e:#?}");
                                    tx_clone
                                        .send(Message::Stopped(
                                            list_clone
                                                .read()
                                                .unwrap()
                                                .iter()
                                                .position(|e| Arc::ptr_eq(&e, &manager_data_clone))
                                                .unwrap(),
                                        ))
                                        .unwrap();
                                },
                            )
                        });
                    }
                    Action::RemoveTask(index) => {
                        let mut list = list.write().unwrap();
                        let origin_data = list.remove(index);
                        let mut origin_data = origin_data.write().unwrap();
                        if let Some(res) = origin_data.result.take() {
                            res.cancel();
                        }
                        if let Some(file_path) = origin_data.file_path.as_ref() {
                            db.remove_entry(file_path).unwrap();
                        }
                    }
                }
            }
        });
        Self {
            tx_ctrl,
            rx_recv,
            handle,
        }
    }

    pub fn stop(&self, index: usize) -> Result<(), crossbeam_channel::SendError<Action>> {
        self.tx_ctrl.send(Action::Stop(index))
    }

    pub fn resume(&self, index: usize) -> Result<(), crossbeam_channel::SendError<Action>> {
        self.tx_ctrl.send(Action::Resume(index))
    }

    pub fn add_task(&self, url: String) -> Result<(), crossbeam_channel::SendError<Action>> {
        self.tx_ctrl.send(Action::AddTask(url))
    }

    pub fn remove_task(&self, index: usize) -> Result<(), crossbeam_channel::SendError<Action>> {
        self.tx_ctrl.send(Action::RemoveTask(index))
    }
}
