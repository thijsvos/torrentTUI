mod app;
mod config;
mod engine;
mod player;
mod types;
mod ui;

use std::io;

use anyhow::Result;
use app::App;
use clap::Parser;
use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, EventStream, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use engine::torrent::{EngineCommand, EngineInfo};
use futures::StreamExt;
use ratatui::{backend::CrosstermBackend, Terminal};
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;
use types::{AppMode, DetailTab};
use ui::input::{validate_torrent_source, InputWidget};

/// Speed-limit input cap in KB/s (10 GB/s). Prevents `kbps * 1024` overflow.
const MAX_SPEED_LIMIT_KBPS: u64 = 10_485_760;

/// Maximum digits accepted in the throttle input dialog. Combined with the
/// numeric cap above, anything past this gets truncated visually.
const MAX_THROTTLE_INPUT_DIGITS: usize = 8;

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
    // Set up panic hook to restore terminal. Disable mouse capture too —
    // otherwise the user's terminal will keep emitting mouse-event escape
    // codes after a crash until they `reset(1)`.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            DisableBracketedPaste
        );
        original_hook(panic_info);
    }));

    // Set up logging to file. Default filter is "torrenttui=warn" so librqbit
    // internals (peer IPs, tracker URLs, info hashes) don't get persisted to
    // disk by default. Users who want verbose logs can set RUST_LOG.
    let log_dir = config::Config::config_dir();
    std::fs::create_dir_all(&log_dir)?;
    let log_file = std::fs::File::create(log_dir.join("torrenttui.log"))?;
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("torrenttui=warn"));
    tracing_subscriber::fmt()
        .with_writer(log_file)
        .with_ansi(false)
        .with_env_filter(filter)
        .init();

    let cli = Cli::parse();

    // Load config. The error here is the I/O error of reading the file; a
    // parse error returns `(default, Some(warning))` so we can surface it.
    let (mut config, config_warning) = match config::Config::load() {
        Ok(pair) => pair,
        Err(e) => {
            tracing::warn!("Failed to load config, using defaults: {e}");
            (
                config::Config::default(),
                Some(format!("Config load failed: {e}")),
            )
        }
    };
    if let Some(ref dir) = cli.download_dir {
        config.general.download_dir = dir.clone();
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    // Bracketed paste is best-effort: terminals that don't support it (e.g.
    // some Windows consoles) just won't send Event::Paste, and that's fine.
    let _ = execute!(stdout, EnableBracketedPaste);
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_app(&mut terminal, cli, config, config_warning).await;

    let _ = execute!(terminal.backend_mut(), DisableBracketedPaste);
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

/// Send an engine command, surfacing failure to the user instead of dropping
/// it silently. If the engine task has died, the channel send returns an Err
/// and the user gets a status-bar message instead of an inert key press.
async fn send_cmd(tx: &mpsc::Sender<EngineCommand>, cmd: EngineCommand, app: &mut App) {
    if let Err(e) = tx.send(cmd).await {
        tracing::error!("engine channel send failed: {e}");
        app.set_error("Engine stopped responding".to_string());
    }
}

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    cli: Cli,
    config: config::Config,
    config_warning: Option<String>,
) -> Result<()> {
    let mut app = App::new();
    let mut input_widget = InputWidget::new();

    app.speed_limit_download_kbps = config.network.max_download_speed_kbps;
    app.speed_limit_upload_kbps = config.network.max_upload_speed_kbps;
    app.confirm_on_quit = config.general.confirm_on_quit;
    app.player_config = config.player.clone();

    if let Some(msg) = config_warning {
        app.set_error(msg);
    }

    let (cmd_tx, cmd_rx) = mpsc::channel::<EngineCommand>(32);
    let (state_tx, mut state_rx) = mpsc::channel::<Vec<types::TorrentInfo>>(4);
    let (msg_tx, mut msg_rx) = mpsc::channel::<String>(16);
    // Engine-to-UI one-shot facts (HTTP API base URL today; future use:
    // listening port, DHT state). Small capacity because messages are rare.
    let (info_tx, mut info_rx) = mpsc::channel::<EngineInfo>(4);

    let engine_config = config.clone();
    // Keep the JoinHandle so we can both detect engine death mid-run and
    // await its persistence flush on shutdown. Wrapped in Option so we can
    // consume it from either path.
    let mut engine_handle: Option<tokio::task::JoinHandle<()>> = Some(tokio::spawn(async move {
        if let Err(e) =
            engine::torrent::run_engine(engine_config, cmd_rx, state_tx, msg_tx, info_tx).await
        {
            tracing::error!("Engine error: {}", e);
        }
    }));

    if let Some(ref source) = cli.torrent_source {
        match validate_torrent_source(source) {
            Ok(()) => {
                send_cmd(&cmd_tx, EngineCommand::AddTorrent(source.clone()), &mut app).await;
            }
            Err(e) => app.set_error(e),
        }
    }

    let download_dir = config.general.download_dir.clone();
    let mut event_stream = EventStream::new();
    // Frame-rate cap for animations and timed-message aging; honors
    // `config.ui.refresh_rate_ms` (clamped to 16–1000 ms / ~60–1 FPS). Rendering
    // itself is change-driven via `needs_render`, so this only paces idle repaints.
    let mut frame_interval = tokio::time::interval(std::time::Duration::from_millis(
        config.ui.refresh_rate_ms.clamp(16, 1000),
    ));
    let mut needs_render = true;

    loop {
        // Detect engine death: previously a panic in the spawned task would
        // silently drop the JoinHandle and the UI would keep rendering stale
        // state with no indication. is_finished() is a cheap flag read.
        if engine_handle.as_ref().is_some_and(|h| h.is_finished()) {
            if let Some(h) = engine_handle.take() {
                match h.await {
                    Ok(()) => tracing::info!("Engine task ended"),
                    Err(e) => tracing::error!("Engine task panicked: {}", e),
                }
            }
            if !app.should_quit {
                app.set_error("Engine task ended unexpectedly; quitting".to_string());
                app.should_quit = true;
            }
        }

        while let Ok(torrents) = state_rx.try_recv() {
            app.handle_state_push(torrents);
            needs_render = true;
        }
        while let Ok(msg) = msg_rx.try_recv() {
            app.set_info(msg);
            needs_render = true;
        }
        while let Ok(info) = info_rx.try_recv() {
            apply_engine_info(&mut app, info);
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
                        ui::detail::render_detail(f, chunks[1], &mut app);
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
            send_cmd(&cmd_tx, EngineCommand::Shutdown, &mut app).await;
            // Give the engine task a chance to flush librqbit's persisted
            // state. The timeout caps how long we wait if it's stuck.
            if let Some(h) = engine_handle.take() {
                let _ = tokio::time::timeout(std::time::Duration::from_secs(5), h).await;
            }
            return Ok(());
        }

        tokio::select! {
            event = event_stream.next() => {
                match event {
                    Some(Ok(Event::Key(key))) => {
                        if key.kind != KeyEventKind::Press {
                            continue;
                        }
                        // Ctrl+C: first opens quit dialog, second force-quits
                        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                            let was_detail = app.mode == AppMode::Detail;
                            if app.mode == AppMode::ConfirmQuit {
                                app.should_quit = true;
                            } else {
                                app.mode = AppMode::ConfirmQuit;
                            }
                            if was_detail {
                                // Quit dialog overlays Detail — drop the heavy
                                // per-tick allocations until the user returns.
                                send_cmd(
                                    &cmd_tx,
                                    EngineCommand::SetDetailTorrent(None),
                                    &mut app,
                                )
                                .await;
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
                    Some(Ok(Event::Paste(s))) if app.mode == AppMode::Input => {
                        // Bracketed-paste payload (e.g. a magnet link from the
                        // clipboard). push_str filters control chars per-char
                        // so escape sequences in the paste never reach the
                        // buffer or the engine.
                        input_widget.push_str(&s);
                        needs_render = true;
                    }
                    Some(Ok(Event::Paste(s))) if app.mode == AppMode::Filter => {
                        for c in s.chars() {
                            if !c.is_control() {
                                app.push_filter_char(c);
                            }
                        }
                        needs_render = true;
                    }
                    Some(Ok(Event::Mouse(mouse)))
                        if app.mode == AppMode::Normal
                            && matches!(
                                mouse.kind,
                                MouseEventKind::Down(crossterm::event::MouseButton::Left)
                            ) =>
                    {
                        if let Some(area) = app.table_area {
                            // Layout: row 0 top-border, row 1 header, rows 2..h-2
                            // data, row h-1 bottom-border. `< content_bottom`
                            // (= area.y + h - 1) correctly excludes the bottom
                            // border row.
                            let content_y = area.y + 2;
                            let content_bottom = area.y + area.height.saturating_sub(1);
                            if mouse.row >= content_y
                                && mouse.row < content_bottom
                                && mouse.column >= area.x
                                && mouse.column < area.x + area.width
                            {
                                // Account for table scroll: the visible top row
                                // is the table state's offset.
                                let visible_offset = (mouse.row - content_y) as usize;
                                let clicked_index = app.table_state.offset() + visible_offset;
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
                    _ => {}
                }
            }
            Some(torrents) = state_rx.recv() => {
                app.handle_state_push(torrents);
                needs_render = true;
            }
            Some(msg) = msg_rx.recv() => {
                app.set_info(msg);
                needs_render = true;
            }
            Some(info) = info_rx.recv() => {
                apply_engine_info(&mut app, info);
                needs_render = true;
            }
            _ = frame_interval.tick() => {
                // Only burn a render frame when something actually animates.
                // Disk-space refresh is internally throttled to ~5s.
                let prev_disk = app.free_disk_space;
                app.update_disk_space(&download_dir);
                let disk_changed = prev_disk != app.free_disk_space;

                if app.has_fetching_metadata() {
                    app.tick_spinner();
                    needs_render = true;
                } else if disk_changed
                    || app.error_message.is_some()
                    || app.info_message.is_some()
                {
                    // Re-render so timed messages can age out cleanly.
                    needs_render = true;
                }
            }
        }
    }
}

async fn handle_normal_mode(
    app: &mut App,
    input_widget: &mut InputWidget,
    key: crossterm::event::KeyEvent,
    cmd_tx: &mpsc::Sender<EngineCommand>,
) {
    match key.code {
        KeyCode::Char('q') => {
            if app.confirm_on_quit_required() {
                app.mode = AppMode::ConfirmQuit;
            } else {
                app.should_quit = true;
            }
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
                // "Any paused (or throttle-paused) → resume all" is the
                // intuitive user model. The previous strict-majority check
                // gave a wrong answer on ties.
                let any_paused = ids.iter().any(|id| {
                    app.torrents.iter().any(|t| {
                        t.id == *id
                            && (matches!(t.status, types::TorrentStatus::Paused)
                                || t.throttle_paused)
                    })
                });
                // Send the whole batch as one message — the 32-slot channel
                // would block on send #33 otherwise, and the engine's own
                // state_tx send (4-slot) could deadlock while the UI waits.
                let cmd = if any_paused {
                    EngineCommand::ResumeMany(ids)
                } else {
                    EngineCommand::PauseMany(ids)
                };
                send_cmd(cmd_tx, cmd, app).await;
                app.clear_marks();
            } else if let Some(torrent) = app.selected_torrent() {
                let id = torrent.id;
                if torrent.throttle_paused {
                    send_cmd(cmd_tx, EngineCommand::Pause(id), app).await;
                } else {
                    match torrent.status {
                        types::TorrentStatus::Downloading => {
                            send_cmd(cmd_tx, EngineCommand::Pause(id), app).await;
                        }
                        types::TorrentStatus::Paused => {
                            send_cmd(cmd_tx, EngineCommand::Resume(id), app).await;
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

            let cmd = if all_paused {
                EngineCommand::ResumeAll
            } else {
                EngineCommand::PauseAll
            };
            send_cmd(cmd_tx, cmd, app).await;
        }
        KeyCode::Char('d') if !app.torrents.is_empty() => {
            app.mode = AppMode::ConfirmDelete;
        }
        KeyCode::Enter if !app.sorted_torrents().is_empty() => {
            app.mode = AppMode::Detail;
            app.detail_tab = DetailTab::Stats;
            app.detail_file_index = 0;
            app.detail_peer_index = 0;
            app.detail_peer_scroll_offset = 0;
            // Tell the engine which torrent we're viewing so the next
            // snapshot includes files/peers/info for only this one.
            let detail_id = app.selected_torrent_id;
            send_cmd(cmd_tx, EngineCommand::SetDetailTorrent(detail_id), app).await;
        }
        KeyCode::Char('?') => {
            app.mode = AppMode::Help;
        }
        KeyCode::Tab => {
            let next = app.sort_column.next();
            app.change_sort_column(next);
        }
        KeyCode::Char('r') => {
            app.toggle_sort_reversed();
        }
        KeyCode::Char('/') => {
            app.mode = AppMode::Filter;
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
    input_widget: &mut InputWidget,
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
                    send_cmd(cmd_tx, EngineCommand::AddTorrent(value), app).await;
                    app.mode = AppMode::Normal;
                }
                Err(e) => {
                    app.set_error(e);
                    app.mode = AppMode::Normal;
                }
            }
        }
        KeyCode::Backspace => {
            input_widget.pop();
        }
        KeyCode::Char(c) => {
            input_widget.push(c);
        }
        _ => {}
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
            // Detail mode left — stop the engine from materializing
            // files/peers for the (now-hidden) torrent.
            send_cmd(cmd_tx, EngineCommand::SetDetailTorrent(None), app).await;
        }
        KeyCode::Tab => {
            app.detail_tab = app.detail_tab.next();
            app.detail_file_index = 0;
            app.detail_peer_index = 0;
            app.detail_peer_scroll_offset = 0;
        }
        KeyCode::Char('j') | KeyCode::Down => match app.detail_tab {
            DetailTab::Files => {
                if let Some(torrent) = app.selected_torrent() {
                    let file_count = torrent.files.len();
                    if file_count > 0 {
                        app.detail_file_index = (app.detail_file_index + 1).min(file_count - 1);
                    }
                }
            }
            DetailTab::Peers => {
                if let Some(torrent) = app.selected_torrent() {
                    let peer_count = torrent.peers.len();
                    if peer_count > 0 {
                        app.detail_peer_index = (app.detail_peer_index + 1).min(peer_count - 1);
                    }
                }
            }
            _ => {}
        },
        KeyCode::Char('k') | KeyCode::Up => match app.detail_tab {
            DetailTab::Files => {
                app.detail_file_index = app.detail_file_index.saturating_sub(1);
            }
            DetailTab::Peers => {
                app.detail_peer_index = app.detail_peer_index.saturating_sub(1);
            }
            _ => {}
        },
        KeyCode::Char(' ') if app.detail_tab == DetailTab::Files => {
            if let Some(torrent) = app.selected_torrent() {
                let torrent_id = torrent.id;
                let file_count = torrent.files.len();
                if app.detail_file_index < file_count {
                    app.toggle_file_selection(torrent_id, app.detail_file_index);
                    let selected = app.selected_file_indices(torrent_id, file_count);
                    send_cmd(
                        cmd_tx,
                        EngineCommand::SetSelectedFiles {
                            id: torrent_id,
                            file_indices: selected,
                        },
                        app,
                    )
                    .await;
                }
            }
        }
        KeyCode::Char('s') if app.detail_tab == DetailTab::Files => {
            handle_stream_keypress(app);
        }
        KeyCode::Char('S') if app.detail_tab == DetailTab::Files => {
            if let Some(torrent) = app.selected_torrent() {
                let torrent_id = torrent.id;
                let total_files = torrent.files.len();
                // Don't send an empty file selection — librqbit may interpret
                // it as "deselect everything" and pause the torrent. This
                // happens when the torrent briefly returns to FetchingMetadata.
                if total_files == 0 {
                    app.set_info("No files to apply yet (still fetching metadata)".to_string());
                } else {
                    let selected = app.selected_file_indices(torrent_id, total_files);
                    send_cmd(
                        cmd_tx,
                        EngineCommand::SetSelectedFiles {
                            id: torrent_id,
                            file_indices: selected,
                        },
                        app,
                    )
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
            app.clear_filter();
            app.mode = AppMode::Normal;
        }
        KeyCode::Enter => {
            app.mode = AppMode::Normal;
            app.restore_selection();
        }
        KeyCode::Backspace => {
            app.pop_filter_char();
        }
        KeyCode::Char(c) => {
            app.push_filter_char(c);
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
        KeyCode::Char(c)
            if c.is_ascii_digit() && app.throttle_input_buf.len() < MAX_THROTTLE_INPUT_DIGITS =>
        {
            app.throttle_input_buf.push(c);
        }
        KeyCode::Enter => {
            let value = app
                .throttle_input_buf
                .parse::<u64>()
                .unwrap_or(0)
                .min(MAX_SPEED_LIMIT_KBPS);
            if app.throttle_step == 0 {
                app.throttle_download_value = value;
                app.throttle_step = 1;
                app.throttle_input_buf = if app.speed_limit_upload_kbps > 0 {
                    app.speed_limit_upload_kbps.to_string()
                } else {
                    String::new()
                };
            } else {
                app.throttle_upload_value = value;
                app.speed_limit_download_kbps = app.throttle_download_value;
                app.speed_limit_upload_kbps = app.throttle_upload_value;
                send_cmd(
                    cmd_tx,
                    EngineCommand::SetSpeedLimits {
                        download_kbps: app.speed_limit_download_kbps,
                        upload_kbps: app.speed_limit_upload_kbps,
                    },
                    app,
                )
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
                send_cmd(
                    cmd_tx,
                    EngineCommand::DeleteMany {
                        ids,
                        delete_files: false,
                    },
                    app,
                )
                .await;
                app.clear_marks();
            } else if let Some(torrent) = app.selected_torrent() {
                let id = torrent.id;
                send_cmd(
                    cmd_tx,
                    EngineCommand::Delete {
                        id,
                        delete_files: false,
                    },
                    app,
                )
                .await;
            }
            app.mode = AppMode::Normal;
        }
        KeyCode::Char('d') => {
            if app.has_marks() {
                let ids: Vec<usize> = app.marked_ids.iter().copied().collect();
                send_cmd(
                    cmd_tx,
                    EngineCommand::DeleteMany {
                        ids,
                        delete_files: true,
                    },
                    app,
                )
                .await;
                app.clear_marks();
            } else if let Some(torrent) = app.selected_torrent() {
                let id = torrent.id;
                send_cmd(
                    cmd_tx,
                    EngineCommand::Delete {
                        id,
                        delete_files: true,
                    },
                    app,
                )
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

fn apply_engine_info(app: &mut App, info: EngineInfo) {
    match info {
        EngineInfo::HttpApiReady { base_url } => {
            app.http_api_base = Some(base_url);
        }
    }
}

/// Handle the `s` keystroke in the Detail view's Files tab. Composes the
/// librqbit stream URL and hands it to the configured external player.
/// All failure modes degrade to a status-bar message instead of a crash.
fn handle_stream_keypress(app: &mut App) {
    let base = match app.http_api_base.clone() {
        Some(b) => b,
        None => {
            app.set_error("Streaming API not ready yet".to_string());
            return;
        }
    };

    // Pull the (torrent_id, file_idx, file_name) tuple inside a tight borrow
    // scope so the immutable borrow ends before we mutate `app` to set
    // status messages.
    let resolved = app.selected_torrent().and_then(|t| {
        t.files
            .get(app.detail_file_index)
            .map(|f| (t.id, app.detail_file_index, f.name.clone()))
    });

    let (torrent_id, file_idx, file_name) = match resolved {
        Some(v) => v,
        None => {
            app.set_error("No file selected (waiting for metadata)".to_string());
            return;
        }
    };

    let url = engine::torrent::stream_url(&base, torrent_id, file_idx);
    match player::spawn_player(&app.player_config, &url) {
        Ok(()) => app.set_info(format!("Streaming {}", file_name)),
        Err(e) => app.set_error(format!("Failed to open player: {}", e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn help_mode_esc_returns_to_normal() {
        let mut app = App::new();
        app.mode = AppMode::Help;
        handle_help_mode(&mut app, key(KeyCode::Esc));
        assert_eq!(app.mode, AppMode::Normal);
    }

    #[test]
    fn help_mode_ignores_other_keys() {
        let mut app = App::new();
        app.mode = AppMode::Help;
        handle_help_mode(&mut app, key(KeyCode::Char('x')));
        assert_eq!(app.mode, AppMode::Help);
    }

    #[test]
    fn quit_mode_y_sets_should_quit() {
        let mut app = App::new();
        app.mode = AppMode::ConfirmQuit;
        handle_quit_mode(&mut app, key(KeyCode::Char('y')));
        assert!(app.should_quit);
    }

    #[test]
    fn quit_mode_n_cancels_without_quitting() {
        let mut app = App::new();
        app.mode = AppMode::ConfirmQuit;
        handle_quit_mode(&mut app, key(KeyCode::Char('n')));
        assert!(!app.should_quit);
        assert_eq!(app.mode, AppMode::Normal);
    }

    #[test]
    fn filter_mode_typing_backspace_and_esc() {
        let mut app = App::new();
        app.mode = AppMode::Filter;
        handle_filter_mode(&mut app, key(KeyCode::Char('a')));
        handle_filter_mode(&mut app, key(KeyCode::Char('b')));
        assert_eq!(app.filter_text, "ab");
        handle_filter_mode(&mut app, key(KeyCode::Backspace));
        assert_eq!(app.filter_text, "a");
        handle_filter_mode(&mut app, key(KeyCode::Esc));
        assert_eq!(app.mode, AppMode::Normal);
        assert_eq!(app.filter_text, ""); // Esc clears the filter
    }

    #[tokio::test]
    async fn normal_mode_a_opens_input() {
        let (tx, _rx) = mpsc::channel::<EngineCommand>(8);
        let mut app = App::new();
        let mut iw = InputWidget::new();
        handle_normal_mode(&mut app, &mut iw, key(KeyCode::Char('a')), &tx).await;
        assert_eq!(app.mode, AppMode::Input);
    }

    #[tokio::test]
    async fn normal_mode_switch_keys_change_mode() {
        let (tx, _rx) = mpsc::channel::<EngineCommand>(8);
        let mut iw = InputWidget::new();

        let mut app = App::new();
        handle_normal_mode(&mut app, &mut iw, key(KeyCode::Char('?')), &tx).await;
        assert_eq!(app.mode, AppMode::Help);

        let mut app = App::new();
        handle_normal_mode(&mut app, &mut iw, key(KeyCode::Char('/')), &tx).await;
        assert_eq!(app.mode, AppMode::Filter);

        let mut app = App::new();
        handle_normal_mode(&mut app, &mut iw, key(KeyCode::Char('t')), &tx).await;
        assert_eq!(app.mode, AppMode::ThrottleInput);
    }

    #[test]
    fn stream_keypress_without_api_sets_error() {
        let mut app = App::new();
        assert!(app.http_api_base.is_none());
        handle_stream_keypress(&mut app);
        assert_eq!(
            app.error_message.as_deref(),
            Some("Streaming API not ready yet")
        );
    }
}
