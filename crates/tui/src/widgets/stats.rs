use crate::state::FDWorkerState;
use fast_down::{ProgressEntry, WorkerId};
use ratatui::layout::Position;
use ratatui::prelude::*;
use ratatui::symbols;
use ratatui::widgets::WidgetRef;
use std::ops::RangeInclusive;
use std::time::Duration;
use swimmer::Recyclable;

// https://github.com/ratatui/ratatui/blob/0afb1a99af8310c29c738bd092e4d08c668955bf/ratatui-widgets/src/gauge.rs
pub(crate) struct WorkerStats {
    // use_unicode: bool,
    style: Style,
    download_color: Color,
    write_color: Color,
    wr_buf: Vec<u8>,
    dl_buf: Vec<u8>,
}

type Precision = f32;
const PADDING: u16 = 1;

impl Recyclable for WorkerStats {
    fn new() -> Self
    where
        Self: Sized,
    {
        WorkerStats::new()
    }

    fn recycle(&mut self) {
        self.wr_buf.clear();
        self.dl_buf.clear();
    }
}

#[inline(always)]
fn calculate_value(entries: &[ProgressEntry], idx: &mut usize, range: RangeInclusive<u64>) -> u64 {
    // let start_byte = i as Precision * per_bytes;
    // let end_byte = (start_byte + per_bytes) as u64;
    // let start_byte = start_byte as u64;
    let mut block_total = 0;
    for segment in entries {
        if segment.end <= *range.start() {
            *idx += 1;
            continue;
        }
        if segment.start >= *range.end() {
            break;
        }
        let overlap_start = segment.start.max(*range.start());
        let overlap_end = segment.end.min(*range.end());
        if overlap_start < overlap_end {
            block_total += overlap_end - overlap_start;
        }
    }

    block_total
}

// todo(CyanChanges): use the same one from cli
pub fn format_size(mut size: f64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB", "PB", "EB", "ZB", "YB"];
    const LEN: usize = UNITS.len();

    let mut unit_index = 0;
    while size >= 700.0 && unit_index < LEN - 1 {
        size /= 1024.0;
        unit_index += 1;
    }
    format!("{:.2} {}", size, UNITS[unit_index])
}

macro_rules! render_span {
    ($rect:expr, $buf:expr, $widget:expr) => {
        $widget.render_ref($rect, $buf);
        $rect.x += 1 + $widget.width() as u16;
    };
}

impl WorkerStats {
    pub(crate) fn new() -> WorkerStats {
        Self {
            // use_unicode: false,
            style: Style::default().bg(Color::Black),
            download_color: Color::Green,
            write_color: Color::Cyan,
            wr_buf: Vec::with_capacity(64),
            dl_buf: Vec::with_capacity(64),
        }
    }

