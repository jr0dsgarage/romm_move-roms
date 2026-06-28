use std::{
    collections::{BTreeMap, HashSet},
    io,
    path::Path,
    sync::mpsc,
    time::Duration,
};

use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, MouseButton, MouseEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Cell, Clear, Gauge, List, ListItem, ListState, Paragraph, Row,
        Scrollbar, ScrollbarOrientation, ScrollbarState, Table, TableState,
    },
};

use crate::model::{Confidence, PlannedMove, Summary};

pub struct AppView {
    pub source_root: std::path::PathBuf,
    pub output_root: std::path::PathBuf,
    pub plan: Vec<PlannedMove>,
    pub summary: Summary,
}

#[derive(Debug, Clone)]
pub struct LoadingUpdate {
    pub phase: String,
    pub phase_index: usize,
    pub phase_total: usize,
    pub current: String,
    pub processed: usize,
    pub total: usize,
}

pub fn run_loading_modal(progress_rx: mpsc::Receiver<LoadingUpdate>) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut state = LoadingUpdate {
        phase: String::from("Starting"),
        phase_index: 0,
        phase_total: 0,
        current: String::from("Preparing scan..."),
        processed: 0,
        total: 0,
    };

    loop {
        terminal.draw(|frame| draw_loading(frame, &state))?;

        match progress_rx.recv_timeout(Duration::from_millis(80)) {
            Ok(update) => state = update,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if event::poll(Duration::from_millis(1))? {
                    let _ = event::read()?;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    Ok(())
}

pub fn run(view: AppView) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = UiState::new(view);
    let res = run_loop(&mut terminal, &mut app);

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    res?;

    if app.confirmed_move {
        println!("Dry-run only. Move confirmed, but no files were moved.");
    } else {
        println!("Dry-run ended. No files were moved.");
    }

    println!(
        "Scanned: {} | Classified: {} | Unclassified: {} | Conflicts: {} | Skipped: {}",
        app.view.summary.scanned_files,
        app.view.summary.planned_moves,
        app.view.summary.unclassified,
        app.view.summary.conflicts,
        app.view.summary.skipped
    );

    Ok(())
}

struct UiState {
    view: AppView,
    selected: usize,
    list_offset: usize,
    expanded_slugs: HashSet<String>,
    expanded_games: HashSet<String>,
    disabled_plan_indices: HashSet<usize>,
    confirmed_move: bool,
    frame_area: Rect,
    scrollbar_dragging: bool,
}

impl UiState {
    fn new(view: AppView) -> Self {
        let expanded_slugs = HashSet::new();

        Self {
            view,
            selected: 0,
            list_offset: 0,
            expanded_slugs,
            expanded_games: HashSet::new(),
            disabled_plan_indices: HashSet::new(),
            confirmed_move: false,
            frame_area: Rect::new(0, 0, 0, 0),
            scrollbar_dragging: false,
        }
    }
}

#[derive(Clone)]
enum DisplayRow {
    SlugHeader {
        slug: String,
        count: usize,
        expanded: bool,
    },
    GameHeader {
        slug: String,
        game: String,
        count: usize,
        confidence: Confidence,
        reason: String,
        has_conflict: bool,
        expanded: bool,
    },
    Item {
        plan_index: usize,
    },
}

#[derive(Clone, Copy)]
enum Pane {
    Input,
    Output,
    Toggle,
}

#[derive(Clone, Copy)]
struct BodyClick {
    row_index: usize,
    pane: Pane,
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut UiState,
) -> Result<()> {
    loop {
        terminal.draw(|frame| draw_ui(frame, app))?;

        if !event::poll(Duration::from_millis(100))? {
            continue;
        }

        match event::read()? {
            Event::Key(key) => match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break,
                KeyCode::Char('n') => {
                    app.confirmed_move = false;
                    break;
                }
                KeyCode::Char('y') => {
                    app.confirmed_move = true;
                    break;
                }
                KeyCode::Down => move_down(app),
                KeyCode::Up => move_up(app),
                KeyCode::Enter | KeyCode::Char(' ') => toggle_section(app),
                KeyCode::Right => expand_section(app),
                KeyCode::Left => collapse_section(app),
                KeyCode::Char('x') => toggle_selected_item_enabled(app),
                KeyCode::PageDown => {
                    for _ in 0..10 {
                        move_down(app);
                    }
                }
                KeyCode::PageUp => {
                    for _ in 0..10 {
                        move_up(app);
                    }
                }
                _ => {}
            },
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::ScrollDown => move_down(app),
                MouseEventKind::ScrollUp => move_up(app),
                MouseEventKind::Down(MouseButton::Left) => {
                    if handle_scrollbar_mouse(app, mouse.column, mouse.row) {
                        app.scrollbar_dragging = true;
                        continue;
                    }

                    if clicked_yes(app.frame_area, mouse.column, mouse.row) {
                        app.confirmed_move = true;
                        break;
                    }
                    if clicked_no(app.frame_area, mouse.column, mouse.row) {
                        app.confirmed_move = false;
                        break;
                    }

                    handle_body_click(app, mouse.column, mouse.row);
                }
                MouseEventKind::Drag(MouseButton::Left) => {
                    if app.scrollbar_dragging {
                        let _ = handle_scrollbar_mouse(app, mouse.column, mouse.row);
                    }
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    app.scrollbar_dragging = false;
                }
                _ => {}
            },
            _ => {}
        }
    }

    Ok(())
}

