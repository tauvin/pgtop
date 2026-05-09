//! TUI-слой: RAII-обёртка над `ratatui::Terminal` и render-функция кадра.
//!
//! Phase 2 завершена: alternate screen + raw mode через `TerminalGuard` (Drop +
//! panic hook), Table со статичными данными, footer с подсказками. Реальные
//! данные от collector'а вместо `mock_rows` подключим в Phase 3.

use std::io::{self, Stdout};

use color_eyre::eyre::{Context, Result};
use crossterm::{
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Paragraph, Row, Table},
};

use crate::app::App;

/// `ratatui::Terminal` параметризован backend'ом, а `CrosstermBackend` —
/// типом стрима, в который шлёт ANSI-байты. Зафиксировав `Stdout`,
/// говорим: «писать в реальный stdout процесса». В юнит-тестах backend
/// можно подменить на `TestBackend` — render-логика не изменится.
pub type Tui = Terminal<CrosstermBackend<Stdout>>;

/// RAII-обёртка над `ratatui::Terminal`: переводит терминал в TUI-режим
/// при создании и восстанавливает при дропе.
///
/// Что даёт RAII здесь:
/// - **Штатный выход** (нормальный `return`, ранний `?`-ошибка) — Drop
///   запускается автоматически на конце scope'а, нет шансов забыть
///   `restore_terminal`.
/// - **Паника** — при `panic = "unwind"` (дефолт Cargo) Drop тоже отработает
///   во время unwinding'а. Но **до** unwinding'а сначала зовётся panic hook —
///   а он по дефолту печатает стек в stderr. Поэтому hook отдельно делает
///   ту же самую очистку: иначе стектрейс уйдёт в alt-screen и пропадёт.
/// - Hook + Drop **идемпотентны**: оба зовут `restore_disciplines`, повторный
///   вызов `disable_raw_mode`/`LeaveAlternateScreen` безвреден.
/// - Под `panic = "abort"` Drop не запускается вообще, и hook остаётся
///   единственной точкой восстановления — поэтому его не убираем.
pub struct TerminalGuard {
    terminal: Tui,
}

impl TerminalGuard {
    /// Перевести терминал в TUI-режим (raw mode + alternate screen),
    /// поставить panic hook и завернуть всё в guard.
    ///
    /// **raw mode** — драйвер терминала перестаёт интерпретировать input:
    /// никакого line-buffering'а (читаем по символу), echo (символы не
    /// дублируются), Ctrl+C/Z как сигналов (приходят как `KeyEvent`).
    ///
    /// **alternate screen** — xterm-фича: переключение на отдельный
    /// экранный буфер. При выходе исходный буфер с командной строкой
    /// и историей возвращается. Так работают vim, less, htop.
    ///
    /// **Terminal::new** — обёртка ratatui. Хранит double buffer
    /// (предыдущий кадр + текущий), на каждом `draw` шлёт в backend
    /// только diff — отсюда «immediate mode без перерисовки всего экрана».
    pub fn new() -> Result<Self> {
        enable_raw_mode().wrap_err("enable raw mode")?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen).wrap_err("enter alternate screen")?;

        Self::install_panic_hook();

        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend).wrap_err("create ratatui terminal")?;
        Ok(Self { terminal })
    }

    /// Доступ к ratatui::Terminal для вызовов `draw`. Метод возвращает
    /// `&mut Tui` — реборроу `*self.terminal` на время вызова. После
    /// возврата borrow заканчивается, и guard снова доступен (например,
    /// чтобы дропнуться в конце функции).
    pub fn terminal(&mut self) -> &mut Tui {
        &mut self.terminal
    }

    /// Custom panic hook: сначала восстанавливаем терминал, потом отдаём
    /// управление исходному hook'у — color-eyre отрисует красивый отчёт
    /// уже в нормальном TTY.
    ///
    /// `take_hook` забирает текущий hook (там может быть уже color-eyre'овский,
    /// если он установлен раньше — мы зовём `color_eyre::install()` в main
    /// до создания guard'а). `set_hook` ставит наш wrapping-hook, который
    /// **после** очистки call'ит исходный — так color-eyre-форматирование
    /// не теряется.
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
        // Best-effort: в Drop нельзя вернуть `Result`. Если что-то пойдёт
        // не так — процесс всё равно умирает, главное попытаться.
        // Идемпотентно с panic hook'ом: если он уже сработал, повторные
        // `LeaveAlternateScreen`/`disable_raw_mode` безвредны.
        let _ = restore_disciplines();
    }
}

