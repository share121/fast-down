use crate::{
    args::DownloadArgs,
    fmt,
    persist::Database,
    progress::{self, Painter as ProgressPainter},
};
use color_eyre::eyre::{Result, eyre};
use fast_down::{Event, MergeProgress, ProgressEntry, Total, file::DownloadOptions};
use reqwest::{
    Client, Proxy,
    header::{self, HeaderValue},
};
use std::num::NonZeroUsize;
use std::{env, path::Path, sync::Arc, time::Instant};
use tokio::{
    io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader},
    runtime::Handle,
    sync::Mutex,
};
use url::Url;

macro_rules! predicate {
    ($args:expr) => {
        if ($args.yes) {
            Some(true)
        } else if ($args.no) {
            Some(false)
        } else {
            None
        }
    };
}

#[inline]
async fn confirm(predicate: impl Into<Option<bool>>, prompt: &str, default: bool) -> Result<bool> {
    fn get_text(value: bool) -> u8 {
        match value {
            true => b'Y',
            false => b'N',
        }
    }
    let text = match default {
        true => b"(Y/n) ",
        false => b"(y/N) ",
    };
    let mut stderr = io::stderr();
    stderr.write_all(prompt.as_bytes()).await?;
    stderr.write_all(text).await?;
    if let Some(value) = predicate.into() {
        stderr.write_all(&[get_text(value), b'\n']).await?;
        return Ok(value);
    }
    stderr.flush().await?;
    let mut input = String::with_capacity(4);
    BufReader::new(io::stdin()).read_line(&mut input).await?;
    match input.trim() {
        "y" | "Y" => Ok(true),
        "n" | "N" => Ok(false),
        "" => Ok(default),
        _ => Err(eyre!(t!("err.confirm.invalid-input"))),
    }
}

fn cancel_expected() -> Result<()> {
    eprintln!("{}", t!("err.cancel"));
    Ok(())
}

