use crate::app::App;
use crate::state::{FDWorkerState, TaskState};
use crate::widgets::stats::WorkerStats;
use ratatui::prelude::*;
use ratatui::widgets::*;
use std::cell::RefCell;
use std::time::{Duration, Instant};

pub struct MainPageState {
    widget_pool: swimmer::Pool<WorkerStats>,
    last_call: Instant,
}

impl MainPageState {
    pub fn new() -> MainPageState {
        MainPageState {
            widget_pool: swimmer::Pool::new(),
            last_call: Instant::now(),
        }
    }

    pub(crate) fn fetch_stats_widget(&self) -> swimmer::Recycled<WorkerStats> {
        self.widget_pool.get()
    }
}

pub fn init_state(app: &mut App) {
    app.states.insert(MainPageState::new());
}

pub fn draw_main(app: &mut App, frame: &mut Frame) {
    let state = app.states.get_mut::<MainPageState>().unwrap();
    let last_call = state.last_call.clone();
    state.last_call = Instant::now();
    let delta_time = Instant::now() - last_call;
    let layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(vec![Constraint::Percentage(25), Constraint::Percentage(75)])
        .split(frame.area());

    let tasks_block = Block::bordered()
        .title(" Tasks ")
        .border_type(BorderType::Rounded);

    let tasks = app.tasks.values().skip(app.scroll.unwrap_or(0));
    frame.render_widget(
        List::new(tasks.map(|task| {
            let style = if app.selected == Some(task.id) {
                Style::default().fg(Color::Yellow).bg(Color::Cyan)
            } else {
                Style::default().fg(Color::Cyan)
            };
            Text::styled(format!("{} {}", task.state_icon(), task.url), style)
        }))
        .block(tasks_block),
        layout[0],
    );

    let statistics_block = Block::bordered()
        .title(" Statistics ")
        .border_type(BorderType::Rounded);
    if let Some(id) = app.selected {
        frame.render_widget(&statistics_block, layout[1]);
        let task = app.tasks.get_mut(&id).unwrap();
        match &mut task.state {
            TaskState::Pending(_) => { /* todo */ }
            TaskState::Request(_, _) => { /* todo */ }
            TaskState::Download(statistics, _, _) => {
                let mut s_rect = statistics_block.inner(layout[1]);
                frame.render_widget(statistics_block, layout[1]);
                let mut wid = state.fetch_stats_widget();
                let begin_span = last_call - Duration::from_secs(5);
                let dur = Instant::now().duration_since(begin_span);
                for idx in 0..statistics.state.len() {
                    statistics.purge_write_spans(idx, &begin_span);
                    statistics.purge_download_spans(idx, &begin_span);
                    let mut delta_download = 0;
                    let mut delta_write = 0;
                    for (_, cnt) in statistics.write_spans(idx) {
                        delta_write += cnt;
                    }
                    for (_, cnt) in statistics.download_spans(idx) {
                        delta_download += cnt;
                    }

                    s_rect = wid.render(
                        s_rect,
                        false,
                        Span::styled(format!("{idx}"), Style::default().fg(Color::LightCyan)),
                        &statistics.state[idx],
                        statistics.write_entries(idx),
                        statistics.download_entries(idx),
                        delta_write,
                        delta_download,
                        dur,
                        statistics.written,
                        statistics.downloaded,
                        statistics.total,
                        frame.buffer_mut(),
                    );
                }
            }
            TaskState::Completed => {}
            TaskState::IoError(_) => { /* todo */ }
        }
    } else {
        frame.render_widget(
            statistics_block.style(Style::default().bg(Color::DarkGray)),
            layout[1],
        );
    };
}
