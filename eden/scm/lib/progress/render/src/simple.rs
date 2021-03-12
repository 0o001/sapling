/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

//! Simple renderer. Does not use complex ANSI escape codes (ex. colors).

use crate::RenderingConfig;
use progress_model::CacheStats;
use progress_model::IoTimeSeries;
use progress_model::ProgressBar;
use progress_model::Registry;
use std::borrow::Cow;
use std::sync::Arc;

/// Render progress into a multi-line string.
pub fn render(registry: &Registry, config: &RenderingConfig) -> String {
    let mut lines = Vec::new();

    let cache_list = registry.list_cache_stats();
    let series_list = registry.list_io_time_series();
    let bar_list = registry.list_progress_bar();

    render_cache_stats(&mut lines, &cache_list);
    render_time_series(&mut lines, &series_list);
    render_progress_bars(&mut lines, &bar_list, config);

    for line in lines.iter_mut() {
        *line = config.truncate_line(&line).to_string();
    }

    lines.join("\n")
}

fn render_time_series(lines: &mut Vec<String>, series_list: &[Arc<IoTimeSeries>]) {
    for model in series_list {
        let mut phrases = Vec::with_capacity(4);
        if model.is_stale() {
            continue;
        }

        // Net [▁▂▄█▇▅▃▆] 3 MB/s
        phrases.push(format!("{:>12}", model.topic()));

        let ascii = ascii_time_series(&model);
        phrases.push(format!("[{}]", ascii));

        let (rx, tx) = model.bytes_per_second();
        let speed = human_rx_tx_per_second(rx, tx);
        if !speed.is_empty() {
            phrases.push(speed);
        }

        let count = model.count();
        if count > 1 {
            let unit = model.count_unit();
            phrases.push(format!("{} {}", count, unit));
        }
        let line = phrases.join("  ");
        lines.push(line);
    }
}

fn render_progress_bars(
    lines: &mut Vec<String>,
    bars: &[Arc<ProgressBar>],
    config: &RenderingConfig,
) {
    let mut hidden = 0;
    let mut shown = 0;
    for bar in bars.iter() {
        if config.delay.as_millis() > 0 && bar.elapsed() < config.delay {
            continue;
        }

        if shown >= config.max_bar_count {
            hidden += 1;
            continue;
        }

        shown += 1;

        // topic [====>    ] 12 / 56 files message
        let topic = capitalize(bar.topic().split_whitespace().next().unwrap_or(""));
        let mut phrases = vec![format!("{:>12}", topic)];
        // [===>    ]

        let (pos, total) = bar.position_total();
        let width = 15usize;
        if total > 0 && pos <= total {
            let (len, end) = if pos == total {
                (width, "")
            } else {
                ((pos * (width as u64) / total) as usize, ">")
            };
            phrases.push(format!(
                "[{}{}{}]",
                str::repeat("=", len),
                end,
                str::repeat(" ", width - len - end.len())
            ));
        } else {
            // Spinner
            let pos = if cfg!(test) {
                5
            } else {
                bar.elapsed().as_millis() / 200
            };
            let spaceship = "<=>";
            let left_max = width - spaceship.len();
            // 0, 1, 2, ..., width - 4, width - 3, width - 4, ..., 0
            let mut left_pad = (pos as usize) % (left_max * 2);
            if left_pad >= left_max {
                left_pad = 2 * left_max - left_pad;
            }
            phrases.push(format!(
                "[{}{}{}]",
                str::repeat(" ", left_pad),
                spaceship,
                str::repeat(" ", left_max - left_pad)
            ));
        }

        // 12 / 56 files
        let unit = bar.unit();
        let phrase = match unit {
            "%" => {
                let total = total.max(1);
                format!("{}%", pos.min(total) * 100 / total)
            }
            "bytes" | "B" => {
                if total == 0 {
                    human_bytes(pos as _)
                } else {
                    format!("{}/{}", human_bytes(pos as _), human_bytes(total as _))
                }
            }
            _ => {
                if total == 0 {
                    if pos == 0 {
                        String::new()
                    } else {
                        format!("{} {}", pos, unit)
                    }
                } else {
                    format!("{}/{} {}", pos, total, unit)
                }
            }
        };
        phrases.push(phrase);

        // message
        if let Some(message) = bar.message() {
            phrases.push(message.to_string());
        }
        lines.push(phrases.join("  "));
    }

    if hidden > 0 {
        lines.push(format!("{:>12}  and {} more", "", hidden));
    }
}

fn render_cache_stats(lines: &mut Vec<String>, list: &[Arc<CacheStats>]) {
    for model in list {
        // topic [====>    ] 12 / 56 files message
        let topic = model.topic();
        let miss = model.miss();
        let hit = model.hit();
        let total = miss + hit;
        if total > 0 {
            let mut line = format!("{:>12}  {}", topic, total);
            if miss > 0 {
                let miss_rate = (miss * 100) / (total.max(1));
                line += &format!(" ({}% miss)", miss_rate);
            }
            lines.push(line);
        }
    }
}

fn human_rx_tx_per_second(rx: u64, tx: u64) -> String {
    let mut result = Vec::new();
    for (speed, symbol) in [(rx, '⬇'), (tx, '⬆')].iter() {
        if *speed > 0 {
            result.push(format!("{} {}", symbol, human_bytes_per_second(*speed)));
        }
    }
    result.join("  ")
}

fn human_bytes(bytes: u64) -> String {
    if bytes < 5000 {
        format!("{}B", bytes)
    } else if bytes < 5_000_000 {
        format!("{}KB", bytes / 1000)
    } else if bytes < 5_000_000_000 {
        format!("{}MB", bytes / 1000000)
    } else {
        format!("{}GB", bytes / 1000000000)
    }
}

fn human_bytes_per_second(bytes_per_second: u64) -> String {
    format!("{}/s", human_bytes(bytes_per_second))
}

fn ascii_time_series(time_series: &IoTimeSeries) -> String {
    const GAUGE_CHARS: &[char] = &[' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let v = time_series.scaled_speeds((GAUGE_CHARS.len() - 1) as u8);
    v.into_iter().map(|i| GAUGE_CHARS[i as usize]).collect()
}

fn capitalize<'a>(s: &'a str) -> Cow<'a, str> {
    if s.chars().next().unwrap_or('A').is_ascii_uppercase() {
        Cow::Borrowed(s)
    } else {
        let mut first = true;
        let s: String = s
            .chars()
            .map(|c| {
                if first {
                    first = false;
                    c.to_ascii_uppercase()
                } else {
                    c
                }
            })
            .collect();
        Cow::Owned(s)
    }
}