fn move_down(app: &mut UiState) {
    let rows = build_display_rows(app);
    if rows.is_empty() {
        return;
    }

    if app.selected + 1 < rows.len() {
        app.selected += 1;
    }
}

fn move_up(app: &mut UiState) {
    if app.selected > 0 {
        app.selected -= 1;
    }
}

fn toggle_section(app: &mut UiState) {
    let rows = build_display_rows(app);
    let Some(row) = rows.get(app.selected) else {
        return;
    };

    match row {
        DisplayRow::SlugHeader { slug, expanded, .. } => {
            if *expanded {
                app.expanded_slugs.remove(slug);
            } else {
                app.expanded_slugs.insert(slug.clone());
            }
        }
        DisplayRow::GameHeader {
            slug,
            game,
            expanded,
            ..
        } => {
            let key = game_key(slug, game);
            if *expanded {
                app.expanded_games.remove(&key);
            } else {
                app.expanded_games.insert(key);
            }
        }
        DisplayRow::Item { .. } => {}
    }

    clamp_selected(app);
}

fn expand_section(app: &mut UiState) {
    let rows = build_display_rows(app);
    let Some(row) = rows.get(app.selected) else {
        return;
    };

    match row {
        DisplayRow::SlugHeader { slug, expanded, .. } => {
            if !expanded {
                app.expanded_slugs.insert(slug.clone());
            }
        }
        DisplayRow::GameHeader {
            slug,
            game,
            expanded,
            ..
        } => {
            if !expanded {
                app.expanded_games.insert(game_key(slug, game));
            }
        }
        DisplayRow::Item { .. } => {}
    }
}

fn collapse_section(app: &mut UiState) {
    let rows = build_display_rows(app);
    let Some(row) = rows.get(app.selected) else {
        return;
    };

    match row {
        DisplayRow::SlugHeader { slug, expanded, .. } => {
            if *expanded {
                app.expanded_slugs.remove(slug);
            }
        }
        DisplayRow::GameHeader {
            slug,
            game,
            expanded,
            ..
        } => {
            if *expanded {
                app.expanded_games.remove(&game_key(slug, game));
            }
        }
        DisplayRow::Item { .. } => {}
    }

    clamp_selected(app);
}

fn toggle_selected_item_enabled(app: &mut UiState) {
    let rows = build_display_rows(app);
    let Some(DisplayRow::Item { plan_index }) = rows.get(app.selected) else {
        return;
    };

    toggle_item_enabled(app, *plan_index);
}