pub async fn download(mut args: DownloadArgs) -> Result<()> {
    if args.browser {
        let url = Url::parse(&args.url)?;
        args.headers
            .entry(header::ORIGIN)
            .or_insert(HeaderValue::from_str(
                url.origin().ascii_serialization().as_str(),
            )?);
        args.headers
            .entry(header::REFERER)
            .or_insert(HeaderValue::from_str(&args.url)?);
    }
    if args.verbose {
        dbg!(&args);
    }
    let mut client = Client::builder().default_headers(args.headers);
    if let Some(ref proxy) = args.proxy {
        client = client.proxy(Proxy::all(proxy)?);
    }
    let client = client.build()?;
    let db = Database::new().await?;

    let info = loop {
        match fast_down::get_url_info(&args.url, &client).await {
            Ok(info) => break info,
            Err(err) => println!("{}: {}", t!("err.url-info"), err),
        }
        tokio::time::sleep(args.retry_gap).await;
    };
    let concurrent = if info.can_fast_download {
        NonZeroUsize::new(args.threads)
    } else {
        None
    };
    let mut save_path =
        Path::new(&args.save_folder).join(args.file_name.as_ref().unwrap_or(&info.file_name));
    if save_path.is_relative()
        && let Ok(current_dir) = env::current_dir()
    {
        save_path = current_dir.join(save_path);
    }
    save_path = path_clean::clean(save_path);
    let save_path_str = Arc::new(save_path.to_str().unwrap().to_string());

    println!(
        // "文件名: {}\n文件大小: {} ({} 字节) \n文件路径: {}\n线程数量: {}\nETag: {:?}\nLast-Modified: {:?}\n",
        "{}",
        t!(
            "msg.url-info",
            name = info.file_name,
            size = fmt::format_size(info.file_size as f64),
            size_in_bytes = info.file_size,
            path = save_path.to_str().unwrap(),
            concurrent = concurrent.unwrap_or(NonZeroUsize::new(1).unwrap()),
            etag = info.etag.as_deref() : {:?},
            last_modified = info.last_modified.as_deref() : {:?}
        )
    );

    #[allow(clippy::single_range_in_vec_init)]
    let mut download_chunks = vec![0..info.file_size];
    let mut resume_download = false;
    let mut write_progress: Vec<ProgressEntry> =
        Vec::with_capacity(concurrent.map(NonZeroUsize::get).unwrap_or(1));

    if save_path.try_exists()? {
        if args.resume
            && info.can_fast_download
            && let Ok(Some(entry)) = db.get_entry(save_path_str.clone()).await
        {
            let downloaded = entry.progress.total();
            if downloaded < info.file_size {
                download_chunks = progress::invert(&entry.progress, info.file_size);
                write_progress = entry.progress.clone();
                resume_download = true;
                println!("{}", t!("msg.resume-download"));
                println!(
                    "{}",
                    t!(
                        "msg.download",
                        completed = fmt::format_size(downloaded as f64),
                        total = fmt::format_size(info.file_size as f64),
                        percentage = downloaded * 100 / info.file_size
                    ),
                );
                if !args.yes
                    && entry.total_size != info.file_size
                    && !confirm(
                        predicate!(args),
                        &t!(
                            "msg.size-mismatch",
                            saved_size = entry.total_size,
                            new_size = info.file_size
                        ),
                        false,
                    )
                    .await?
                {
                    return cancel_expected();
                }
                if entry.etag != info.etag {
                    if !confirm(
                        predicate!(args),
                        &format!(
                            "原文件 ETag: {:?}\n现文件 ETag: {:?}\n文件 ETag 不一致，是否继续？",
                            entry.etag, info.etag
                        ),
                        false,
                    )
                    .await?
                    {
                        return cancel_expected();
                    }
                } else if let Some(progress_etag) = entry.etag.as_ref()
                    && progress_etag.starts_with("W/")
                {
                    if !confirm(
                        predicate!(args),
                        &t!("msg.weak-etag", etag = progress_etag),
                        false,
                    )
                    .await?
                    {
                        return cancel_expected();
                    }
                } else if !confirm(predicate!(args), &t!("msg.no-etag"), false).await? {
                    return cancel_expected();
                }
                if entry.last_modified != info.last_modified
                    && !confirm(
                        predicate!(args),
                        &t!(
                          "msg.last-modified-mismatch",
                          saved_last_modified = entry.last_modified : {:?},
                          new_last_modified = info.last_modified : {:?}
                        ),
                        false,
                    )
                    .await?
                {
                    return cancel_expected();
                }
            }
        }
        if !args.yes
            && !resume_download
            && !args.force
            && !confirm(predicate!(args), &t!("msg.file-overwrite"), false).await?
        {
            return cancel_expected();
        }
    }

    let result = fast_down::file::download(
        client,
        info.final_url.clone(),
        download_chunks,
        info.file_size,
        &save_path,
        DownloadOptions {
            concurrent,
            retry_gap: args.retry_gap,
            write_buffer_size: args.write_buffer_size,
            write_channel_size: args.write_channel_size,
        },
    )
    .await?;

    let result_clone = result.clone();
    let rt_handle = Handle::current();
    ctrlc::set_handler(move || {
        rt_handle.block_on(async {
            result_clone.cancel();
            result_clone.join().await.unwrap();
        })
    })?;

    let mut last_db_update = Instant::now();

    if !resume_download {
        db.init_entry(
            save_path_str.clone(),
            info.file_size,
            info.etag,
            info.last_modified,
            info.file_name,
            info.final_url.to_string(),
        )
        .await?;
    }

    let painter = Arc::new(Mutex::new(ProgressPainter::new(
        write_progress.clone(),
        info.file_size,
        args.progress_width,
        0.9,
        args.repaint_gap,
    )));
    let painter_handle = ProgressPainter::start_update_thread(painter.clone());
    let start = Instant::now();
    while let Ok(e) = result.event_chain.recv().await {
        match e {
            Event::DownloadProgress(_, p) => painter.lock().await.add(p),
            Event::WriteProgress(_, p) => {
                write_progress.merge_progress(p);
                if last_db_update.elapsed().as_secs() >= 1 {
                    last_db_update = Instant::now();
                    db.update_entry(
                        save_path_str.clone(),
                        write_progress.clone(),
                        start.elapsed().as_millis() as u64,
                    )
                    .await?;
                }
            }
            Event::ConnectError(id, err) => painter.lock().await.print(&format!(
                "{} {}\n {:?}\n",
                t!("verbose.worker-id", id = id),
                t!("verbose.connect-error"),
                err
            ))?,
            Event::DownloadError(id, err) => painter.lock().await.print(&format!(
                "{} {}\n {:?}\n",
                t!("verbose.worker-id", id = id),
                t!("verbose.download-error"),
                err
            ))?,
            Event::WriteError(err) => painter.lock().await.print(&format!(
                "{}\n{:?}\n",
                t!("verbose.write-error"),
                err
            ))?,
            Event::Connecting(id) => {
                if args.verbose {
                    painter.lock().await.print(&format!(
                        "{} {}\n",
                        t!("verbose.worker-id", id = id),
                        t!("verbose.connecting")
                    ))?;
                }
            }
            Event::Downloading(id) => {
                if args.verbose {
                    painter.lock().await.print(&format!(
                        "{} {}\n",
                        t!("verbose.worker-id", id = id),
                        t!("verbose.downloading")
                    ))?;
                }
            }
            Event::Finished(id) => {
                if args.verbose {
                    painter.lock().await.print(&format!(
                        "{} {}\n",
                        t!("verbose.worker-id", id = id),
                        t!("verbose.finished")
                    ))?;
                }
            }
            Event::Abort(id) => {
                painter.lock().await.print(&format!(
                    "{} {}\n",
                    t!("verbose.worker-id", id = id),
                    t!("verbose.abort")
                ))?;
            }
        }
    }
    db.update_entry(
        save_path_str.clone(),
        write_progress.clone(),
        start.elapsed().as_millis() as u64,
    )
    .await?;
    painter.lock().await.update()?;
    painter_handle.cancel();
    result.join().await?;
    Ok(())
}