/// Снятие TUI-дисциплин — возврат в нормальный TTY. Используется и из
/// panic hook'а, и из `Drop`, отсюда единая helper-функция.
///
/// Порядок: сначала `LeaveAlternateScreen` (вернуться к командной строке),
/// потом `disable_raw_mode` (вернуть line-buffering/echo). Если перепутать,
/// пользователь увидит alt-screen без raw mode (или наоборот) — выглядит
/// «сломанно».
fn restore_disciplines() -> io::Result<()> {
    execute!(io::stdout(), LeaveAlternateScreen)?;
    disable_raw_mode()?;
    Ok(())
}

/// Один кадр UI: внешний `Block`, внутри — `Table` сверху и `Paragraph`-footer
/// снизу. Layout-вертикальный split: таблица берёт всё свободное место,
/// footer — ровно одну строку.
///
/// Принимает `&mut App` (а не `&App`), потому что `render_stateful_widget`
/// требует `&mut TableState` — ratatui мутирует state при необходимости
/// прокрутки (если выделение ушло за видимый край, offset подвинется).
pub fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();

    let block = Block::bordered().title(" pgtop — Activity ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // `Layout::vertical([...])` — шорткат для
    // `Layout::default().direction(Direction::Vertical).constraints([...])`.
    // `.areas::<N>(rect)` возвращает `[Rect; N]`. Через `let [a, b] = ...`
    // получаем компайл-тайм проверку: если поменяем число constraint'ов,
    // pattern перестанет матчиться и compilation сломается. Безопаснее
    // `.split()`, который отдаёт `Rc<[Rect]>` и индексируется по числу.
    let [table_area, footer_area] = Layout::vertical([
        Constraint::Min(0),    // таблица — всё свободное место
        Constraint::Length(1), // подсказка — ровно одна строка
    ])
    .areas(inner);

    render_table(frame, table_area, app);
    render_footer(frame, footer_area);
}

fn render_table(frame: &mut Frame, area: Rect, app: &mut App) {
    let header_style = Style::new().add_modifier(Modifier::BOLD);
    let header =
        Row::new(["pid", "user", "state", "wait", "duration", "query"]).style(header_style);

    let widths = [
        Constraint::Length(7),
        Constraint::Length(14),
        Constraint::Length(20),
        Constraint::Length(15),
        Constraint::Length(10),
        Constraint::Min(0),
    ];

    // app.rows.iter() даёт &MockRow; .copied() — Copy у [T; N] есть когда
    // T: Copy (а T = &'static str — Copy). Получаем итератор MockRow по
    // value, который Row::new принимает как IntoIterator ячеек.
    let rows = app.rows.iter().copied().map(Row::new);

    let table = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(Style::new().reversed());

    frame.render_stateful_widget(table, area, &mut app.table_state);
}

/// Footer-строка с подсказками хоткеев.
///
/// Тут знакомимся с rich-text API ratatui:
/// - `Span` — кусок строки с одним `Style` (например, `"q".bold()`).
/// - `Line` — последовательность `Span`'ов на одной строке.
/// - `Text` — несколько `Line` (Paragraph принимает `Into<Text>`).
///
/// `Stylize`-трейт даёт builder-методы прямо на `&str` (`"q".bold()` =
/// `Span::raw("q").add_modifier(BOLD)`) и на `Style`/`Span`/`Line` —
/// удобно для коротких inline-стилей. `Style::new().dim()` уменьшает
/// яркость — стандартный вид «второстепенного» текста.
fn render_footer(frame: &mut Frame, area: Rect) {
    let line = Line::from(vec![
        Span::raw(" "),
        "q".bold(),
        Span::raw(" / "),
        "Esc".bold(),
        Span::raw(" quit  ·  "),
        "↑".bold(),
        Span::raw(" "),
        "↓".bold(),
        Span::raw(" move"),
    ]);

    let footer = Paragraph::new(line).style(Style::new().dim());
    frame.render_widget(footer, area);
}
