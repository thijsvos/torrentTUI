//! Terminal BitTorrent client built on librqbit and ratatui.
//!
//! The process runs as two halves that never share memory. `run_app` owns the
//! terminal, the [`app::App`] state and every key press; `run_engine` runs in
//! its own tokio task and owns the librqbit `Session`. They talk over four mpsc
//! channels: commands down (`EngineCommand`, 32 slots), torrent snapshots up
//! (`Vec<TorrentInfo>`, 4 slots), status-bar strings up (16 slots),
//! and one-shot engine facts up (`EngineInfo`, 4 slots). Nothing in `ui` or
//! `app` may touch the session directly — that separation keeps rendering off
//! the engine's critical path and lets the UI notice an engine panic instead of
//! rendering frozen state forever.
//!
//! Two more channels feed the UI from tasks that are not the engine: the
//! disk-space probe (`Option<u64>`, 1 slot) and indexer-search outcomes
//! (`SearchOutcome`, 4 slots). Search HTTP deliberately lives in its own
//! spawned task rather than the engine loop, where a slow indexer would stall
//! the 100 ms state ticks.
//!
//! Because the channels are bounded, the UI must never fan out one send per
//! torrent: the batch variants (`PauseMany`, `DeleteMany`) exist so a bulk
//! action cannot fill the command queue while the engine is blocked pushing
//! state the UI has not drained yet.

mod actions;
mod app;
mod config;
mod engine;
mod opener;
mod player;
mod search;
mod types;
mod ui;

use std::io;
use std::path::Path;

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
use ui::input::{validate_magnet, validate_torrent_source, InputWidget};

/// Speed-limit cap in KB/s: ⌊10⁹ / 1024⌋, ≈953 MiB/s. The binding constraint
/// is governor's pacing ceiling, not the `NonZeroU32` quota: librqbit's
/// limiter replenishes one byte-permit per whole nanosecond at most, so
/// ~1 GB/s is the fastest rate it can pace — a higher cap would display
/// limits the limiter cannot enforce. (Near the cap, enforcement quantizes
/// to 10⁹/k bytes/sec for integer k — coarser than configured, never a
/// stall.) Applied by [`clamped_speed_limit`] to every entry point: the
/// throttle dialog, the values read from `config.toml`, and the limits the
/// engine enforces.
const MAX_SPEED_LIMIT_KBPS: u64 = 976_562;

/// Speed-limit floor in KB/s for nonzero limits. librqbit acquires limiter
/// permits in whole 16 KiB chunks, and a per-second quota smaller than one
/// chunk makes governor report the acquire as impossible — killing the peer
/// task instead of pacing it.
const MIN_SPEED_LIMIT_KBPS: u64 = 16;

/// Clamp a user-supplied speed limit (`0` stays 0 = unlimited). The single
/// choke point for the floor and the cap — the status bar multiplies by 1024
/// without saturating, and the engine converts to the limiter's `NonZeroU32`
/// bytes/sec quota, so a value that skips this can overflow the display or
/// produce a quota the limiter cannot serve.
pub(crate) fn clamped_speed_limit(kbps: u64) -> u64 {
    if kbps == 0 {
        0
    } else {
        kbps.clamp(MIN_SPEED_LIMIT_KBPS, MAX_SPEED_LIMIT_KBPS)
    }
}

/// Size at which the log is rotated on startup. The default filter is
/// `torrenttui=warn`, which writes almost nothing, so this only bites people
/// running with `RUST_LOG` turned up.
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

/// Maximum digits accepted in the throttle input dialog. Combined with the
/// numeric cap above, anything past this gets truncated visually.
const MAX_THROTTLE_INPUT_DIGITS: usize = 8;