fn toggle_item_enabled(app: &mut UiState, plan_index: usize) {
    if app.disabled_plan_indices.contains(&plan_index) {
        app.disabled_plan_indices.remove(&plan_index);
    } else {
        app.disabled_plan_indices.insert(plan_index);
    }
}

fn clamp_selected(app: &mut UiState) {
    let rows = build_display_rows(app);
    if rows.is_empty() {
        app.selected = 0;
        app.list_offset = 0;
        return;
    }

    if app.selected >= rows.len() {
        app.selected = rows.len() - 1;
    }
}

fn draw_ui(frame: &mut Frame, app: &mut UiState) {
    app.frame_area = frame.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(5),
            Constraint::Length(3),
        ])
        .split(frame.area());

    draw_header(frame, app, chunks[0]);
    draw_body(frame, app, chunks[1]);
    draw_footer(frame, app, chunks[2]);
}

fn draw_header(frame: &mut Frame, app: &UiState, area: Rect) {
    let header_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    let left = Paragraph::new(vec![
        Line::from(vec![Span::styled(
            "Source",
            Style::default().fg(Color::Cyan),
        )]),
        Line::from(app.view.source_root.display().to_string()),
    ])
    .alignment(Alignment::Left)
    .block(Block::default().borders(Borders::ALL));

    let right = Paragraph::new(vec![
        Line::from(vec![Span::styled(
            "Destination",
            Style::default().fg(Color::Cyan),
        )]),
        Line::from(app.view.output_root.display().to_string()),
    ])
    .alignment(Alignment::Right)
    .block(Block::default().borders(Borders::ALL));

    frame.render_widget(left, header_chunks[0]);
    frame.render_widget(right, header_chunks[1]);
}

