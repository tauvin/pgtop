//! Sparkline-полоса в шапке: TPS / Active connections / Cache hit %.
//!
//! Три sparkline'а side-by-side, каждый с top-border'ом, в title которого
//! зашита текущая величина. Использует `ratatui::widgets::Sparkline`.

use std::collections::VecDeque;

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, Sparkline},
};

use crate::app::StatsHistory;

/// Render header-sparklines в `area` высотой ≥ 3 (1 строка border+title +
/// 2 строки bars). Если `history.current == None` (ещё нет снимка),
/// рисуются пустые рамки с прочерками.
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

    // TPS часто <10 (на наш load-generator); умножаем на 10 перед truncate'ом
    // в u64, чтобы дробные значения формировали видимые столбцы. Sparkline
    // auto-max адаптируется под максимум в данных, поэтому масштабирование
    // не искажает форму графика.
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

    // u32 → u64. Без умножения: значения уже целые и обычно >= 0.
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

    // 0..100, без скейлинга. Фиксируем max=100 чтобы 99% не выглядело как
    // «забит», когда диапазон стабилизировался.
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

/// Заглушка — пока не используется, но может пригодиться для других metric'ов
/// в Phase 6+. Выносить общий `render_sparkline_metric` overengineering для 3
/// разных типов данных (f64+scale, u32, f64-percentage), поэтому пока 3 hand-rolled.
#[allow(dead_code)]
pub(super) fn _to_u64<T: Copy + Into<u64>>(buf: &VecDeque<T>) -> Vec<u64> {
    buf.iter().copied().map(|v| v.into()).collect()
}
