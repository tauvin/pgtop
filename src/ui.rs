//! TUI-слой: RAII-обёртка над `ratatui::Terminal` и render-функция кадра.
//!
//! Phase 4 block A-B: цветовая подсветка по state/duration; Mode-aware render
//! с overlay-модалкой для Detail view.

use std::io::{self, Stdout};

use chrono::{DateTime, Utc};
use color_eyre::eyre::{Context, Result};
use crossterm::{
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph, Row, Table, Wrap},
};

use crate::{
    app::{App, Mode, Sort, SortBy},
    db::Backend,
};

/// `ratatui::Terminal` параметризован backend'ом, а `CrosstermBackend` —
/// типом стрима, в который шлёт ANSI-байты. Зафиксировав `Stdout`,
/// говорим: «писать в реальный stdout процесса». В юнит-тестах backend
/// можно подменить на `TestBackend` — render-логика не изменится.
pub type Tui = Terminal<CrosstermBackend<Stdout>>;

/// RAII-обёртка над `ratatui::Terminal`: переводит терминал в TUI-режим
/// при создании и восстанавливает при дропе.
///
/// Drop запускается на конце scope'а — нормальный return, ?-ошибка или
/// panic-unwinding. Дополнительно ставим panic hook: он вызывается **до**
/// unwinding'а и Drop'ов, и тоже делает cleanup — иначе panic-стек ушёл бы
/// в alt-screen и пропал. Hook + Drop идемпотентны (повторный
/// `LeaveAlternateScreen`/`disable_raw_mode` безвредны).
pub struct TerminalGuard {
    terminal: Tui,
}

impl TerminalGuard {
    pub fn new() -> Result<Self> {
        enable_raw_mode().wrap_err("enable raw mode")?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen).wrap_err("enter alternate screen")?;

        Self::install_panic_hook();

        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend).wrap_err("create ratatui terminal")?;
        Ok(Self { terminal })
    }

    pub fn terminal(&mut self) -> &mut Tui {
        &mut self.terminal
    }

    fn install_panic_hook() {
        let original = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = restore_disciplines();
            original(info);
        }));
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = restore_disciplines();
    }
}

fn restore_disciplines() -> io::Result<()> {
    execute!(io::stdout(), LeaveAlternateScreen)?;
    disable_raw_mode()?;
    Ok(())
}

/// Главный entry point рендера. Сначала всегда рисуем base view (table +
/// footer); затем overlay-модалка по `app.mode`. Это даёт «фон + popup»-эффект:
/// модалка `Clear`'ит свою область, поэтому таблица за ней не просвечивает,
/// но вокруг видна.
pub fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    render_base(frame, area, app);

    // Match по `&app.mode`, чтобы не двигать значение и не требовать Copy
    // на Mode (в block C появится `Filter(String)` — неCopy).
    if let Mode::Detail(pid) = &app.mode {
        let pid = *pid;
        if let Some(b) = app.backends.iter().find(|b| b.pid == pid) {
            render_detail(frame, area, b);
        }
        // Если pid не нашёлся — set_backends уже вернул Mode::Normal,
        // сюда мы бы не попали. Защитный no-op на всякий случай.
    }
}

/// База: рамка с заголовком + таблица + filter-line + footer.
///
/// Filter-line всегда занимает 1 строку — даже когда пустая. Это упрощает
/// layout (фиксированный `[Min(0), Length(1), Length(1)]`) и UI не «прыгает»
/// по вертикали при включении/выключении фильтра.
fn render_base(frame: &mut Frame, area: Rect, app: &mut App) {
    let visible = app.filtered.len();
    let total = app.backends.len();
    let title = if visible == total {
        format!(" pgtop — Activity ({total} backends) ")
    } else {
        format!(" pgtop — Activity ({visible}/{total} backends) ")
    };
    let block = Block::bordered().title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [table_area, filter_area, footer_area] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);

    render_table(frame, table_area, app);
    render_filter_line(frame, filter_area, app);
    render_footer(frame, footer_area, &app.mode);
}

fn render_table(frame: &mut Frame, area: Rect, app: &mut App) {
    let header = build_header_row(app.sort);

    let widths = [
        Constraint::Length(7),
        Constraint::Length(14),
        Constraint::Length(20),
        Constraint::Length(15),
        Constraint::Length(10),
        Constraint::Min(0),
    ];

    // Считаем «сейчас» один раз на кадр, чтобы duration в разных строках был
    // консистентен по единой точке отсчёта. Итерируем `visible_backends`
    // (отфильтрованные) — `table_state.selected` индексирует именно их.
    //
    // Собираем в `Vec<Row>` явно: иначе immutable borrow от
    // `app.visible_backends()` тянется до конца Table::new(...) и мешает
    // mutable borrow `&mut app.table_state` ниже. С `Vec` — borrow
    // заканчивается на `.collect()`.
    let now = Utc::now();
    let rows: Vec<Row<'static>> = app
        .visible_backends()
        .map(|b| backend_to_row(b, now))
        .collect();

    let table = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(Style::new().reversed());

    frame.render_stateful_widget(table, area, &mut app.table_state);
}