fn draw_body(frame: &mut Frame, app: &mut UiState, area: Rect) {
    let rows = build_display_rows(app);

    let body_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(46),
            Constraint::Length(5),
            Constraint::Percentage(49),
        ])
        .split(area);

    // Reserve one content line for in-pane headers so both left and right panes align.
    let viewport_height = body_chunks[0].height.saturating_sub(3) as usize;
    if viewport_height == 0 {
        return;
    }

    clamp_selected(app);

    if app.selected < app.list_offset {
        app.list_offset = app.selected;
    }
    if app.selected >= app.list_offset + viewport_height {
        app.list_offset = app.selected + 1 - viewport_height;
    }

    let visible_rows: Vec<DisplayRow> = rows
        .iter()
        .skip(app.list_offset)
        .take(viewport_height)
        .cloned()
        .collect();

    let mut input_state = ListState::default();
    if !visible_rows.is_empty() {
        let relative_selected = app.selected.saturating_sub(app.list_offset);
        input_state.select(Some(relative_selected + 1));
    }

    let mut output_state = TableState::default();
    if !visible_rows.is_empty() {
        output_state.select(Some(app.selected.saturating_sub(app.list_offset)));
    }

    let mut toggle_state = TableState::default();
    if !visible_rows.is_empty() {
        // +1 because gutter includes a leading spacer row to align with output table header.
        toggle_state.select(Some(app.selected.saturating_sub(app.list_offset) + 1));
    }

    let mut toggle_rows: Vec<Row> = vec![Row::new(vec![Cell::from(String::new())])];
    toggle_rows.extend(visible_rows
        .iter()
        .map(|row| match row {
            DisplayRow::SlugHeader { .. } | DisplayRow::GameHeader { .. } => {
                Row::new(vec![Cell::from(String::new())])
            }
            DisplayRow::Item { plan_index } => {
                let checked = !app.disabled_plan_indices.contains(plan_index);
                let checkbox = if checked { "[x]" } else { "[ ]" };
                let checkbox_style = if checked {
                    Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Red).add_modifier(Modifier::DIM)
                };

                Row::new(vec![Cell::from(Line::from(checkbox).alignment(Alignment::Center)).style(checkbox_style)])
            }
        })
    );

    let mut input_items: Vec<ListItem> = vec![
        ListItem::new(Line::from(vec![Span::styled(
            "Path",
            Style::default().fg(Color::Yellow),
        )])),
    ];

    input_items.extend(visible_rows.iter()
        .map(|row| match row {
            DisplayRow::SlugHeader {
                slug,
                count,
                expanded,
            } => {
                let icon = if *expanded { "▼" } else { "▶" };
                ListItem::new(Line::from(vec![Span::styled(
                    format!("{} {} ({})", icon, slug, count),
                    Style::default().fg(Color::Cyan),
                )]))
            }
            DisplayRow::GameHeader {
                game,
                count,
                expanded,
                ..
            } => {
                let icon = if *expanded { "▼" } else { "▶" };
                ListItem::new(Line::from(vec![Span::styled(
                    format!("  {} {} ({})", icon, game, count),
                    Style::default().fg(Color::Magenta),
                )]))
            }
            DisplayRow::Item { plan_index } => {
                let item = &app.view.plan[*plan_index];
                let source = relative_display(&app.view.source_root, &item.source);
                ListItem::new(Line::from(format!("    {}", source)))
            }
        })
    );

    let output_rows: Vec<Row> = visible_rows
        .iter()
        .map(|row| match row {
            DisplayRow::SlugHeader {
                slug,
                count,
                expanded,
            } => {
                let icon = if *expanded { "▼" } else { "▶" };
                Row::new(vec![
                    Cell::from(format!("{} {} ({})", icon, slug, count)),
                    Cell::from(String::new()),
                    Cell::from(String::new()),
                    Cell::from(String::new()),
                ])
                .style(Style::default().fg(Color::Cyan))
            }
            DisplayRow::GameHeader {
                game,
                count,
                confidence,
                reason,
                has_conflict,
                expanded,
                ..
            } => {
                let icon = if *expanded { "▼" } else { "▶" };
                Row::new(vec![
                    Cell::from(format!("  {} {} ({})", icon, game, count)),
                    Cell::from(confidence.as_str()).style(confidence_style(*confidence, true)),
                    Cell::from(reason.clone()),
                    Cell::from(if *has_conflict { "[CONFLICT]" } else { "" }),
                ])
                .style(Style::default().fg(Color::Magenta))
            }
            DisplayRow::Item { plan_index } => {
                let item = &app.view.plan[*plan_index];
                let marker = if item.has_conflict { " [CONFLICT]" } else { "" };
                let checked = !app.disabled_plan_indices.contains(plan_index);
                let row_style = transfer_state_style(checked);

                let path = item
                    .destination
                    .as_ref()
                    .map(|dst| relative_display(&app.view.output_root, dst))
                    .unwrap_or_else(|| String::from("Unclassified"));

                Row::new(vec![
                    Cell::from(format!("    {}", path)),
                    Cell::from(item.confidence.as_str())
                        .style(confidence_style(item.confidence, checked)),
                    Cell::from(item.reason.clone()),
                    Cell::from(marker).style(if item.has_conflict {
                        Style::default().fg(Color::Red)
                    } else {
                        Style::default()
                    }),
                ])
                .style(row_style)
            }
        })
        .collect();

    let input_list = List::new(input_items)
        .block(
            Block::default()
                .title("Input files")
                .borders(Borders::ALL),
        )
        .highlight_style(Style::default().bg(Color::DarkGray))
        .highlight_symbol("> ");

    frame.render_stateful_widget(input_list, body_chunks[0], &mut input_state);
    let toggle_table = Table::new(toggle_rows, [Constraint::Length(5)])
        .block(Block::default().title("Sel").borders(Borders::ALL))
        .highlight_style(Style::default().bg(Color::DarkGray))
        .highlight_symbol("")
        .column_spacing(0);

    frame.render_stateful_widget(toggle_table, body_chunks[1], &mut toggle_state);

    let output_table = Table::new(
        output_rows,
        [
            Constraint::Min(50),
            Constraint::Length(10),
            Constraint::Min(18),
            Constraint::Length(11),
        ],
    )
    .header(
        Row::new(vec!["Path", "Confidence", "Reason", "Conflict"]).style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(ratatui::style::Modifier::BOLD),
        ),
    )
    .block(
        Block::default()
            .title("Output preview (relative)")
            .borders(Borders::ALL),
    )
    .highlight_style(Style::default().bg(Color::DarkGray))
    .highlight_symbol("> ")
    .column_spacing(1);

    frame.render_stateful_widget(output_table, body_chunks[2], &mut output_state);

    let mut scrollbar_state = ScrollbarState::new(rows.len()).position(app.list_offset);
    let scrollbar = Scrollbar::default().orientation(ScrollbarOrientation::VerticalRight);
    frame.render_stateful_widget(scrollbar, body_chunks[2], &mut scrollbar_state);
}

