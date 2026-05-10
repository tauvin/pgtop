//! Header sparklines: TPS, active connections, cache hit %.

use std::collections::VecDeque;

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, Sparkline},
};

use crate::app::StatsHistory;

/// Render three side-by-side sparklines into `area`. `area` should be at
/// least three rows tall (top border + bars).
pub fn render_sparklines(frame: &mut Frame, area: Rect, history: &StatsHistory) {
    let [tps_area, conns_area, cache_area] = Layout::horizontal([
        Constraint::Ratio(1, 3),
        Constraint::Ratio(1, 3),
        Constraint::Ratio(1, 3),
    ])
    .areas(area);

    render_tps(frame, tps_area, history);
    render_conns(frame, conns_area, history);
    render_cache_hit(frame, cache_area, history);
}

fn render_tps(frame: &mut Frame, area: Rect, history: &StatsHistory) {
    let title = match history.current {
        Some(s) => format!(" TPS: {:.1} ", s.tps),
        None => " TPS: — ".to_string(),
    };

    let data: Vec<u64> = history
        .tps
        .iter()
        .map(|&v| (v * 10.0).max(0.0) as u64)
        .collect();

    let block = Block::default().borders(Borders::TOP).title(title);
    let sparkline = Sparkline::default()
        .block(block)
        .data(&data)
        .style(Style::new().fg(Color::Cyan));

    frame.render_widget(sparkline, area);
}

fn render_conns(frame: &mut Frame, area: Rect, history: &StatsHistory) {
    let title = match history.current {
        Some(s) => format!(" Active: {} ", s.active_connections),
        None => " Active: — ".to_string(),
    };

    let data: Vec<u64> = history.conns.iter().map(|&v| v as u64).collect();

    let block = Block::default().borders(Borders::TOP).title(title);
    let sparkline = Sparkline::default()
        .block(block)
        .data(&data)
        .style(Style::new().fg(Color::Magenta));

    frame.render_widget(sparkline, area);
}

fn render_cache_hit(frame: &mut Frame, area: Rect, history: &StatsHistory) {
    let title = match history.current {
        Some(s) => format!(" Cache hit: {:.1}% ", s.cache_hit_pct),
        None => " Cache hit: — ".to_string(),
    };

    let data: Vec<u64> = history
        .cache_hit
        .iter()
        .map(|&v| v.max(0.0) as u64)
        .collect();

    let block = Block::default().borders(Borders::TOP).title(title);
    let sparkline = Sparkline::default()
        .block(block)
        .data(&data)
        .max(100)
        .style(Style::new().fg(Color::Green));

    frame.render_widget(sparkline, area);
}

#[allow(dead_code)]
pub(super) fn _to_u64<T: Copy + Into<u64>>(buf: &VecDeque<T>) -> Vec<u64> {
    buf.iter().copied().map(|v| v.into()).collect()
}