/// Строка состояния фильтра между таблицей и footer'ом.
///
/// - В `Mode::Filter`: показываем `/`-префикс, текущий ввод, маркер курсора
///   `█` и индикатор `(invalid regex)` если regex не скомпилировался.
/// - В `Normal`/`Detail` с активным фильтром: dim-строка `filter: pattern`.
/// - Иначе пустая (1 строка зарезервирована, рисуется ничего).
fn render_filter_line(frame: &mut Frame, area: Rect, app: &App) {
    let line = if matches!(app.mode, Mode::Filter) {
        let value = app.filter.input.value();
        let invalid = !value.is_empty() && app.filter.regex.is_none();
        let mut spans = vec![
            Span::raw(" /"),
            Span::raw(value.to_string()).bold(),
            "█".bold(),
        ];
        if invalid {
            spans.push(Span::raw("  "));
            spans.push("(invalid regex)".red());
        }
        Line::from(spans)
    } else if app.filter.regex.is_some() {
        Line::from(vec![
            " filter: ".dim(),
            app.filter.input.value().to_string().dim(),
        ])
    } else {
        return; // пустая строка — ничего не рендерим
    };

    frame.render_widget(Paragraph::new(line), area);
}

/// Заголовок таблицы с индикатором сортировки на активной колонке.
/// Активная колонка получает суффикс ` ▲`/` ▼` — пользователь видит,
/// по чему сейчас сортировано и куда.
fn build_header_row(sort: Sort) -> Row<'static> {
    const COLUMNS: [SortBy; 6] = [
        SortBy::Pid,
        SortBy::User,
        SortBy::State,
        SortBy::Wait,
        SortBy::Duration,
        SortBy::Query,
    ];

    let cells: Vec<String> = COLUMNS
        .iter()
        .map(|&col| {
            if col == sort.by {
                format!("{} {}", col.label(), sort.direction.arrow())
            } else {
                col.label().to_string()
            }
        })
        .collect();

    Row::new(cells).style(Style::new().add_modifier(Modifier::BOLD))
}

/// `Backend` → `Row<'static>` со строками-владельцами + цветовая подсветка.
fn backend_to_row(b: &Backend, now: DateTime<Utc>) -> Row<'static> {
    Row::new([
        b.pid.to_string(),
        b.usename.clone().unwrap_or_else(em_dash),
        b.state.clone().unwrap_or_else(em_dash),
        format_wait(b),
        format_duration(b.query_start, now),
        format_query(b.query.as_deref()),
    ])
    .style(row_style(b, now))
}

/// Стиль для всей строки таблицы исходя из состояния backend'а.
///
/// Приоритет: красный > жёлтый > зелёный > default.
/// - Красный: active-запрос дольше 10 секунд (визуальный сигнал «долгий»).
/// - Жёлтый: idle in transaction (потенциально удерживает локи / vacuum).
/// - Зелёный: обычный active (≤10s).
/// - Default: всё остальное (idle-сессии, fastpath function call и т.п.).
fn row_style(b: &Backend, now: DateTime<Utc>) -> Style {
    const LONG_QUERY_THRESHOLD_SECS: i64 = 10;

    let state = b.state.as_deref();

    if state == Some("active") {
        let long = b
            .query_start
            .map(|s| (now - s).num_seconds() > LONG_QUERY_THRESHOLD_SECS)
            .unwrap_or(false);
        return if long {
            Style::new().fg(Color::Red)
        } else {
            Style::new().fg(Color::Green)
        };
    }

    if state.is_some_and(|s| s.starts_with("idle in transaction")) {
        return Style::new().fg(Color::Yellow);
    }

    Style::default()
}

fn format_wait(b: &Backend) -> String {
    match (&b.wait_event_type, &b.wait_event) {
        (Some(t), Some(e)) => format!("{t}: {e}"),
        (Some(t), None) => t.clone(),
        (None, Some(e)) => e.clone(),
        (None, None) => em_dash(),
    }
}

fn format_duration(query_start: Option<DateTime<Utc>>, now: DateTime<Utc>) -> String {
    let Some(start) = query_start else {
        return em_dash();
    };
    let total_secs = (now - start).num_seconds().max(0);
    let h = total_secs / 3600;
    let m = (total_secs % 3600) / 60;
    let s = total_secs % 60;
    format!("{h}:{m:02}:{s:02}")
}