fn draw_footer(frame: &mut Frame, app: &UiState, area: Rect) {
    let footer_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(20), Constraint::Length(34)])
        .split(area);

    let total_items = app.view.plan.len();
    let disabled_items = app.disabled_plan_indices.len();
    let enabled_items = total_items.saturating_sub(disabled_items);

    let left = Paragraph::new(Line::from(format!(
        "Scanned: {} | Classified: {} | Unclassified: {} | Conflicts: {} | Skipped: {} | Enabled: {} | Disabled: {}",
        app.view.summary.scanned_files,
        app.view.summary.planned_moves,
        app.view.summary.unclassified,
        app.view.summary.conflicts,
        app.view.summary.skipped,
        enabled_items,
        disabled_items
    )))
    .block(Block::default().borders(Borders::ALL));

    let right = Paragraph::new(Line::from(vec![
        Span::raw("Move ROMS? "),
        Span::styled("[ Y ]", Style::default().fg(Color::Green)),
        Span::raw("  "),
        Span::styled("[ N ]", Style::default().fg(Color::Red)),
    ]))
    .alignment(Alignment::Right)
    .block(Block::default().borders(Borders::ALL));

    frame.render_widget(left, footer_chunks[0]);
    frame.render_widget(right, footer_chunks[1]);
}

fn footer_action_rects(area: Rect) -> (Rect, Rect) {
    let footer_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(3),
        ])
        .split(area);

    let bottom = footer_chunks[2];
    let right_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(20), Constraint::Length(34)])
        .split(bottom);

    let prompt = right_chunks[1];
    let y = Rect {
        x: prompt.x + 13,
        y: prompt.y + 1,
        width: 5,
        height: 1,
    };
    let n = Rect {
        x: prompt.x + 20,
        y: prompt.y + 1,
        width: 5,
        height: 1,
    };

    (y, n)
}

fn clicked_yes(area: Rect, x: u16, y: u16) -> bool {
    let (yes, _) = footer_action_rects(area);
    x >= yes.x && x < yes.x + yes.width && y == yes.y
}

fn clicked_no(area: Rect, x: u16, y: u16) -> bool {
    let (_, no) = footer_action_rects(area);
    x >= no.x && x < no.x + no.width && y == no.y
}

fn handle_body_click(app: &mut UiState, x: u16, y: u16) {
    let Some(click) = clicked_body_row_info(app, x, y) else {
        return;
    };

    app.selected = click.row_index;

    let rows = build_display_rows(app);
    let Some(row) = rows.get(click.row_index) else {
        return;
    };

    match row {
        DisplayRow::SlugHeader { slug, expanded, .. } => {
            if *expanded {
                app.expanded_slugs.remove(slug);
            } else {
                app.expanded_slugs.insert(slug.clone());
            }
        }
        DisplayRow::GameHeader {
            slug,
            game,
            expanded,
            ..
        } => {
            let key = game_key(slug, game);
            if *expanded {
                app.expanded_games.remove(&key);
            } else {
                app.expanded_games.insert(key);
            }
        }
        DisplayRow::Item { plan_index } => {
            if matches!(click.pane, Pane::Toggle) {
                toggle_item_enabled(app, *plan_index);
            }
        }
    }

    clamp_selected(app);
}