    #[inline]
    pub(crate) fn render(
        &mut self,
        rect: Rect,
        shrink: bool,
        label: Span,
        state: &FDWorkerState,
        write_entries: &[ProgressEntry],
        download_entries: &[ProgressEntry],
        delta_write: u64,
        delta_download: u64,
        duration: Duration,
        written: u64,
        downloaded: u64,
        total: u64,
        buf: &mut Buffer,
    ) -> Rect {
        if rect.is_empty() {
            return Rect::ZERO;
        }
        let shrink = if rect.height < 2 { true } else { shrink };

        // buf.set_style(rect, self.style);

        // compute label value and its position
        // label is put at the center of the gauge_area

        let status_indicator = Span::styled(
            format!(
                "{}", state,
            ), Color::Reset
        );

        let written_label = Span::styled(
            format!(
                "🚚 {:>7}/s {:>3}%",
                format_size(delta_write as f64 / duration.as_secs_f64()),
                Precision::round((written as Precision) / (total as Precision) * 100.0)
            ),
            Color::White,
        );
        let downloaded_label = Span::styled(
            format!(
                "💾 {:>7}/s {:>3}%",
                format_size(delta_download as f64 / duration.as_secs_f64()),
                Precision::round((downloaded as Precision) / (total as Precision) * 100.0)
            ),
            Color::White,
        );

        let labels_width = (label.width() as u16)
            + (status_indicator.width() as u16)
            + (downloaded_label.width() as u16)
            + (written_label.width() as u16)
            + 6 + PADDING;
        {
            let mut rect = rect;
            rect.x += PADDING;
            render_span!(rect, buf, status_indicator);
            render_span!(rect, buf, label);
            render_span!(rect, buf, downloaded_label);
            render_span!(rect, buf, written_label);
        }

        let width = if shrink {
            if rect.width < labels_width {
                rect.width
            } else {
                rect.width
                    .saturating_sub(labels_width)
                    .saturating_sub(PADDING * 2)
            }
        } else {
            rect.width.saturating_sub(PADDING * 2)
        };

        let per_bytes = total as Precision / width as Precision;
        self.dl_buf.clear();
        self.wr_buf.clear();
        self.dl_buf
            .reserve_exact((width as usize).saturating_sub(self.dl_buf.capacity()));
        self.wr_buf
            .reserve_exact((width as usize).saturating_sub(self.wr_buf.capacity()));
        let mut wr_idx = 0;
        let mut dl_idx = 0;
        for i in 0..width {
            let start_byte = i as Precision * per_bytes;
            let end_byte = (start_byte + per_bytes) as u64;
            let start_byte = start_byte as u64;
            let dl_block = calculate_value(
                &download_entries[dl_idx..],
                &mut dl_idx,
                RangeInclusive::new(start_byte, end_byte),
            );
            let wr_block = calculate_value(
                &write_entries[wr_idx..],
                &mut wr_idx,
                RangeInclusive::new(start_byte, end_byte),
            );
            self.dl_buf.push(
                ((dl_block as Precision / per_bytes * (BLOCK_CHARS.len() - 1) as Precision).round()
                    as usize)
                    .min(BLOCK_CHARS.len() - 1) as _,
            );
            self.wr_buf.push(
                ((wr_block as Precision / per_bytes * (BLOCK_CHARS.len() - 1) as Precision).round()
                    as usize)
                    .min(BLOCK_CHARS.len() - 1) as _,
            );
        }

        let bar_y = if shrink { 0u16 } else { 1u16 };
        let bar_x = if shrink {
            labels_width + PADDING
        } else {
            PADDING
        };
        for i in 0..width {
            if self.wr_buf[i as usize] == 0 {
                buf[(rect.x + bar_x + i, rect.y + bar_y)]
                    .set_symbol(BLOCK_CHARS[self.dl_buf[i as usize] as usize])
                    .set_fg(self.download_color)
                    .set_bg(self.style.bg.unwrap_or(Color::Reset));
            } else {
                buf[(rect.x + bar_x + i, rect.y + bar_y)]
                    .set_symbol(BLOCK_CHARS[self.wr_buf[i as usize] as usize])
                    .set_fg(self.write_color)
                    .set_bg(self.download_color);
            }
        }

        let mut rect = rect;
        rect.y += bar_y + 1;
        rect.height -= bar_y + 1;
        rect

        // // the gauge will be filled proportionally to the ratio
        // let filled_width = f64::from(progress_area.width) * self.ratio;
        // let end = if self.use_unicode {
        //     progress_area.left() + filled_width.floor() as u16
        // } else {
        //     progress_area.left() + filled_width.round() as u16
        // };
        // for y in progress_area.top()..progress_area.bottom() {
        //     // render the filled area (left to end)
        //     for x in progress_area.left()..end {
        //         // Use full block for the filled part of the gauge and spaces for the part that is
        //         // covered by the label. Note that the background and foreground colors are swapped
        //         // for the label part, otherwise the gauge will be inverted
        //         if x < label_col || x > label_col + clamped_label_width || y != label_row {
        //             buf[(x, y)]
        //                 .set_symbol(symbols::block::FULL)
        //                 .set_fg(self.style.fg.unwrap_or(Color::Reset))
        //                 .set_bg(self.gauge_style.bg.unwrap_or(Color::Reset));
        //         } else {
        //             buf[(x, y)]
        //                 .set_symbol(" ")
        //                 .set_fg(self.gauge_style.bg.unwrap_or(Color::Reset))
        //                 .set_bg(self.gauge_style.fg.unwrap_or(Color::Reset));
        //         }
        //     }
        //     if self.use_unicode && self.ratio < 1.0 {
        //         buf[(end, y)].set_symbol(get_unicode_block(filled_width % 1.0));
        //     }
        // }
        // // render the label
        // buf.set_span(label_col, label_row, label, clamped_label_width);
    }
}

const BLOCK_CHARS: [&str; 9] = [" ", "▏", "▎", "▍", "▌", "▋", "▊", "▉", "█"];