fn format_query(query: Option<&str>) -> String {
    let Some(q) = query else {
        return em_dash();
    };
    let one_line: String = q.split_whitespace().collect::<Vec<_>>().join(" ");
    match one_line.char_indices().nth(60) {
        Some((cutoff, _)) => format!("{}…", &one_line[..cutoff]),
        None => one_line,
    }
}

fn em_dash() -> String {
    "—".to_string()
}

/// Footer мode-aware: в каждом режиме свой набор хоткеев.
fn render_footer(frame: &mut Frame, area: Rect, mode: &Mode) {
    let line = match mode {
        Mode::Normal => Line::from(vec![
            Span::raw(" "),
            "q".bold(),
            Span::raw(" quit  ·  "),
            "↑↓".bold(),
            Span::raw(" move  ·  "),
            "Enter".bold(),
            Span::raw(" details  ·  "),
            "/".bold(),
            Span::raw(" filter  ·  "),
            "s".bold(),
            Span::raw("/"),
            "S".bold(),
            Span::raw(" sort"),
        ]),
        Mode::Detail(_) => Line::from(vec![
            Span::raw(" "),
            "Esc".bold(),
            Span::raw(" close  ·  "),
            "q".bold(),
            Span::raw(" quit"),
        ]),
        Mode::Filter => Line::from(vec![
            Span::raw(" "),
            "Enter".bold(),
            Span::raw(" apply  ·  "),
            "Esc".bold(),
            Span::raw(" cancel"),
        ]),
    };

    let footer = Paragraph::new(line).style(Style::new().dim());
    frame.render_widget(footer, area);
}

/// Detail-модалка: центрированный popup поверх таблицы.
///
/// `Clear` widget «прокалывает дыру» в фоне — иначе symbols таблицы
/// просвечивали бы под содержимым. Это идиоматический ratatui-паттерн
/// для popup'ов.
fn render_detail(frame: &mut Frame, area: Rect, b: &Backend) {
    let popup = centered_rect(80, 70, area);

    frame.render_widget(Clear, popup);

    let block = Block::bordered().title(format!(" Detail · pid {} ", b.pid));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let lines = build_detail_lines(b);
    let para = Paragraph::new(lines).wrap(Wrap { trim: false });
    frame.render_widget(para, inner);
}

/// Построить список строк для detail-модалки. Группы разделены пустыми Line —
/// визуальные «секции»: connection / state / timing / xid / query.
fn build_detail_lines(b: &Backend) -> Vec<Line<'static>> {
    vec![
        kv("user", b.usename.as_deref()),
        kv("database", b.datname.as_deref()),
        kv("application", b.application_name.as_deref()),
        kv("client", b.client_addr.as_deref()),
        kv("backend type", b.backend_type.as_deref()),
        Line::from(""),
        kv("state", b.state.as_deref()),
        Line::from(vec!["wait: ".bold(), Span::raw(format_wait(b))]),
        Line::from(""),
        kv_dt("backend started", Some(b.backend_start)),
        kv_dt("tx started", b.xact_start),
        kv_dt("query started", b.query_start),
        kv_dt("state changed", b.state_change),
        Line::from(""),
        kv("xid", b.backend_xid.as_deref()),
        kv("xmin", b.backend_xmin.as_deref()),
        Line::from(""),
        Line::from("query:".bold()),
        Line::from(b.query.clone().unwrap_or_else(em_dash)),
    ]
}

/// «label: value» с bold-меткой; пустое значение → em-dash.
fn kv(label: &'static str, value: Option<&str>) -> Line<'static> {
    Line::from(vec![
        format!("{label}: ").bold(),
        Span::raw(value.unwrap_or("—").to_string()),
    ])
}

/// Аналогично `kv`, но для `DateTime<Utc>` форматируем как HH:MM:SS.
fn kv_dt(label: &'static str, value: Option<DateTime<Utc>>) -> Line<'static> {
    let s = value
        .map(|t| t.format("%H:%M:%S").to_string())
        .unwrap_or_else(em_dash);
    Line::from(vec![format!("{label}: ").bold(), Span::raw(s)])
}

/// Центрированный прямоугольник заданного процента от `area`.
///
/// Двойной Layout-split: сначала вертикально режем на три полосы (sides + middle),
/// потом среднюю — горизонтально на три. Возвращаем центральный квадрант.
/// `[_, mid, _] = ...areas(...)` — destructure с игнорированием боковых частей.
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let [_, mid_v, _] = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .areas(area);

    let [_, mid_h, _] = Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .areas(mid_v);

    mid_h
}