fn clicked_body_row_info(app: &UiState, x: u16, y: u16) -> Option<BodyClick> {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(5),
            Constraint::Length(3),
        ])
        .split(app.frame_area);

    let body = chunks[1];
    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(46),
            Constraint::Length(5),
            Constraint::Percentage(49),
        ])
        .split(body);

    let in_left = contains(panes[0], x, y);
    let in_middle = contains(panes[1], x, y);
    let in_right = contains(panes[2], x, y);
    if !in_left && !in_middle && !in_right {
        return None;
    }

    let (pane, pane_type) = if in_left {
        (panes[0], Pane::Input)
    } else if in_middle {
        (panes[1], Pane::Toggle)
    } else {
        (panes[2], Pane::Output)
    };
    let content_top = pane.y.saturating_add(1);
    let content_height = pane.height.saturating_sub(2);
    if content_height == 0 {
        return None;
    }

    if y < content_top || y >= content_top.saturating_add(content_height) {
        return None;
    }

    if y == content_top {
        return None;
    }

    let row_in_view = (y - content_top - 1) as usize;
    let rows = build_display_rows(app);
    let global_index = app.list_offset + row_in_view;
    if global_index >= rows.len() {
        return None;
    }

    Some(BodyClick {
        row_index: global_index,
        pane: pane_type,
    })
}

fn handle_scrollbar_mouse(app: &mut UiState, x: u16, y: u16) -> bool {
    let rows_len = build_display_rows(app).len();
    if rows_len == 0 {
        return false;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(5),
            Constraint::Length(3),
        ])
        .split(app.frame_area);

    let body = chunks[1];
    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(46),
            Constraint::Length(5),
            Constraint::Percentage(49),
        ])
        .split(body);

    let output = panes[2];
    if output.width < 3 || output.height < 3 {
        return false;
    }

    let scrollbar_x = output.x.saturating_add(output.width.saturating_sub(1));
    let track_top = output.y.saturating_add(1);
    let track_height = output.height.saturating_sub(2);
    if track_height == 0 {
        return false;
    }

    if x != scrollbar_x || y < track_top || y >= track_top.saturating_add(track_height) {
        return false;
    }

    let viewport_height = output.height.saturating_sub(3) as usize;
    if viewport_height == 0 {
        return false;
    }

    let max_offset = rows_len.saturating_sub(viewport_height);
    if max_offset == 0 {
        app.list_offset = 0;
        app.selected = 0;
        return true;
    }

    let local_y = (y - track_top) as f64;
    let denom = (track_height.saturating_sub(1)) as f64;
    let ratio = if denom <= 0.0 { 0.0 } else { (local_y / denom).clamp(0.0, 1.0) };

    let new_offset = (ratio * max_offset as f64).round() as usize;
    app.list_offset = new_offset.min(max_offset);
    app.selected = app.list_offset.min(rows_len.saturating_sub(1));

    true
}

fn contains(rect: Rect, x: u16, y: u16) -> bool {
    x >= rect.x && x < rect.x.saturating_add(rect.width) && y >= rect.y && y < rect.y.saturating_add(rect.height)
}

fn relative_display(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    let mut text = rel.display().to_string().replace('/', "\\");

    if text == "." || text.is_empty() {
        return String::from("\\");
    }

    if !text.starts_with('\\') {
        text.insert(0, '\\');
    }

    text
}

