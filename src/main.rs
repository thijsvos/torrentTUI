mod app;
mod config;
mod engine;
mod types;
mod ui;

use std::io;

use anyhow::Result;
use app::App;
use clap::Parser;
use crossterm::{
    event::{
        DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEventKind,
        KeyModifiers, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use engine::torrent::EngineCommand;
use futures::StreamExt;
use ratatui::{backend::CrosstermBackend, Terminal};
use tokio::sync::mpsc;
use types::AppMode;
use ui::input::{validate_torrent_source, InputWidget};

#[derive(Parser)]
#[command(name = "torrenttui", about = "Terminal BitTorrent client")]
struct Cli {
    /// Magnet link or .torrent file path to add on startup
    torrent_source: Option<String>,

    /// Download directory override
    #[arg(short, long)]
    download_dir: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Set up panic hook to restore terminal
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        original_hook(panic_info);
    }));

    // Set up logging to file
    let log_dir = config::Config::config_dir();
    std::fs::create_dir_all(&log_dir)?;
    let log_file = std::fs::File::create(log_dir.join("torrenttui.log"))?;
    tracing_subscriber::fmt()
        .with_writer(log_file)
        .with_ansi(false)
        .init();

    let cli = Cli::parse();

    // Load config
    let mut config = config::Config::load().unwrap_or_default();
    if let Some(ref dir) = cli.download_dir {
        config.general.download_dir = dir.clone();
    }

    // Terminal setup
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_app(&mut terminal, cli, config).await;

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(e) = result {
        eprintln!("Error: {}", e);
    }

    Ok(())
}

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    cli: Cli,
    config: config::Config,
) -> Result<()> {
    let mut app = App::new();
    let mut input_widget = InputWidget::new();

    // Apply config speed limits
    app.speed_limit_download_kbps = config.network.max_download_speed_kbps;
    app.speed_limit_upload_kbps = config.network.max_upload_speed_kbps;

    // Set up engine channels
    let (cmd_tx, cmd_rx) = mpsc::channel::<EngineCommand>(32);
    let (state_tx, mut state_rx) = mpsc::channel::<Vec<types::TorrentInfo>>(4);
    let (msg_tx, mut msg_rx) = mpsc::channel::<String>(16);

    // Spawn engine
    let engine_config = config.clone();
    tokio::spawn(async move {
        if let Err(e) = engine::torrent::run_engine(engine_config, cmd_rx, state_tx, msg_tx).await {
            tracing::error!("Engine error: {}", e);
        }
    });

    // Handle CLI torrent source (magnet or .torrent file)
    if let Some(ref source) = cli.torrent_source {
        if validate_torrent_source(source).is_ok() {
            let _ = cmd_tx.send(EngineCommand::AddTorrent(source.clone())).await;
        }
    }

    // Load session and re-add saved torrents
    let session_data = engine::session::SessionData::load().unwrap_or_default();
    for saved in &session_data.torrents {
        let _ = cmd_tx
            .send(EngineCommand::AddTorrent(saved.magnet_link.clone()))
            .await;
    }

    let download_dir = config.general.download_dir.clone();
    let mut event_stream = EventStream::new();
    let mut auto_save_interval = tokio::time::interval(std::time::Duration::from_secs(60));
    // Target ~30 FPS for smooth UI; the tick just caps the frame rate
    let mut frame_interval = tokio::time::interval(std::time::Duration::from_millis(33));
    let mut needs_render = true;

    loop {
        // Drain any pending state updates and messages without blocking
        while let Ok(torrents) = state_rx.try_recv() {
            app.torrents = torrents;
            app.restore_selection();
            needs_render = true;
        }
        while let Ok(msg) = msg_rx.try_recv() {
            app.set_info(msg);
            needs_render = true;
        }

        app.clear_expired_messages();

        if needs_render {
            needs_render = false;
            terminal.draw(|f| {
                let chunks = ui::layout::get_layout(f.area());

                ui::layout::render_header(f, chunks[0]);

                match app.mode {
                    AppMode::Detail => {
                        ui::detail::render_detail(f, chunks[1], &app);
                        app.table_area = None;
                    }
                    _ => {
                        app.table_area = Some(chunks[1]);
                        ui::table::render_table(f, chunks[1], &mut app);
                    }
                }

                match app.mode {
                    AppMode::Input => {
                        ui::input::render_input(f, chunks[2], &input_widget);
                    }
                    AppMode::Filter => {
                        ui::layout::render_filter_bar(f, chunks[2], &app.filter_text);
                    }
                    AppMode::ThrottleInput => {
                        ui::layout::render_throttle_bar(
                            f,
                            chunks[2],
                            app.throttle_step,
                            &app.throttle_input_buf,
                        );
                    }
                    _ => {
                        ui::layout::render_status_bar(f, chunks[2], &app);
                    }
                }

                // Overlays
                if app.mode == AppMode::Help {
                    ui::help::render_help(f, f.area());
                }
                if app.mode == AppMode::ConfirmDelete {
                    let label = if app.has_marks() {
                        format!("{} selected torrents", app.marked_count())
                    } else {
                        app.selected_torrent()
                            .map(|t| t.name.clone())
                            .unwrap_or_default()
                    };
                    if !label.is_empty() {
                        ui::dialogs::render_delete_dialog(f, f.area(), &label);
                    }
                }
                if app.mode == AppMode::ConfirmQuit {
                    ui::dialogs::render_quit_dialog(f, f.area());
                }
            })?;
        }

        if app.should_quit {
            save_session(&app);
            let _ = cmd_tx.send(EngineCommand::Shutdown).await;
            return Ok(());
        }

        tokio::select! {
            // Terminal events — highest priority for responsiveness
            event = event_stream.next() => {
                match event {
                    Some(Ok(Event::Key(key))) => {
                        if key.kind != KeyEventKind::Press {
                            continue;
                        }
                        // Ctrl+C: first opens quit dialog, second force-quits
                        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                            if app.mode == AppMode::ConfirmQuit {
                                app.should_quit = true;
                            } else {
                                app.mode = AppMode::ConfirmQuit;
                            }
                            needs_render = true;
                            continue;
                        }
                        match app.mode {
                            AppMode::Input => handle_input_mode(&mut app, &mut input_widget, key, &cmd_tx).await,
                            AppMode::Normal => handle_normal_mode(&mut app, &mut input_widget, key, &cmd_tx).await,
                            AppMode::Detail => handle_detail_mode(&mut app, key, &cmd_tx).await,
                            AppMode::Help => handle_help_mode(&mut app, key),
                            AppMode::ConfirmDelete => handle_delete_mode(&mut app, key, &cmd_tx).await,
                            AppMode::ConfirmQuit => handle_quit_mode(&mut app, key),
                            AppMode::Filter => handle_filter_mode(&mut app, key),
                            AppMode::ThrottleInput => handle_throttle_mode(&mut app, key, &cmd_tx).await,
                        }
                        needs_render = true;
                    }
                    Some(Ok(Event::Mouse(mouse))) => {
                        if app.mode == AppMode::Normal {
                            if let MouseEventKind::Down(crossterm::event::MouseButton::Left) = mouse.kind {
                                if let Some(area) = app.table_area {
                                    // Table has 1-cell border top + 1-row header
                                    let content_y = area.y + 2;
                                    let content_bottom = area.y + area.height.saturating_sub(1);
                                    if mouse.row >= content_y && mouse.row < content_bottom
                                        && mouse.column >= area.x && mouse.column < area.x + area.width
                                    {
                                        let clicked_index = (mouse.row - content_y) as usize;
                                        let count = app.sorted_torrents().len();
                                        if clicked_index < count {
                                            app.selected_index = clicked_index;
                                            app.update_selected_id();
                                            app.table_state.select(Some(clicked_index));
                                            needs_render = true;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            // State update (blocking recv when nothing was drained above)
            Some(torrents) = state_rx.recv() => {
                app.torrents = torrents;
                app.restore_selection();
                needs_render = true;
            }
            // Engine messages
            Some(msg) = msg_rx.recv() => {
                app.set_info(msg);
                needs_render = true;
            }
            // Frame tick — caps at ~30 FPS, drives spinner & disk space
            _ = frame_interval.tick() => {
                app.tick_spinner();
                app.update_disk_space(&download_dir);
                needs_render = true;
            }
            // Auto-save session
            _ = auto_save_interval.tick() => {
                save_session(&app);
            }
        }
    }
}

fn save_session(app: &App) {
    let session = engine::session::SessionData {
        torrents: app
            .torrents
            .iter()
            .filter(|t| !t.magnet_link.is_empty())
            .map(|t| engine::session::SavedTorrent {
                magnet_link: t.magnet_link.clone(),
                download_path: std::path::PathBuf::new(),
                is_paused: matches!(t.status, types::TorrentStatus::Paused),
            })
            .collect(),
    };
    if let Err(e) = session.save() {
        tracing::error!("Failed to save session: {}", e);
    }
}

async fn handle_normal_mode(
    app: &mut App,
    input_widget: &mut InputWidget<'_>,
    key: crossterm::event::KeyEvent,
    cmd_tx: &mpsc::Sender<EngineCommand>,
) {
    match key.code {
        KeyCode::Char('q') => {
            app.mode = AppMode::ConfirmQuit;
        }
        KeyCode::Char('j') | KeyCode::Down => app.next(),
        KeyCode::Char('k') | KeyCode::Up => app.previous(),
        KeyCode::Char('a') => {
            app.mode = AppMode::Input;
            input_widget.clear();
        }
        KeyCode::Char('p') => {
            if app.has_marks() {
                let ids: Vec<usize> = app.marked_ids.iter().copied().collect();
                let paused_count = ids
                    .iter()
                    .filter(|id| {
                        app.torrents.iter().any(|t| {
                            t.id == **id && matches!(t.status, types::TorrentStatus::Paused)
                        })
                    })
                    .count();
                let should_resume = paused_count > ids.len() / 2;
                for id in &ids {
                    if should_resume {
                        let _ = cmd_tx.send(EngineCommand::Resume(*id)).await;
                    } else {
                        let _ = cmd_tx.send(EngineCommand::Pause(*id)).await;
                    }
                }
                app.clear_marks();
            } else if let Some(torrent) = app.selected_torrent() {
                let id = torrent.id;
                if torrent.throttle_paused {
                    let _ = cmd_tx.send(EngineCommand::Pause(id)).await;
                } else {
                    match torrent.status {
                        types::TorrentStatus::Downloading => {
                            let _ = cmd_tx.send(EngineCommand::Pause(id)).await;
                        }
                        types::TorrentStatus::Paused => {
                            let _ = cmd_tx.send(EngineCommand::Resume(id)).await;
                        }
                        _ => {}
                    }
                }
            }
        }
        KeyCode::Char('P') => {
            let all_paused = app
                .torrents
                .iter()
                .filter(|t| {
                    matches!(
                        t.status,
                        types::TorrentStatus::Downloading | types::TorrentStatus::Paused
                    )
                })
                .all(|t| matches!(t.status, types::TorrentStatus::Paused));

            if all_paused {
                let _ = cmd_tx.send(EngineCommand::ResumeAll).await;
            } else {
                let _ = cmd_tx.send(EngineCommand::PauseAll).await;
            }
        }
        KeyCode::Char('d') => {
            if !app.torrents.is_empty() {
                app.mode = AppMode::ConfirmDelete;
            }
        }
        KeyCode::Enter => {
            if !app.sorted_torrents().is_empty() {
                app.mode = AppMode::Detail;
                app.detail_tab_index = 0;
                app.detail_file_index = 0;
            }
        }
        KeyCode::Char('?') => {
            app.mode = AppMode::Help;
        }
        KeyCode::Tab => {
            app.sort_column = match app.sort_column {
                types::SortColumn::Index => types::SortColumn::Name,
                types::SortColumn::Name => types::SortColumn::Size,
                types::SortColumn::Size => types::SortColumn::Progress,
                types::SortColumn::Progress => types::SortColumn::Speed,
                types::SortColumn::Speed => types::SortColumn::Peers,
                types::SortColumn::Peers => types::SortColumn::Eta,
                types::SortColumn::Eta => types::SortColumn::Status,
                types::SortColumn::Status => types::SortColumn::Index,
            };
        }
        KeyCode::Char('r') => {
            app.sort_reversed = !app.sort_reversed;
        }
        KeyCode::Char('/') => {
            app.mode = AppMode::Filter;
            // Don't clear existing filter text - let user edit it
        }
        KeyCode::Char('t') => {
            app.mode = AppMode::ThrottleInput;
            app.throttle_step = 0;
            app.throttle_input_buf = if app.speed_limit_download_kbps > 0 {
                app.speed_limit_download_kbps.to_string()
            } else {
                String::new()
            };
        }
        KeyCode::Char(' ') => {
            app.toggle_mark();
            app.next();
        }
        KeyCode::Char('v') => {
            app.mark_all();
        }
        KeyCode::Char('V') => {
            app.clear_marks();
        }
        KeyCode::Esc => {
            app.clear_marks();
        }
        _ => {}
    }
}

async fn handle_input_mode(
    app: &mut App,
    input_widget: &mut InputWidget<'_>,
    key: crossterm::event::KeyEvent,
    cmd_tx: &mpsc::Sender<EngineCommand>,
) {
    match key.code {
        KeyCode::Esc => {
            app.mode = AppMode::Normal;
        }
        KeyCode::Enter => {
            let value = input_widget.value().trim().to_string();
            match validate_torrent_source(&value) {
                Ok(()) => {
                    let _ = cmd_tx.send(EngineCommand::AddTorrent(value)).await;
                    app.mode = AppMode::Normal;
                }
                Err(e) => {
                    app.set_error(e);
                    app.mode = AppMode::Normal;
                }
            }
        }
        _ => {
            input_widget.textarea.input(key);
        }
    }
}

async fn handle_detail_mode(
    app: &mut App,
    key: crossterm::event::KeyEvent,
    cmd_tx: &mpsc::Sender<EngineCommand>,
) {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.mode = AppMode::Normal;
        }
        KeyCode::Tab => {
            app.detail_tab_index = (app.detail_tab_index + 1) % 4;
            app.detail_file_index = 0;
            app.detail_peer_index = 0;
        }
        // Navigation (Files tab = 2, Peers tab = 3)
        KeyCode::Char('j') | KeyCode::Down => {
            if app.detail_tab_index == 2 {
                if let Some(torrent) = app.selected_torrent() {
                    let file_count = torrent.files.len();
                    if file_count > 0 {
                        app.detail_file_index = (app.detail_file_index + 1).min(file_count - 1);
                    }
                }
            } else if app.detail_tab_index == 3 {
                if let Some(torrent) = app.selected_torrent() {
                    let peer_count = torrent.peers.len();
                    if peer_count > 0 {
                        app.detail_peer_index = (app.detail_peer_index + 1).min(peer_count - 1);
                    }
                }
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if app.detail_tab_index == 2 {
                app.detail_file_index = app.detail_file_index.saturating_sub(1);
            } else if app.detail_tab_index == 3 {
                app.detail_peer_index = app.detail_peer_index.saturating_sub(1);
            }
        }
        KeyCode::Char(' ') => {
            if app.detail_tab_index == 2 {
                if let Some(torrent) = app.selected_torrent() {
                    let torrent_id = torrent.id;
                    let file_count = torrent.files.len();
                    if app.detail_file_index < file_count {
                        app.toggle_file_selection(torrent_id, app.detail_file_index);
                        let selected = app.selected_file_indices(torrent_id, file_count);
                        let _ = cmd_tx
                            .send(EngineCommand::SetSelectedFiles {
                                id: torrent_id,
                                file_indices: selected,
                            })
                            .await;
                    }
                }
            }
        }
        KeyCode::Char('S') => {
            if app.detail_tab_index == 2 {
                if let Some(torrent) = app.selected_torrent() {
                    let torrent_id = torrent.id;
                    let total_files = torrent.files.len();
                    let selected = app.selected_file_indices(torrent_id, total_files);
                    let _ = cmd_tx
                        .send(EngineCommand::SetSelectedFiles {
                            id: torrent_id,
                            file_indices: selected,
                        })
                        .await;
                }
            }
        }
        _ => {}
    }
}

fn handle_help_mode(app: &mut App, key: crossterm::event::KeyEvent) {
    match key.code {
        KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => {
            app.mode = AppMode::Normal;
        }
        _ => {}
    }
}

fn handle_filter_mode(app: &mut App, key: crossterm::event::KeyEvent) {
    match key.code {
        KeyCode::Esc => {
            // Clear filter and exit
            app.filter_text.clear();
            app.mode = AppMode::Normal;
            app.restore_selection();
        }
        KeyCode::Enter => {
            // Keep filter active, back to normal
            app.mode = AppMode::Normal;
            app.restore_selection();
        }
        KeyCode::Backspace => {
            app.filter_text.pop();
        }
        KeyCode::Char(c) => {
            app.filter_text.push(c);
        }
        _ => {}
    }
}

async fn handle_throttle_mode(
    app: &mut App,
    key: crossterm::event::KeyEvent,
    cmd_tx: &mpsc::Sender<EngineCommand>,
) {
    match key.code {
        KeyCode::Esc => {
            app.mode = AppMode::Normal;
        }
        KeyCode::Backspace => {
            app.throttle_input_buf.pop();
        }
        KeyCode::Char(c) if c.is_ascii_digit() => {
            app.throttle_input_buf.push(c);
        }
        KeyCode::Enter => {
            let value = app.throttle_input_buf.parse::<u64>().unwrap_or(0);
            if app.throttle_step == 0 {
                // Store download value, move to upload
                app.throttle_download_value = value;
                app.throttle_step = 1;
                app.throttle_input_buf = if app.speed_limit_upload_kbps > 0 {
                    app.speed_limit_upload_kbps.to_string()
                } else {
                    String::new()
                };
            } else {
                // Apply both limits
                app.throttle_upload_value = value;
                app.speed_limit_download_kbps = app.throttle_download_value;
                app.speed_limit_upload_kbps = app.throttle_upload_value;
                let _ = cmd_tx
                    .send(EngineCommand::SetSpeedLimits {
                        download_kbps: app.speed_limit_download_kbps,
                        upload_kbps: app.speed_limit_upload_kbps,
                    })
                    .await;
                app.mode = AppMode::Normal;
            }
        }
        _ => {}
    }
}

async fn handle_delete_mode(
    app: &mut App,
    key: crossterm::event::KeyEvent,
    cmd_tx: &mpsc::Sender<EngineCommand>,
) {
    match key.code {
        KeyCode::Char('k') => {
            if app.has_marks() {
                let ids: Vec<usize> = app.marked_ids.iter().copied().collect();
                for id in ids {
                    let _ = cmd_tx
                        .send(EngineCommand::Delete {
                            id,
                            delete_files: false,
                        })
                        .await;
                }
                app.clear_marks();
            } else if let Some(torrent) = app.selected_torrent() {
                let id = torrent.id;
                let _ = cmd_tx
                    .send(EngineCommand::Delete {
                        id,
                        delete_files: false,
                    })
                    .await;
            }
            app.mode = AppMode::Normal;
        }
        KeyCode::Char('d') => {
            if app.has_marks() {
                let ids: Vec<usize> = app.marked_ids.iter().copied().collect();
                for id in ids {
                    let _ = cmd_tx
                        .send(EngineCommand::Delete {
                            id,
                            delete_files: true,
                        })
                        .await;
                }
                app.clear_marks();
            } else if let Some(torrent) = app.selected_torrent() {
                let id = torrent.id;
                let _ = cmd_tx
                    .send(EngineCommand::Delete {
                        id,
                        delete_files: true,
                    })
                    .await;
            }
            app.mode = AppMode::Normal;
        }
        KeyCode::Char('c') | KeyCode::Esc => {
            app.mode = AppMode::Normal;
        }
        _ => {}
    }
}

fn handle_quit_mode(app: &mut App, key: crossterm::event::KeyEvent) {
    match key.code {
        KeyCode::Char('y') => {
            app.should_quit = true;
        }
        KeyCode::Char('n') | KeyCode::Esc => {
            app.mode = AppMode::Normal;
        }
        _ => {}
    }
}