#[derive(Parser)]
#[command(name = "torrenttui", version, about = "Terminal BitTorrent client")]
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

    // Parse before touching the filesystem. clap exits from inside `parse()`
    // for --version and --help, so doing this first keeps those from creating
    // a config directory and writing a startup marker to the log of a machine
    // that may never have run the app (#44).
    let cli = Cli::parse();

    // Set up logging to file. Default filter is "torrenttui=warn" so librqbit
    // internals (peer IPs, tracker URLs, info hashes) don't get persisted to
    // disk by default. Users who want verbose logs can set RUST_LOG.
    let log_dir = config::Config::config_dir();
    std::fs::create_dir_all(&log_dir)?;
    let log_file = open_log_file(&log_dir)?;
    // The log is cumulative now, so mark where each run begins. Written
    // straight to the file rather than through tracing: the default filter is
    // `torrenttui=warn` and a startup notice is not a warning, so it would be
    // filtered out for exactly the users who need it. Timestamps come from the
    // tracing lines that follow.
    {
        use std::io::Write;
        let _ = writeln!(
            &log_file,
            "=== torrenttui {} ===",
            env!("CARGO_PKG_VERSION")
        );
    }
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("torrenttui=warn"));
    tracing_subscriber::fmt()
        .with_writer(log_file)
        .with_ansi(false)
        .with_env_filter(filter)
        .init();

    // Load config. A parse error returns `(default, Some(warning))` so we can
    // surface it; an Err is an I/O failure either reading an existing config or
    // — on first run, when `load` writes the defaults out — creating the config
    // directory, serializing, fsyncing or renaming the new file.
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
    // A quoted `--download-dir '~/dl'` dodges shell tilde expansion, so
    // expand here like the config loader does.
    if let Some(ref dir) = cli.download_dir {
        config.general.download_dir = config::expand_tilde(dir);
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

/// Open `torrenttui.log` for appending, rotating it first if it has grown past
/// [`MAX_LOG_BYTES`].
///
/// Appending rather than truncating matters because the failures people most
/// want logs for — a crash, a hang, a torrent misbehaving — are usually
/// followed immediately by restarting the app, and truncating on launch
/// destroyed exactly the evidence they came back for (#42). One previous
/// generation is kept as `torrenttui.log.1`.
fn open_log_file(log_dir: &Path) -> io::Result<std::fs::File> {
    let path = log_dir.join("torrenttui.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_LOG_BYTES) {
        // A failed rotation is not worth refusing to start over; the append
        // below just carries on in the oversized file.
        let _ = std::fs::rename(&path, log_dir.join("torrenttui.log.1"));
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
}

/// The UI loop. Spawns the engine task, then alternates between draining the
/// engine's channels, rendering, and awaiting the next event.
///
/// Rendering is change-driven: `needs_render` is set by anything that alters
/// what is on screen, and the frame interval exists only to age out timed
/// messages and advance the metadata spinner. An idle session with no torrents
/// therefore draws nothing at all.
///
/// Each pass drains the state/message/info channels with `try_recv` *before*
/// the `select!`, so a burst of engine pushes collapses into a single repaint
/// instead of one per message. The engine's `JoinHandle` is retained rather
/// than detached purely so a panicked engine is noticed here instead of leaving
/// the UI rendering frozen state forever.
///
/// On quit it sends `Shutdown` and waits up to 5 s for the engine to flush
/// librqbit's persisted state and finish any watch-folder cleanup.
async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    cli: Cli,
    config: config::Config,
    config_warning: Option<String>,
) -> Result<()> {
    let mut app = App::new();
    let mut input_widget = InputWidget::new();

    // Clamp here, not just in the throttle dialog: these reach an unchecked
    // `* 1024` in the status bar, which overflows on an absurd config value
    // (#49).
    app.speed_limit_download_kbps = clamped_speed_limit(config.network.max_download_speed_kbps);
    app.speed_limit_upload_kbps = clamped_speed_limit(config.network.max_upload_speed_kbps);
    app.confirm_on_quit = config.general.confirm_on_quit;
    app.player_config = config.player.clone();
    app.watch_dir_configured = config.general.watch_dir.is_some();
    app.search_config = config.search.clone();
    app.download_dir = config.general.download_dir.clone();

    if let Some(msg) = config_warning {
        app.set_error(msg);
    }

    let (cmd_tx, cmd_rx) = mpsc::channel::<EngineCommand>(32);
    let (state_tx, mut state_rx) = mpsc::channel::<Vec<types::TorrentInfo>>(4);
    let (msg_tx, mut msg_rx) = mpsc::channel::<String>(16);
    // Engine-to-UI one-shot facts (HTTP API base URL today; future use:
    // listening port, DHT state). Small capacity because messages are rare.
    let (info_tx, mut info_rx) = mpsc::channel::<EngineInfo>(4);
    // Free-space probe results. Depth 1: only the newest reading matters, and
    // a full channel just means a probe is still in flight.
    let (disk_tx, mut disk_rx) = mpsc::channel::<Option<u64>>(1);
    // Indexer-search outcomes from UI-spawned tasks. Generation filtering in
    // App drops anything from a superseded query, so depth just needs to hold
    // a small backlog of stale sends.
    let (search_tx, mut search_rx) = mpsc::channel::<search::SearchOutcome>(4);
    // Built lazily on the first search so an unused feature costs nothing at
    // startup; a failed build surfaces as a status-bar error and the next
    // search attempt retries it.
    let mut search_client: Option<reqwest::Client> = None;

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
        // Same tilde story as --download-dir; magnet links never start with
        // `~` so expansion is a no-op for them.
        let source = config::expand_tilde(source);
        match validate_torrent_source(&source) {
            Ok(()) => {
                send_cmd(&cmd_tx, EngineCommand::AddTorrent(source), &mut app).await;
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
    let mut engine_died = false;

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
                // Without this the loop can quit without ever drawing the
                // message, so the TUI just vanishes. The Err below is what
                // puts it on stderr once the terminal is restored.
                needs_render = true;
                engine_died = true;
            }
        }

        while let Ok(torrents) = state_rx.try_recv() {
            // Only repaint when something on screen actually moved.
            needs_render |= app.handle_state_push(torrents);
        }
        while let Ok(msg) = msg_rx.try_recv() {
            app.set_info(msg);
            needs_render = true;
        }
        while let Ok(info) = info_rx.try_recv() {
            apply_engine_info(&mut app, info);
            needs_render = true;
        }
        while let Ok(free) = disk_rx.try_recv() {
            if app.free_disk_space != free {
                app.set_disk_space(free);
                needs_render = true;
            }
        }
        while let Ok(outcome) = search_rx.try_recv() {
            needs_render |= app.apply_search_outcome(outcome);
        }

        app.clear_expired_messages();

        if needs_render {
            needs_render = false;
            terminal.draw(|f| {
                let chunks = ui::layout::get_layout(f.area());

                ui::layout::render_header(f, chunks[0]);

                // While the palette overlay is open, the main area keeps
                // showing the view it opened over.
                let view_mode = if app.mode == AppMode::Palette {
                    app.palette.return_mode.clone()
                } else {
                    app.mode.clone()
                };
                match view_mode {
                    AppMode::Detail => {
                        ui::detail::render_detail(f, chunks[1], &mut app);
                        app.table_area = None;
                    }
                    // The search views cover the torrent table, so clicks must
                    // not map to its hidden rows — same hygiene as Detail. In
                    // Search (typing) mode the previous results stay visible
                    // for the refine loop; before any search exists the
                    // torrent table shows through instead.
                    AppMode::SearchResults => {
                        ui::search::render_search_view(f, chunks[1], &mut app);
                        app.table_area = None;
                    }
                    AppMode::Search if app.search.searched_once || app.search.in_flight => {
                        ui::search::render_search_view(f, chunks[1], &mut app);
                        app.table_area = None;
                    }
                    _ => {
                        app.table_area = Some(chunks[1]);
                        ui::table::render_table(f, chunks[1], &mut app);
                    }
                }
                if app.mode == AppMode::Palette {
                    // Click hit-testing is Normal-mode-gated anyway, but keep
                    // the Detail precedent: no clicks onto a covered table.
                    app.table_area = None;
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
                    // An active error outranks the input bar: the errors fired
                    // while staying in Search mode (providers disabled, client
                    // build failure) mean the search cannot proceed, and the
                    // status bar is the only widget that renders them.
                    AppMode::Search if app.error_message.is_none() => {
                        ui::layout::render_search_bar(f, chunks[2], &app.search.input);
                    }
                    _ => {
                        ui::layout::render_status_bar(f, chunks[2], &app);
                    }
                }

                if app.mode == AppMode::Help {
                    ui::help::render_help(f, f.area(), &mut app);
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
                        ui::dialogs::render_delete_dialog(
                            f,
                            f.area(),
                            &label,
                            app.watch_dir_configured,
                        );
                    }
                }
                if app.mode == AppMode::ConfirmQuit {
                    ui::dialogs::render_quit_dialog(f, f.area());
                }
                if app.mode == AppMode::Palette {
                    ui::palette::render_palette(f, f.area(), &mut app);
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
            return if engine_died {
                Err(anyhow::anyhow!("engine task ended unexpectedly"))
            } else {
                Ok(())
            };
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
                            // in_detail_view, not mode == Detail: the detail
                            // view also sits under a palette overlay, and
                            // skipping the clear there leaked per-tick
                            // file/peer materialization in the engine.
                            let was_detail = app.in_detail_view();
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
                        // Ctrl+P: open the command palette from any non-input
                        // mode. Consumed even where it can't open, so it never
                        // types a literal 'p' into a text field.
                        if key.code == KeyCode::Char('p')
                            && key.modifiers.contains(KeyModifiers::CONTROL)
                        {
                            app.open_palette();
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
                            AppMode::Search => handle_search_input_mode(&mut app, key, &search_tx, &mut search_client),
                            AppMode::SearchResults => handle_search_results_mode(&mut app, key, &cmd_tx, &search_tx, &mut search_client).await,
                            AppMode::Palette => handle_palette_mode(&mut app, key, &mut input_widget, &cmd_tx, &search_tx, &mut search_client).await,
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
                    Some(Ok(Event::Paste(s))) if app.mode == AppMode::Search => {
                        // Same filter+cap as typing — search_push_char owns both.
                        for c in s.chars() {
                            app.search_push_char(c);
                        }
                        needs_render = true;
                    }
                    Some(Ok(Event::Paste(s))) if app.mode == AppMode::Palette => {
                        for c in s.chars() {
                            app.palette_push_char(c);
                        }
                        needs_render = true;
                    }
                    Some(Ok(Event::Resize(_, _))) => {
                        // Without this a resize leaves a stale frame until the
                        // next key press or animation tick — most visible on
                        // the static search views, but a gap for every mode.
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
                needs_render |= app.handle_state_push(torrents);
            }
            Some(msg) = msg_rx.recv() => {
                app.set_info(msg);
                needs_render = true;
            }
            Some(info) = info_rx.recv() => {
                apply_engine_info(&mut app, info);
                needs_render = true;
            }
            Some(outcome) = search_rx.recv() => {
                needs_render |= app.apply_search_outcome(outcome);
            }
            _ = frame_interval.tick() => {
                // Only burn a render frame when something actually animates.
                // Disk-space refresh is internally throttled to ~5s.
                let prev_disk = app.free_disk_space;
                // Off the reactor: `available_space` is a blocking statvfs, and
                // on a hung NFS/SMB mount it blocks for the mount timeout —
                // which would freeze the whole TUI, not just this readout.
                if app.disk_space_due() {
                    let dir = download_dir.clone();
                    let tx = disk_tx.clone();
                    tokio::task::spawn_blocking(move || {
                        let _ = tx.try_send(fs4::available_space(&dir).ok());
                    });
                }
                let disk_changed = prev_disk != app.free_disk_space;

                if app.has_fetching_metadata() || app.search_spinner_active() {
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

// ---------------------------------------------------------------------------
// Actions. Each user-invokable operation is a named function; the per-mode
// key handlers and the command palette's `execute_action` both dispatch to
// these, so the palette is an alternate front door onto the same code, never
// a second implementation.
// ---------------------------------------------------------------------------

/// The `q` action: confirm when work is active, quit outright otherwise.
fn request_quit(app: &mut App) {
    if app.confirm_on_quit_required() {
        app.mode = AppMode::ConfirmQuit;
    } else {
        app.should_quit = true;
    }
}

fn open_add_input(app: &mut App, input_widget: &mut InputWidget) {
    app.mode = AppMode::Input;
    input_widget.clear();
}

/// The search buffer is deliberately not cleared: coming back to search
/// shows the previous query ready to refine.
fn open_search_input(app: &mut App) {
    app.mode = AppMode::Search;
}

/// Pause/unpause the marked set, or the selected torrent.
async fn pause_toggle(app: &mut App, cmd_tx: &mpsc::Sender<EngineCommand>) {
    if app.has_marks() {
        let ids: Vec<usize> = app.marked_ids.iter().copied().collect();
        // "Any paused → resume all" is the intuitive model. Since speed
        // limits moved into librqbit's rate limiter, Paused always means
        // the user asked for it — the old throttle-pause ambiguity (#47)
        // no longer exists.
        let any_paused = ids.iter().any(|id| {
            app.torrents
                .iter()
                .any(|t| t.id == *id && matches!(t.status, types::TorrentStatus::Paused))
        });
        // Send the whole batch as one message — the 32-slot channel
        // would block on the 33rd send otherwise, and the engine's own
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
        match torrent.status {
            // Seeding and Complete are pausable too — librqbit
            // handles a finished torrent fine, and the marked and
            // `P` paths already pause them, so leaving them out
            // here made `p` a dropped keypress on a seeding row.
            types::TorrentStatus::Downloading
            | types::TorrentStatus::Complete
            | types::TorrentStatus::Seeding => {
                send_cmd(cmd_tx, EngineCommand::Pause(id), app).await;
            }
            types::TorrentStatus::Paused => {
                send_cmd(cmd_tx, EngineCommand::Resume(id), app).await;
            }
            types::TorrentStatus::FetchingMetadata => {
                app.set_info("Can't pause while fetching metadata".to_string());
            }
            _ => {}
        }
    }
}

/// Pause or resume everything.
async fn pause_all(app: &mut App, cmd_tx: &mpsc::Sender<EngineCommand>) {
    // "Everything paused → resume all", like the `p` handler. `.all()` on
    // an empty iterator is true, so an idle session needs the emptiness
    // guard or `P` resumes nothing at all.
    let pausable: Vec<&types::TorrentInfo> = app
        .torrents
        .iter()
        .filter(|t| {
            matches!(
                t.status,
                types::TorrentStatus::Downloading
                    | types::TorrentStatus::Paused
                    | types::TorrentStatus::Complete
                    | types::TorrentStatus::Seeding
            )
        })
        .collect();
    let all_paused = !pausable.is_empty()
        && pausable
            .iter()
            .all(|t| matches!(t.status, types::TorrentStatus::Paused));

    let cmd = if all_paused {
        EngineCommand::ResumeAll
    } else {
        EngineCommand::PauseAll
    };
    send_cmd(cmd_tx, cmd, app).await;
}

/// Guard on the *visible* list, like `open_detail` does: with a filter that
/// matches nothing the dialog label is empty and the popup is never drawn,
/// leaving the user in a modal with nothing on screen.
fn open_delete_confirm(app: &mut App) {
    if !app.sorted_torrents().is_empty() || app.has_marks() {
        app.mode = AppMode::ConfirmDelete;
    }
}

async fn open_detail(app: &mut App, cmd_tx: &mpsc::Sender<EngineCommand>) {
    if app.sorted_torrents().is_empty() {
        return;
    }
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

fn open_throttle(app: &mut App) {
    app.mode = AppMode::ThrottleInput;
    app.throttle_step = 0;
    app.throttle_input_buf = if app.speed_limit_download_kbps > 0 {
        app.speed_limit_download_kbps.to_string()
    } else {
        String::new()
    };
}

fn toggle_mark_and_advance(app: &mut App) {
    app.toggle_mark();
    app.next();
}

fn open_help(app: &mut App) {
    app.help_scroll = 0;
    app.mode = AppMode::Help;
}

async fn handle_normal_mode(
    app: &mut App,
    input_widget: &mut InputWidget,
    key: crossterm::event::KeyEvent,
    cmd_tx: &mpsc::Sender<EngineCommand>,
) {
    match key.code {
        KeyCode::Char('q') => request_quit(app),
        KeyCode::Char('j') | KeyCode::Down => app.next(),
        KeyCode::Char('k') | KeyCode::Up => app.previous(),
        KeyCode::Char('a') => open_add_input(app, input_widget),
        KeyCode::Char('s') => open_search_input(app),
        KeyCode::Char('o') => handle_open_folder(app),
        KeyCode::Char(':') => {
            app.open_palette();
        }
        KeyCode::Char('p') => pause_toggle(app, cmd_tx).await,
        KeyCode::Char('P') => pause_all(app, cmd_tx).await,
        KeyCode::Char('d') => open_delete_confirm(app),
        KeyCode::Enter => open_detail(app, cmd_tx).await,
        KeyCode::Char('?') => open_help(app),
        KeyCode::Tab => {
            let next = app.sort_column.next();
            app.change_sort_column(next);
        }
        KeyCode::Char('r') => app.toggle_sort_reversed(),
        KeyCode::Char('/') => app.mode = AppMode::Filter,
        KeyCode::Char('t') => open_throttle(app),
        KeyCode::Char(' ') => toggle_mark_and_advance(app),
        KeyCode::Char('v') => app.mark_all(),
        KeyCode::Char('V') => app.clear_marks(),
        KeyCode::Esc => app.clear_marks(),
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
            // No shell is involved in TUI input, so `~/x.torrent` only works
            // if we expand it ourselves.
            let value = config::expand_tilde(input_widget.value().trim());
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

/// Build the shared HTTP client for indexer searches. `https_only` because
/// both providers are HTTPS and a downgrade would leak queries in plaintext;
/// an explicit User-Agent because an empty one is a common WAF block.
fn build_search_client(config: &config::SearchConfig) -> reqwest::Result<reqwest::Client> {
    let timeout = std::time::Duration::from_secs(config.timeout_secs.clamp(1, 30));
    reqwest::Client::builder()
        .https_only(true)
        .timeout(timeout)
        .connect_timeout(timeout.min(std::time::Duration::from_secs(5)))
        .user_agent(concat!("torrenttui/", env!("CARGO_PKG_VERSION")))
        .build()
}

/// Fire (or with `retry` re-fire) a search from the current App state and
/// switch to the results view. All the state transitions live in the App
/// methods; this owns only the pieces that touch the runtime — the lazy
/// client and the task spawn.
fn start_search(
    app: &mut App,
    search_tx: &mpsc::Sender<search::SearchOutcome>,
    search_client: &mut Option<reqwest::Client>,
    retry: bool,
) {
    if !app.search_config.enable_apibay && !app.search_config.enable_torrents_csv {
        app.set_error("All search providers are disabled in config".to_string());
        return;
    }
    if search_client.is_none() {
        match build_search_client(&app.search_config) {
            Ok(client) => *search_client = Some(client),
            Err(e) => {
                tracing::debug!("search client build failed: {e}");
                app.set_error("Search unavailable: could not initialize HTTP client".to_string());
                return;
            }
        }
    }
    let Some(client) = search_client.clone() else {
        return;
    };
    let fired = if retry {
        app.refire_search()
    } else {
        app.fire_search()
    };
    let Some((query, generation)) = fired else {
        return; // empty query: silent no-op, stay where we are
    };
    search::spawn_search(
        query,
        generation,
        client,
        app.search_config.clone(),
        search_tx.clone(),
    );
    app.mode = AppMode::SearchResults;
}

/// Keys while typing a search query. Sync — spawning the search task doesn't
/// await anything.
fn handle_search_input_mode(
    app: &mut App,
    key: crossterm::event::KeyEvent,
    search_tx: &mpsc::Sender<search::SearchOutcome>,
    search_client: &mut Option<reqwest::Client>,
) {
    match key.code {
        KeyCode::Esc => {
            // One level shallower: back to the results if any exist (or are on
            // the way), otherwise out to Normal. Never cancels a flight.
            app.mode = if app.search.searched_once || app.search.in_flight {
                AppMode::SearchResults
            } else {
                AppMode::Normal
            };
        }
        KeyCode::Enter => {
            start_search(app, search_tx, search_client, false);
        }
        KeyCode::Backspace => {
            app.search_pop_char();
        }
        KeyCode::Char(c) => {
            app.search_push_char(c);
        }
        _ => {}
    }
}

/// Keys in the results view. Enter adds the highlighted result through the
/// same `AddTorrent` path as the manual flow — the magnet is built locally
/// from the validated info hash, so nothing is copied or shown to the user.
async fn handle_search_results_mode(
    app: &mut App,
    key: crossterm::event::KeyEvent,
    cmd_tx: &mpsc::Sender<EngineCommand>,
    search_tx: &mpsc::Sender<search::SearchOutcome>,
    search_client: &mut Option<reqwest::Client>,
) {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            // Results (and any in-flight search) persist; `s` from Normal
            // comes back to them. A landing outcome just repaints nothing.
            app.mode = AppMode::Normal;
        }
        KeyCode::Char('s') => {
            app.mode = AppMode::Search; // buffer still holds the query
        }
        KeyCode::Char(':') => {
            app.open_palette();
        }
        KeyCode::Char('j') | KeyCode::Down => app.search_next(),
        KeyCode::Char('k') | KeyCode::Up => app.search_previous(),
        KeyCode::Tab => app.cycle_result_sort(),
        // `r` is taken by retry in this view, so reverse lives on `R`.
        KeyCode::Char('R') => app.reverse_result_sort(),
        KeyCode::Char('r') => retry_search(app, search_tx, search_client),
        KeyCode::Enter => download_selected_result(app, cmd_tx).await,
        _ => {}
    }
}

fn retry_search(
    app: &mut App,
    search_tx: &mpsc::Sender<search::SearchOutcome>,
    search_client: &mut Option<reqwest::Client>,
) {
    if !app.search.in_flight {
        start_search(app, search_tx, search_client, true);
    }
}

/// Add the highlighted search result through the same `AddTorrent` path as
/// the manual flow.
async fn download_selected_result(app: &mut App, cmd_tx: &mpsc::Sender<EngineCommand>) {
    if app.search.in_flight {
        return;
    }
    // Borrow-scope the read like handle_stream_keypress does.
    let picked = app
        .selected_search_result()
        .map(|r| (r.info_hash.clone(), r.title.clone()));
    let Some((info_hash, title)) = picked else {
        return;
    };
    // Block a re-send only while the torrent is actually in the
    // session: after a delete (or a failed add) the hash is still in
    // `added` for the ✓ mark, but Enter must work again. In the brief
    // window before the first state push the presence check misses and
    // a double-press reaches the engine, whose AlreadyManaged reply
    // answers "already downloaded" — that backstop is the design.
    if app.search.added.contains(&info_hash) && app.torrent_in_session(&info_hash) {
        app.set_info(format!("Already added: {}", title));
        return;
    }
    let magnet = search::build_magnet(&info_hash, &title);
    // Belt-and-braces: the same gate the manual flow uses. Parse-time
    // hash validation makes a failure here impossible unless an
    // invariant broke, and then an error beats a corrupt command.
    if let Err(e) = validate_magnet(&magnet) {
        app.set_error(e);
        return;
    }
    send_cmd(cmd_tx, EngineCommand::AddTorrent(magnet), app).await;
    app.search.added.insert(info_hash);
    // Stay in the results for multi-grab; the engine reports
    // duplicates/failures on its own message channel.
    app.set_info(format!("Added: {}", title));
}

async fn handle_detail_mode(
    app: &mut App,
    key: crossterm::event::KeyEvent,
    cmd_tx: &mpsc::Sender<EngineCommand>,
) {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => detail_back(app, cmd_tx).await,
        KeyCode::Char(':') => {
            app.open_palette();
        }
        KeyCode::Tab => cycle_detail_tab(app),
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
            toggle_file_selection_key(app, cmd_tx).await;
        }
        KeyCode::Char('s') if app.detail_tab == DetailTab::Files => {
            handle_stream_keypress(app);
        }
        KeyCode::Char('o') => {
            handle_open_folder(app);
        }
        KeyCode::Char('S') if app.detail_tab == DetailTab::Files => {
            apply_file_selection(app, cmd_tx).await;
        }
        _ => {}
    }
}

/// Leave Detail — and stop the engine from materializing files/peers for the
/// (now-hidden) torrent.
async fn detail_back(app: &mut App, cmd_tx: &mpsc::Sender<EngineCommand>) {
    app.mode = AppMode::Normal;
    send_cmd(cmd_tx, EngineCommand::SetDetailTorrent(None), app).await;
}

fn cycle_detail_tab(app: &mut App) {
    app.detail_tab = app.detail_tab.next();
    app.detail_file_index = 0;
    app.detail_peer_index = 0;
    app.detail_peer_scroll_offset = 0;
}

async fn toggle_file_selection_key(app: &mut App, cmd_tx: &mpsc::Sender<EngineCommand>) {
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

async fn apply_file_selection(app: &mut App, cmd_tx: &mpsc::Sender<EngineCommand>) {
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

/// Route a palette-chosen action to the same function its keybinding calls.
/// The palette has already closed (mode restored) by the time this runs, so
/// mode transitions inside the actions behave exactly as if the key was
/// pressed in the origin view.
async fn execute_action(
    id: actions::ActionId,
    app: &mut App,
    input_widget: &mut InputWidget,
    cmd_tx: &mpsc::Sender<EngineCommand>,
    search_tx: &mpsc::Sender<search::SearchOutcome>,
    search_client: &mut Option<reqwest::Client>,
) {
    use actions::ActionId as A;
    match id {
        A::AddTorrent => open_add_input(app, input_widget),
        A::OpenSearch => open_search_input(app),
        A::PauseToggle => pause_toggle(app, cmd_tx).await,
        A::PauseAll => pause_all(app, cmd_tx).await,
        A::Delete => open_delete_confirm(app),
        A::RevealFolder => handle_open_folder(app),
        A::OpenDetail => open_detail(app, cmd_tx).await,
        A::CycleSort => {
            let next = app.sort_column.next();
            app.change_sort_column(next);
        }
        A::ReverseSort => app.toggle_sort_reversed(),
        A::OpenFilter => app.mode = AppMode::Filter,
        A::OpenThrottle => open_throttle(app),
        A::ToggleMark => toggle_mark_and_advance(app),
        A::MarkAll => app.mark_all(),
        A::ClearMarks => app.clear_marks(),
        A::ToggleHelp => open_help(app),
        A::Quit => request_quit(app),
        A::CycleTab => cycle_detail_tab(app),
        A::ToggleFileSelection => toggle_file_selection_key(app, cmd_tx).await,
        A::ApplyFileSelection => apply_file_selection(app, cmd_tx).await,
        A::StreamFile => handle_stream_keypress(app),
        A::DetailBack => detail_back(app, cmd_tx).await,
        A::DownloadResult => download_selected_result(app, cmd_tx).await,
        A::EditQuery => app.mode = AppMode::Search,
        A::RetrySearch => retry_search(app, search_tx, search_client),
        A::CycleResultSort => app.cycle_result_sort(),
        A::ReverseResultSort => app.reverse_result_sort(),
        A::SearchBack => app.mode = AppMode::Normal,
    }
}

/// Keys while the command palette is open. Plain letters filter (so `j`
/// stays typeable in queries); navigation is arrows or Ctrl+j/k.
async fn handle_palette_mode(
    app: &mut App,
    key: crossterm::event::KeyEvent,
    input_widget: &mut InputWidget,
    cmd_tx: &mpsc::Sender<EngineCommand>,
    search_tx: &mpsc::Sender<search::SearchOutcome>,
    search_client: &mut Option<reqwest::Client>,
) {
    match key.code {
        KeyCode::Esc => app.close_palette(),
        KeyCode::Enter => {
            let matches = app.palette_matches();
            let Some(&target) = matches.get(app.palette_selected_index(&matches)) else {
                return;
            };
            if let Some(anchor) = app.palette.anchor {
                if !std::ptr::eq(anchor, target) {
                    // The deliberately-navigated-to action vanished (its
                    // availability changed under the palette). Re-anchor to
                    // the top instead of firing whatever shifted into its
                    // place — one extra Enter beats an unwanted action.
                    app.palette.anchor = None;
                    return;
                }
            }
            if let Some(id) = target.id {
                app.close_palette();
                execute_action(id, app, input_widget, cmd_tx, search_tx, search_client).await;
            }
        }
        KeyCode::Down => app.palette_next(),
        KeyCode::Up => app.palette_previous(),
        KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => app.palette_next(),
        KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.palette_previous()
        }
        KeyCode::Backspace => app.palette_pop_char(),
        KeyCode::Char(c) => app.palette_push_char(c),
        _ => {}
    }
}

fn handle_help_mode(app: &mut App, key: crossterm::event::KeyEvent) {
    match key.code {
        KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => {
            app.mode = AppMode::Normal;
        }
        // The registry-driven table outgrows short terminals; the renderer
        // clamps the offset against the real maximum, so saturating math is
        // all the handler needs.
        KeyCode::Char('j') | KeyCode::Down => {
            app.help_scroll = app.help_scroll.saturating_add(1);
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.help_scroll = app.help_scroll.saturating_sub(1);
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
            // Clamp exactly like the engine will, so the dialog, status bar
            // and enforced limit all agree (including the 16 KB/s floor).
            let value = clamped_speed_limit(app.throttle_input_buf.parse::<u64>().unwrap_or(0));
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

/// Keys for the delete confirmation: `k` keeps the downloaded files, `d`
/// deletes them, `c` or Esc cancels. Both `k` and `d` remove the torrent from
/// the session — the choice is only about the data on disk. Operates on the
/// marked set when there is one, otherwise on the selected row, and marks are
/// cleared afterwards either way, including marks hidden by the active filter.
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

/// Handle the `o` keystroke (Normal and Detail): reveal the selected
/// torrent's data in the system file manager, degrading to opening the
/// download directory when the data isn't on disk yet. No-op with nothing
/// selected, matching the other selection-scoped keys.
fn handle_open_folder(app: &mut App) {
    let Some((name, content_path)) = app
        .selected_torrent()
        .map(|t| (t.name.clone(), t.content_path.clone()))
    else {
        return;
    };
    let target = opener::resolve_reveal_target(
        Path::new(&app.download_dir),
        content_path.as_deref().map(Path::new),
    );
    match opener::reveal(&target) {
        Ok(()) => match target {
            opener::RevealTarget::Item(_) => app.set_info(format!("Opening folder for {}", name)),
            // Neutral wording: the fallback covers both "metadata still
            // resolving" and "data moved/deleted outside the app", and
            // guessing the cause here misled in exactly the second case.
            opener::RevealTarget::Dir(_) => app.set_info("Opening the download folder".to_string()),
        },
        Err(e) => app.set_error(format!("Failed to open file manager: {}", e)),
    }
}

/// Handle the `s` keystroke in Detail mode's Files tab. Composes the
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
    use std::io::Write;

    fn key(code: KeyCode) -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn read_log(dir: &Path) -> String {
        std::fs::read_to_string(dir.join("torrenttui.log")).unwrap()
    }

    #[test]
    fn clamped_speed_limit_bounds_every_entry_point() {
        // Calls the production function rather than re-implementing the
        // clamp, so deleting either bound fails here.
        assert_eq!(clamped_speed_limit(u64::MAX), MAX_SPEED_LIMIT_KBPS);
        assert_eq!(clamped_speed_limit(0), 0);
        assert_eq!(clamped_speed_limit(1024), 1024);
        // Nonzero values below one 16 KiB chunk/sec clamp *up*: librqbit's
        // limiter cannot serve a quota smaller than one chunk.
        assert_eq!(clamped_speed_limit(1), MIN_SPEED_LIMIT_KBPS);
        assert_eq!(clamped_speed_limit(15), MIN_SPEED_LIMIT_KBPS);
        assert_eq!(clamped_speed_limit(16), 16);
        // The conversions downstream of the cap: the status bar's `* 1024`
        // and the engine's NonZeroU32 bytes/sec quota for the rate limiter.
        let capped = clamped_speed_limit(u64::MAX);
        assert!(capped.checked_mul(1024).is_some());
        assert!(u32::try_from(capped.saturating_mul(1024)).is_ok());
        // The cap must stay at or below governor's pacing ceiling: the
        // limiter replenishes one byte-permit per whole nanosecond at most,
        // so a quota above 1e9 bytes/sec is displayed but not enforced.
        assert!(capped.saturating_mul(1024) <= 1_000_000_000);
    }

    #[tokio::test]
    async fn throttle_dialog_sends_clamped_limits() {
        // The dialog clamps through `clamped_speed_limit`, so the engine, the
        // status bar and the confirmation message can never disagree — the
        // floor raises a 1 KB/s entry to 16, and an over-cap entry (the buf
        // takes up to 8 digits, well past the ~4.2M cap) comes back capped.
        let (tx, mut rx) = mpsc::channel::<EngineCommand>(8);
        let mut app = App::new();
        open_throttle(&mut app);
        handle_throttle_mode(&mut app, key(KeyCode::Char('1')), &tx).await;
        handle_throttle_mode(&mut app, key(KeyCode::Enter), &tx).await;
        for c in "99999999".chars() {
            handle_throttle_mode(&mut app, key(KeyCode::Char(c)), &tx).await;
        }
        handle_throttle_mode(&mut app, key(KeyCode::Enter), &tx).await;
        match rx.try_recv().expect("a command should have been sent") {
            EngineCommand::SetSpeedLimits {
                download_kbps,
                upload_kbps,
            } => {
                assert_eq!(download_kbps, MIN_SPEED_LIMIT_KBPS);
                assert_eq!(upload_kbps, MAX_SPEED_LIMIT_KBPS);
            }
            other => panic!("expected SetSpeedLimits, got {other:?}"),
        }
        assert_eq!(app.mode, AppMode::Normal);
    }

    #[tokio::test]
    async fn pause_all_resumes_when_everything_is_paused() {
        let (tx, mut rx) = mpsc::channel::<EngineCommand>(8);
        let mut app = App::new();
        let mut iw = InputWidget::new();
        app.handle_state_push(vec![
            torrent(0, types::TorrentStatus::Paused),
            torrent(1, types::TorrentStatus::Paused),
        ]);
        handle_normal_mode(&mut app, &mut iw, key(KeyCode::Char('P')), &tx).await;
        assert!(matches!(
            rx.try_recv().expect("a command"),
            EngineCommand::ResumeAll
        ));
    }

    #[tokio::test]
    async fn pause_all_on_an_idle_session_does_not_resume() {
        // `.all()` on an empty iterator is true, which used to make `P` send
        // ResumeAll with nothing to resume.
        let (tx, mut rx) = mpsc::channel::<EngineCommand>(8);
        let mut app = App::new();
        let mut iw = InputWidget::new();
        handle_normal_mode(&mut app, &mut iw, key(KeyCode::Char('P')), &tx).await;
        assert!(matches!(
            rx.try_recv().expect("a command"),
            EngineCommand::PauseAll
        ));
    }

    #[tokio::test]
    async fn p_pauses_a_seeding_torrent() {
        // Was a silent no-op: the match fell through to `_ => {}`, so a single
        // seeding torrent could not be stopped even though mark+p and P both
        // paused it.
        for status in [
            types::TorrentStatus::Seeding,
            types::TorrentStatus::Complete,
        ] {
            let (tx, mut rx) = mpsc::channel::<EngineCommand>(8);
            let mut app = App::new();
            let mut iw = InputWidget::new();
            app.handle_state_push(vec![torrent(0, status.clone())]);
            handle_normal_mode(&mut app, &mut iw, key(KeyCode::Char('p')), &tx).await;
            assert!(
                matches!(rx.try_recv().expect("a command"), EngineCommand::Pause(0)),
                "{status:?} should pause"
            );
        }
    }

    #[test]
    fn log_is_appended_not_truncated() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut f = open_log_file(dir.path()).unwrap();
            writeln!(f, "first run").unwrap();
        }
        {
            let mut f = open_log_file(dir.path()).unwrap();
            writeln!(f, "second run").unwrap();
        }
        let contents = read_log(dir.path());
        // The whole point of #42: the earlier run survives the relaunch.
        assert!(contents.contains("first run"), "{contents:?}");
        assert!(contents.contains("second run"), "{contents:?}");
    }

    #[test]
    fn log_is_created_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let f = open_log_file(dir.path()).unwrap();
        drop(f);
        assert!(dir.path().join("torrenttui.log").exists());
        assert!(!dir.path().join("torrenttui.log.1").exists());
    }

    #[test]
    fn oversized_log_is_rotated_on_open() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("torrenttui.log"),
            vec![b'x'; (MAX_LOG_BYTES + 1) as usize],
        )
        .unwrap();

        let mut f = open_log_file(dir.path()).unwrap();
        writeln!(f, "fresh run").unwrap();
        drop(f);

        let rotated = dir.path().join("torrenttui.log.1");
        assert!(rotated.exists(), "previous generation should be kept");
        assert_eq!(
            std::fs::metadata(&rotated).unwrap().len(),
            MAX_LOG_BYTES + 1
        );
        // The live log restarts from the rotation, not from 5 MB of history.
        let contents = read_log(dir.path());
        assert_eq!(contents, "fresh run\n");
    }

    #[test]
    fn log_under_the_cap_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("torrenttui.log"), b"keep me\n").unwrap();

        let mut f = open_log_file(dir.path()).unwrap();
        writeln!(f, "and me").unwrap();
        drop(f);

        assert!(!dir.path().join("torrenttui.log.1").exists());
        assert_eq!(read_log(dir.path()), "keep me\nand me\n");
    }

    #[test]
    fn rotation_replaces_the_older_generation() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("torrenttui.log.1"), b"ancient\n").unwrap();
        std::fs::write(
            dir.path().join("torrenttui.log"),
            vec![b'y'; (MAX_LOG_BYTES + 1) as usize],
        )
        .unwrap();

        drop(open_log_file(dir.path()).unwrap());

        // Only one previous generation is kept, so `ancient` is gone.
        let rotated = std::fs::read(dir.path().join("torrenttui.log.1")).unwrap();
        assert_eq!(rotated.len(), (MAX_LOG_BYTES + 1) as usize);
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

    fn torrent(id: usize, status: types::TorrentStatus) -> types::TorrentInfo {
        types::TorrentInfo {
            id,
            name: format!("t{id}"),
            size_bytes: 100,
            downloaded_bytes: 10,
            uploaded_bytes: 0,
            download_speed: 0,
            upload_speed: 0,
            peers_connected: 0,
            peers_total: 0,
            status,
            eta_seconds: None,
            files: Vec::new(),
            peers: Vec::new(),
            info_hash: String::new(),
            trackers: Vec::new(),
            piece_length: None,
            content_path: None,
        }
    }

    #[tokio::test]
    async fn bulk_pause_resumes_when_any_marked_torrent_is_paused() {
        // "Any paused → resume all" over the marked set.
        let (tx, mut rx) = mpsc::channel::<EngineCommand>(8);
        let mut app = App::new();
        let mut iw = InputWidget::new();
        app.handle_state_push(vec![
            torrent(0, types::TorrentStatus::Downloading),
            torrent(1, types::TorrentStatus::Paused),
        ]);
        app.marked_ids.insert(0);
        app.marked_ids.insert(1);
        handle_normal_mode(&mut app, &mut iw, key(KeyCode::Char('p')), &tx).await;
        match rx.try_recv().expect("a command should have been sent") {
            EngineCommand::ResumeMany(ids) => assert_eq!(ids.len(), 2),
            other => panic!("expected ResumeMany, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn p_resumes_a_paused_torrent() {
        // Paused always means user-paused now that speed limits live in
        // librqbit's rate limiter, so `p` on a Paused row is always Resume.
        let (tx, mut rx) = mpsc::channel::<EngineCommand>(8);
        let mut app = App::new();
        let mut iw = InputWidget::new();
        app.handle_state_push(vec![torrent(0, types::TorrentStatus::Paused)]);
        handle_normal_mode(&mut app, &mut iw, key(KeyCode::Char('p')), &tx).await;
        assert!(matches!(
            rx.try_recv().expect("a command"),
            EngineCommand::Resume(0)
        ));
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

    fn ctrl(code: KeyCode) -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    #[tokio::test]
    async fn colon_opens_the_palette_from_every_hosting_mode() {
        let (tx, _rx) = mpsc::channel::<EngineCommand>(8);
        let (search_tx, _search_rx) = search_channel();
        let mut client = None;
        let mut iw = InputWidget::new();

        let mut app = App::new();
        handle_normal_mode(&mut app, &mut iw, key(KeyCode::Char(':')), &tx).await;
        assert_eq!(app.mode, AppMode::Palette);
        assert_eq!(app.palette.return_mode, AppMode::Normal);

        let mut app = App::new();
        app.mode = AppMode::Detail;
        handle_detail_mode(&mut app, key(KeyCode::Char(':')), &tx).await;
        assert_eq!(app.mode, AppMode::Palette);
        assert_eq!(app.palette.return_mode, AppMode::Detail);

        let mut app = App::new();
        app.mode = AppMode::SearchResults;
        handle_search_results_mode(
            &mut app,
            key(KeyCode::Char(':')),
            &tx,
            &search_tx,
            &mut client,
        )
        .await;
        assert_eq!(app.mode, AppMode::Palette);
        assert_eq!(app.palette.return_mode, AppMode::SearchResults);
    }

    #[tokio::test]
    async fn palette_enter_runs_the_filtered_action() {
        // Type "pause all", Enter: the palette must dispatch the same
        // PauseAll the `P` key sends, and land back in Normal mode.
        let (tx, mut rx) = mpsc::channel::<EngineCommand>(8);
        let (search_tx, _search_rx) = search_channel();
        let mut client = None;
        let mut iw = InputWidget::new();
        let mut app = App::new();
        app.handle_state_push(vec![torrent(0, types::TorrentStatus::Downloading)]);
        assert!(app.open_palette());
        for c in "pause all".chars() {
            handle_palette_mode(
                &mut app,
                key(KeyCode::Char(c)),
                &mut iw,
                &tx,
                &search_tx,
                &mut client,
            )
            .await;
        }
        let top = app.palette_matches();
        assert!(
            actions::tui_description(top[0]).contains("all torrents"),
            "top match should be Pause/unpause all, got {:?}",
            actions::tui_description(top[0])
        );
        handle_palette_mode(
            &mut app,
            key(KeyCode::Enter),
            &mut iw,
            &tx,
            &search_tx,
            &mut client,
        )
        .await;
        assert!(matches!(
            rx.try_recv().expect("a command"),
            EngineCommand::PauseAll
        ));
        assert_eq!(app.mode, AppMode::Normal);
    }

    #[tokio::test]
    async fn palette_esc_returns_and_ctrl_jk_navigates() {
        let (tx, _rx) = mpsc::channel::<EngineCommand>(8);
        let (search_tx, _search_rx) = search_channel();
        let mut client = None;
        let mut iw = InputWidget::new();
        let mut app = App::new();
        app.mode = AppMode::SearchResults;
        assert!(app.open_palette());
        handle_palette_mode(
            &mut app,
            ctrl(KeyCode::Char('j')),
            &mut iw,
            &tx,
            &search_tx,
            &mut client,
        )
        .await;
        assert_eq!(app.palette_selected_index(&app.palette_matches()), 1);
        handle_palette_mode(
            &mut app,
            ctrl(KeyCode::Char('k')),
            &mut iw,
            &tx,
            &search_tx,
            &mut client,
        )
        .await;
        assert_eq!(app.palette_selected_index(&app.palette_matches()), 0);
        // Plain j is typing, not navigation.
        handle_palette_mode(
            &mut app,
            key(KeyCode::Char('j')),
            &mut iw,
            &tx,
            &search_tx,
            &mut client,
        )
        .await;
        assert_eq!(app.palette.input, "j");
        handle_palette_mode(
            &mut app,
            key(KeyCode::Esc),
            &mut iw,
            &tx,
            &search_tx,
            &mut client,
        )
        .await;
        assert_eq!(app.mode, AppMode::SearchResults);
    }

    #[tokio::test]
    async fn palette_enter_on_a_vanished_anchor_fires_nothing_once() {
        // The review's misfire scenario: anchor an action, let a background
        // event remove it from the match list, press Enter. Nothing may
        // execute; the cursor re-anchors to the top instead.
        let (tx, mut rx) = mpsc::channel::<EngineCommand>(8);
        let (search_tx, _search_rx) = search_channel();
        let mut client = None;
        let mut iw = InputWidget::new();
        let mut app = App::new();
        app.mode = AppMode::SearchResults;
        app.search.searched_once = true;
        app.search.query = "q".to_string();
        app.search.results = vec![search_result_fixture()];
        assert!(app.open_palette());
        // Anchor "Download the selected result" (alphabetically third with
        // an empty query, behind "Back…" and "Cycle result sort column";
        // present because no search is in flight).
        for _ in 0..2 {
            handle_palette_mode(
                &mut app,
                ctrl(KeyCode::Char('j')),
                &mut iw,
                &tx,
                &search_tx,
                &mut client,
            )
            .await;
        }
        let anchored = app.palette.anchor.expect("anchored");
        assert!(actions::tui_description(anchored).contains("Download"));
        // Background change: a retry starts, hiding DownloadResult.
        app.search.in_flight = true;
        handle_palette_mode(
            &mut app,
            key(KeyCode::Enter),
            &mut iw,
            &tx,
            &search_tx,
            &mut client,
        )
        .await;
        assert!(rx.try_recv().is_err(), "vanished anchor must not fire");
        assert_eq!(
            app.mode,
            AppMode::Palette,
            "palette stays open, re-anchored"
        );
        assert!(app.palette.anchor.is_none());
    }

    #[tokio::test]
    async fn palette_action_executes_in_the_detail_context() {
        // CycleTab chosen from a Detail-opened palette must mutate detail
        // state exactly like pressing Tab in Detail.
        let (tx, _rx) = mpsc::channel::<EngineCommand>(8);
        let (search_tx, _search_rx) = search_channel();
        let mut client = None;
        let mut iw = InputWidget::new();
        let mut app = App::new();
        app.handle_state_push(vec![torrent(0, types::TorrentStatus::Downloading)]);
        app.mode = AppMode::Detail;
        assert!(app.open_palette());
        for c in "cycle detail".chars() {
            handle_palette_mode(
                &mut app,
                key(KeyCode::Char(c)),
                &mut iw,
                &tx,
                &search_tx,
                &mut client,
            )
            .await;
        }
        handle_palette_mode(
            &mut app,
            key(KeyCode::Enter),
            &mut iw,
            &tx,
            &search_tx,
            &mut client,
        )
        .await;
        assert_eq!(app.mode, AppMode::Detail);
        assert_eq!(app.detail_tab, DetailTab::Info);
    }

    #[test]
    fn open_folder_with_nothing_selected_is_a_no_op() {
        // Must return before touching the filesystem or spawning anything —
        // the selection guard is the only thing between `o` and a spawn.
        let mut app = App::new();
        handle_open_folder(&mut app);
        assert!(app.info_message.is_none());
        assert!(app.error_message.is_none());
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

    fn search_channel() -> (
        mpsc::Sender<search::SearchOutcome>,
        mpsc::Receiver<search::SearchOutcome>,
    ) {
        mpsc::channel::<search::SearchOutcome>(4)
    }

    fn search_result_fixture() -> search::SearchResult {
        search::SearchResult {
            title: "Arch Linux ISO".to_string(),
            info_hash: "88066b90278f2de655ee2dd44e784c340b54e45c".to_string(),
            size_bytes: Some(735051776),
            seeders: 12,
            leechers: 1,
            source: search::SourceSet {
                apibay: true,
                torrents_csv: false,
            },
        }
    }

    #[tokio::test]
    async fn normal_mode_s_opens_search() {
        let (tx, _rx) = mpsc::channel::<EngineCommand>(8);
        let mut app = App::new();
        let mut iw = InputWidget::new();
        handle_normal_mode(&mut app, &mut iw, key(KeyCode::Char('s')), &tx).await;
        assert_eq!(app.mode, AppMode::Search);
    }

    /// A client that cannot reach the network: everything is proxied to a
    /// closed local port, so the spawned provider task fails instantly with
    /// "connection refused" instead of touching real indexers from CI.
    fn offline_client() -> Option<reqwest::Client> {
        Some(
            reqwest::Client::builder()
                .proxy(reqwest::Proxy::all("http://127.0.0.1:9").expect("static proxy url"))
                .timeout(std::time::Duration::from_millis(200))
                .build()
                .expect("client builds"),
        )
    }

    #[tokio::test]
    async fn search_input_enter_fires_and_switches_to_results() {
        // Needs a runtime because start_search spawns the provider task; the
        // offline client makes that task fail locally and its outcome dies
        // unobserved on the channel we hold.
        let (search_tx, _search_rx) = search_channel();
        let mut client = offline_client();
        let mut app = App::new();
        app.mode = AppMode::Search;
        for c in "arch".chars() {
            handle_search_input_mode(&mut app, key(KeyCode::Char(c)), &search_tx, &mut client);
        }
        assert_eq!(app.search.input, "arch");
        handle_search_input_mode(&mut app, key(KeyCode::Enter), &search_tx, &mut client);
        assert_eq!(app.mode, AppMode::SearchResults);
        assert!(app.search.in_flight);
        assert_eq!(app.search.generation, 1);
    }

    #[tokio::test]
    async fn search_input_enter_on_empty_query_is_a_no_op() {
        let (search_tx, _search_rx) = search_channel();
        let mut client = None;
        let mut app = App::new();
        app.mode = AppMode::Search;
        handle_search_input_mode(&mut app, key(KeyCode::Enter), &search_tx, &mut client);
        assert_eq!(app.mode, AppMode::Search);
        assert!(!app.search.in_flight);
        assert_eq!(app.search.generation, 0);
    }

    #[test]
    fn search_input_esc_goes_to_results_only_once_one_exists() {
        let (search_tx, _search_rx) = search_channel();
        let mut client = None;
        let mut app = App::new();
        app.mode = AppMode::Search;
        handle_search_input_mode(&mut app, key(KeyCode::Esc), &search_tx, &mut client);
        assert_eq!(app.mode, AppMode::Normal);

        app.mode = AppMode::Search;
        app.search.searched_once = true;
        handle_search_input_mode(&mut app, key(KeyCode::Esc), &search_tx, &mut client);
        assert_eq!(app.mode, AppMode::SearchResults);
    }

    #[test]
    fn search_with_all_providers_disabled_errors_without_spawning() {
        let (search_tx, _search_rx) = search_channel();
        let mut client = None;
        let mut app = App::new();
        app.search_config.enable_apibay = false;
        app.search_config.enable_torrents_csv = false;
        app.mode = AppMode::Search;
        app.search.input = "arch".to_string();
        handle_search_input_mode(&mut app, key(KeyCode::Enter), &search_tx, &mut client);
        assert_eq!(app.mode, AppMode::Search);
        assert!(!app.search.in_flight);
        assert!(app
            .error_message
            .as_deref()
            .is_some_and(|m| m.contains("disabled")));
        assert!(client.is_none());
    }

    #[tokio::test]
    async fn search_results_tab_cycles_sort_and_shift_r_reverses() {
        let (tx, _rx) = mpsc::channel::<EngineCommand>(8);
        let (search_tx, _search_rx) = search_channel();
        let mut client = None;
        let mut app = App::new();
        app.mode = AppMode::SearchResults;
        app.search.results = vec![search_result_fixture()];
        handle_search_results_mode(&mut app, key(KeyCode::Tab), &tx, &search_tx, &mut client).await;
        assert_eq!(
            app.search.sort_column,
            crate::app::ResultSortColumn::Size,
            "Tab moves off the Seeders default"
        );
        assert!(!app.search.sort_reversed);
        handle_search_results_mode(
            &mut app,
            key(KeyCode::Char('R')),
            &tx,
            &search_tx,
            &mut client,
        )
        .await;
        assert!(app.search.sort_reversed, "R flips the direction");
        // Sorting stays in the results view; nothing reaches the engine.
        assert_eq!(app.mode, AppMode::SearchResults);
    }

    #[tokio::test]
    async fn search_results_enter_sends_a_valid_magnet_and_marks_added() {
        let (tx, mut rx) = mpsc::channel::<EngineCommand>(8);
        let (search_tx, _search_rx) = search_channel();
        let mut client = None;
        let mut app = App::new();
        app.mode = AppMode::SearchResults;
        app.search.results = vec![search_result_fixture()];
        handle_search_results_mode(&mut app, key(KeyCode::Enter), &tx, &search_tx, &mut client)
            .await;
        match rx.try_recv().expect("an AddTorrent command") {
            EngineCommand::AddTorrent(magnet) => {
                assert!(magnet
                    .starts_with("magnet:?xt=urn:btih:88066b90278f2de655ee2dd44e784c340b54e45c"));
                assert!(validate_magnet(&magnet).is_ok());
            }
            other => panic!("expected AddTorrent, got {other:?}"),
        }
        // Multi-grab: stays in the results view, row marked as added.
        assert_eq!(app.mode, AppMode::SearchResults);
        assert!(app
            .search
            .added
            .contains("88066b90278f2de655ee2dd44e784c340b54e45c"));
    }

    #[tokio::test]
    async fn search_results_double_enter_blocks_once_the_torrent_appears() {
        let (tx, mut rx) = mpsc::channel::<EngineCommand>(8);
        let (search_tx, _search_rx) = search_channel();
        let mut client = None;
        let mut app = App::new();
        app.mode = AppMode::SearchResults;
        app.search.results = vec![search_result_fixture()];
        handle_search_results_mode(&mut app, key(KeyCode::Enter), &tx, &search_tx, &mut client)
            .await;
        rx.try_recv().expect("first Enter sends");
        // Simulate the engine's state push confirming the add. Uppercase hash
        // exercises the case-insensitive presence check (search hashes are
        // normalized lowercase, the engine reports librqbit's formatting).
        let mut t = torrent(0, types::TorrentStatus::Downloading);
        t.info_hash = "88066B90278F2DE655EE2DD44E784C340B54E45C".to_string();
        app.handle_state_push(vec![t]);
        handle_search_results_mode(&mut app, key(KeyCode::Enter), &tx, &search_tx, &mut client)
            .await;
        assert!(rx.try_recv().is_err(), "second Enter must not send");
        assert!(app
            .info_message
            .as_deref()
            .is_some_and(|m| m.contains("Already added")));
    }

    #[tokio::test]
    async fn search_results_enter_readds_after_the_torrent_is_gone() {
        // A deleted torrent (or a failed add) must stay re-addable: the ✓ set
        // remembers the grab, but the guard defers to live session state.
        let (tx, mut rx) = mpsc::channel::<EngineCommand>(8);
        let (search_tx, _search_rx) = search_channel();
        let mut client = None;
        let mut app = App::new();
        app.mode = AppMode::SearchResults;
        app.search.results = vec![search_result_fixture()];
        app.search
            .added
            .insert("88066b90278f2de655ee2dd44e784c340b54e45c".to_string());
        assert!(app.torrents.is_empty());
        handle_search_results_mode(&mut app, key(KeyCode::Enter), &tx, &search_tx, &mut client)
            .await;
        assert!(
            matches!(rx.try_recv(), Ok(EngineCommand::AddTorrent(_))),
            "Enter must re-send when the torrent is no longer in the session"
        );
    }

    #[tokio::test]
    async fn search_results_enter_while_loading_is_inert() {
        let (tx, mut rx) = mpsc::channel::<EngineCommand>(8);
        let (search_tx, _search_rx) = search_channel();
        let mut client = None;
        let mut app = App::new();
        app.mode = AppMode::SearchResults;
        app.search.results = vec![search_result_fixture()];
        app.search.in_flight = true;
        handle_search_results_mode(&mut app, key(KeyCode::Enter), &tx, &search_tx, &mut client)
            .await;
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn search_results_esc_and_q_leave_state_intact() {
        let (tx, _rx) = mpsc::channel::<EngineCommand>(8);
        let (search_tx, _search_rx) = search_channel();
        let mut client = None;
        for code in [KeyCode::Esc, KeyCode::Char('q')] {
            let mut app = App::new();
            app.mode = AppMode::SearchResults;
            app.search.searched_once = true;
            app.search.results = vec![search_result_fixture()];
            handle_search_results_mode(&mut app, key(code), &tx, &search_tx, &mut client).await;
            assert_eq!(app.mode, AppMode::Normal);
            assert_eq!(app.search.results.len(), 1, "results persist");
        }
    }

    #[tokio::test]
    async fn search_results_s_reopens_the_prefilled_input() {
        let (tx, _rx) = mpsc::channel::<EngineCommand>(8);
        let (search_tx, _search_rx) = search_channel();
        let mut client = None;
        let mut app = App::new();
        app.mode = AppMode::SearchResults;
        app.search.input = "arch".to_string();
        handle_search_results_mode(
            &mut app,
            key(KeyCode::Char('s')),
            &tx,
            &search_tx,
            &mut client,
        )
        .await;
        assert_eq!(app.mode, AppMode::Search);
        assert_eq!(app.search.input, "arch");
    }
}