fn build_display_rows(app: &UiState) -> Vec<DisplayRow> {
    let mut grouped: BTreeMap<String, BTreeMap<String, Vec<usize>>> = BTreeMap::new();

    for (index, item) in app.view.plan.iter().enumerate() {
        let slug = item
            .platform_slug
            .clone()
            .unwrap_or_else(|| String::from("unclassified"));
        let game = infer_game_name(item, &app.view.output_root);
        grouped
            .entry(slug)
            .or_default()
            .entry(game)
            .or_default()
            .push(index);
    }

    let mut rows = Vec::new();
    for (slug, games) in grouped {
        let slug_count: usize = games.values().map(std::vec::Vec::len).sum();
        let expanded = app.expanded_slugs.contains(&slug);
        rows.push(DisplayRow::SlugHeader {
            slug: slug.clone(),
            count: slug_count,
            expanded,
        });

        if expanded {
            for (game, indices) in games {
                let game_expanded = app.expanded_games.contains(&game_key(&slug, &game));
                let summary_item = indices
                    .first()
                    .and_then(|index| app.view.plan.get(*index));
                rows.push(DisplayRow::GameHeader {
                    slug: slug.clone(),
                    game: game.clone(),
                    count: indices.len(),
                    confidence: summary_item
                        .map(|item| item.confidence)
                        .unwrap_or(Confidence::Unknown),
                    reason: summary_item
                        .map(|item| item.reason.clone())
                        .unwrap_or_else(|| String::from("no summary")),
                    has_conflict: indices
                        .iter()
                        .any(|index| app.view.plan[*index].has_conflict),
                    expanded: game_expanded,
                });

                if game_expanded {
                    rows.extend(indices.into_iter().map(|plan_index| DisplayRow::Item { plan_index }));
                }
            }
        }
    }

    rows
}

fn game_key(slug: &str, game: &str) -> String {
    format!("{}::{}", slug, game)
}

fn infer_game_name(item: &PlannedMove, output_root: &Path) -> String {
    if let Some(destination) = &item.destination {
        if let Ok(rel) = destination.strip_prefix(output_root) {
            let mut components = rel.iter().filter_map(|c| c.to_str());
            let _slug = components.next();
            if let Some(game) = components.next() {
                return game.to_string();
            }
        }
    }

    item.source
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| String::from("unknown-game"))
}

fn draw_loading(frame: &mut Frame, state: &LoadingUpdate) {
    let area = frame.area();
    frame.render_widget(
        Block::default()
            .title("ROM Dry-Run Preview")
            .borders(Borders::ALL),
        area,
    );

    let modal = centered_rect(80, 30, area);
    frame.render_widget(Clear, modal);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Min(0),
        ])
        .split(inner_modal(modal));

    frame.render_widget(
        Block::default()
            .title("Working")
            .borders(Borders::ALL),
        modal,
    );

    let phase = Paragraph::new(format!("Phase: {}", state.phase));
    let phase_counter = Paragraph::new(format!(
        "Phase {}/{}",
        state.phase_index.max(1),
        state.phase_total.max(1)
    ));
    let current = Paragraph::new(format!("Current: {}", state.current));
    let detail = Paragraph::new(format!(
        "Phase progress: {} / {}",
        state.processed,
        state.total
    ));

    let ratio = if state.total == 0 {
        0.0
    } else {
        (state.processed as f64 / state.total as f64).clamp(0.0, 1.0)
    };

    let bar = Gauge::default()
        .block(Block::default().borders(Borders::ALL).title("Phase progress"))
        .style(Style::default().bg(Color::DarkGray))
        .gauge_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .ratio(ratio)
        .label(format!("{:>3}%", (ratio * 100.0) as u32));

    frame.render_widget(phase, chunks[0]);
    frame.render_widget(phase_counter, chunks[1]);
    frame.render_widget(current, chunks[2]);
    frame.render_widget(detail, chunks[3]);
    frame.render_widget(bar, chunks[4]);
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

fn inner_modal(area: Rect) -> Rect {
    Rect {
        x: area.x.saturating_add(1),
        y: area.y.saturating_add(1),
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    }
}

fn transfer_state_style(enabled: bool) -> Style {
    if enabled {
        Style::default()
    } else {
        Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM)
    }
}

fn confidence_style(confidence: Confidence, enabled: bool) -> Style {
    let base = match confidence {
        Confidence::Exact => Style::default().fg(Color::Green),
        Confidence::High => Style::default().fg(Color::Cyan),
        Confidence::Ambiguous => Style::default().fg(Color::Yellow),
        Confidence::Unknown => Style::default().fg(Color::Red),
    };

    if enabled {
        base.add_modifier(Modifier::BOLD)
    } else {
        base.add_modifier(Modifier::DIM)
    }
}
