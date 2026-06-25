//! dogma — thin binary that wires the library into a tokio runtime + terminal.

use dogma::{api, app, auth, cli, config, ui};

use std::io;
use std::time::Duration;

use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::info;
use tracing_subscriber::EnvFilter;

use crate::api::HttpClient;
use crate::api::types::{HttpReq, HttpResult, HttpResultKind, WsClientMsg};
use crate::api::ws::WireOut;
use crate::app::action::Action;
use crate::app::event::AppEvent;
use crate::app::state::App;
use crate::cli::Args;
use crate::config::Config;

fn main() -> ExitCode {
    let args = Args::parse();
    if let Err(e) = init_logging(args.verbose) {
        eprintln!("error: failed to init logging: {e:#}");
        return ExitCode::from(2);
    }

    let cfg = Config::load()
        .unwrap_or_default()
        .override_server(args.server.clone());

    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: build tokio runtime: {e:#}");
            return ExitCode::from(2);
        }
    };

    match rt.block_on(run(args, cfg)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(code) => ExitCode::from(code),
    }
}

async fn run(args: Args, cfg: Config) -> std::result::Result<(), u8> {
    // 1. Pre-TUI auth (plain stdio). Errors short-circuit before we touch
    //    the terminal.
    let auth = match auth::authenticate(&cfg.server).await {
        Ok(a) => a,
        Err(auth::AuthError::Connect(msg)) => {
            eprintln!("error: {msg}");
            eprintln!("hint:  is `nerve start` running at {} ?", cfg.server);
            return Err(2);
        }
        Err(auth::AuthError::BadPassword(_n)) => {
            eprintln!("error: authentication failed");
            return Err(1);
        }
        Err(auth::AuthError::Other(msg)) => {
            eprintln!("error: {msg}");
            return Err(2);
        }
    };

    // 2. Set up channels and background tasks.
    let (event_tx, event_rx) = mpsc::channel::<AppEvent>(512);
    let (action_tx, action_rx) = mpsc::channel::<Action>(256);

    // WS task — bridges WS frames into AppEvent::Wire/WireConn.
    let (ws_bridge_tx, mut ws_bridge_rx) = mpsc::channel::<WireOut>(512);
    let ws_action_tx = api::ws::spawn(cfg.server.clone(), auth.clone(), ws_bridge_tx);

    // HTTP worker — shares the AuthHandle so it reissues the token on 401.
    let http_client = HttpClient::with_auth(&cfg.server, auth.clone()).map_err(|e| {
        eprintln!("error: {e:#}");
        2
    })?;
    spawn_http_worker(
        http_client.clone(),
        action_rx,
        event_tx.clone(),
        ws_action_tx.clone(),
    );

    // WS bridge → AppEvent forwarder
    {
        let event_tx = event_tx.clone();
        tokio::spawn(async move {
            while let Some(ev) = ws_bridge_rx.recv().await {
                let app_ev = match ev {
                    WireOut::Server(m) => AppEvent::Wire(m),
                    WireOut::Conn(c) => AppEvent::WireConn(c),
                };
                if event_tx.send(app_ev).await.is_err() {
                    break;
                }
            }
        });
    }

    // Tick task.
    {
        let event_tx = event_tx.clone();
        tokio::spawn(async move {
            let mut t = tokio::time::interval(Duration::from_millis(250));
            loop {
                t.tick().await;
                if event_tx.send(AppEvent::Tick).await.is_err() {
                    break;
                }
            }
        });
    }

    // Initial HTTP requests.
    action_tx
        .send(Action::Http(HttpReq::ListSessions))
        .await
        .ok();

    // 3. Initialize terminal.
    if let Err(e) = enable_raw_mode() {
        eprintln!("error: enable raw mode: {e:#}");
        return Err(2);
    }
    let mut stdout = io::stdout();
    if let Err(e) = execute!(stdout, EnterAlternateScreen) {
        eprintln!("error: enter alt screen: {e:#}");
        let _ = disable_raw_mode();
        return Err(2);
    }
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = match Terminal::new(backend) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: create terminal: {e:#}");
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
            return Err(2);
        }
    };

    // Panic hook: restore terminal before propagating.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        default_hook(info);
    }));

    // 4. Build App and run main loop.
    let mut app = App::new(cfg.server.clone(), auth.token().await);
    let loop_result = main_loop(&mut app, &mut terminal, event_tx, event_rx, action_tx, args).await;

    // 5. Restore terminal.
    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    let _ = terminal.show_cursor();

    if let Some(fatal) = app.fatal {
        eprintln!("fatal: {fatal}");
        return Err(1);
    }
    match loop_result {
        Ok(()) => Ok(()),
        Err(e) => {
            eprintln!("error: {e:#}");
            Err(1)
        }
    }
}

async fn main_loop(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    _event_tx: mpsc::Sender<AppEvent>,
    mut event_rx: mpsc::Receiver<AppEvent>,
    action_tx: mpsc::Sender<Action>,
    args: Args,
) -> anyhow::Result<()> {
    // Spawn a thread to read crossterm events and forward to event_tx.
    let term_tx = action_tx.clone();
    let _ = term_tx; // unused for now; input pump uses event_tx

    let term_event_tx = _event_tx;
    std::thread::spawn(move || -> Result<()> {
        loop {
            if crossterm::event::poll(Duration::from_millis(100)).context("poll terminal events")? {
                let ev = crossterm::event::read().context("read terminal event")?;
                if term_event_tx.blocking_send(AppEvent::Term(ev)).is_err() {
                    break;
                }
            }
        }
        Ok(())
    });

    // Optional --session: switch right after connect.
    if let Some(sid) = args.session.clone() {
        action_tx
            .send(Action::Ws(WsClientMsg::SwitchSession { session_id: sid }))
            .await
            .ok();
    }

    // Periodically refresh notifications so the nav-rail alert and the notifs
    // tab stay current without a manual reload. The first tick fires
    // immediately, populating the pending indicator at startup.
    let notif_tx = action_tx.clone();
    tokio::spawn(async move {
        let mut iv = tokio::time::interval(Duration::from_secs(10));
        loop {
            iv.tick().await;
            if notif_tx
                .send(Action::Http(HttpReq::ListNotifications))
                .await
                .is_err()
            {
                break;
            }
        }
    });

    let mut last_render = Instant::now() - Duration::from_secs(1);
    let render_interval = Duration::from_millis(33);

    loop {
        let event = match event_rx.recv().await {
            Some(e) => e,
            None => break,
        };

        let actions = app::update::update(app, event);
        for a in actions {
            match a {
                Action::Quit => {
                    app.should_quit = true;
                }
                _ => {
                    let _ = action_tx.send(a).await;
                }
            }
        }

        if app.should_quit {
            break;
        }

        if app.dirty && last_render.elapsed() >= render_interval {
            terminal.draw(|f| ui::render(app, f)).context("draw")?;
            app.dirty = false;
            last_render = Instant::now();
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// HTTP worker
// ---------------------------------------------------------------------------

fn spawn_http_worker(
    http: HttpClient,
    mut rx: mpsc::Receiver<Action>,
    event_tx: mpsc::Sender<AppEvent>,
    ws_tx: mpsc::Sender<WsClientMsg>,
) {
    // The lazy "create + send" flow carries the first message on the
    // `CreateSession` action itself (see app/state.rs::send_input), so the
    // worker just forwards it back as `pending_content`.
    tokio::spawn(async move {
        while let Some(action) = rx.recv().await {
            match action {
                Action::Http(HttpReq::ListSessions) => {
                    let r = http.list_sessions().await.map_err(|e| format!("{e:#}"));
                    let _ = event_tx
                        .send(AppEvent::Http(HttpResult {
                            kind: HttpResultKind::Sessions(r),
                        }))
                        .await;
                }
                Action::Http(HttpReq::GetMessages { session_id, limit }) => {
                    let r = http
                        .get_messages(&session_id, limit)
                        .await
                        .map_err(|e| format!("{e:#}"));
                    let _ = event_tx
                        .send(AppEvent::Http(HttpResult {
                            kind: HttpResultKind::Messages {
                                session_id,
                                limit,
                                result: r,
                            },
                        }))
                        .await;
                }
                Action::Http(HttpReq::CreateSession { title, content }) => {
                    let r = http
                        .create_session(title.as_deref())
                        .await
                        .map_err(|e| format!("{e:#}"));
                    // The first message rides along on the action; the App
                    // sends it once the session id is known. None = just create.
                    let _ = event_tx
                        .send(AppEvent::Http(HttpResult {
                            kind: HttpResultKind::SessionCreated {
                                pending_content: content,
                                result: r,
                            },
                        }))
                        .await;
                }
                Action::Http(HttpReq::ListTasks) => {
                    let r = http.list_tasks().await.map_err(|e| format!("{e:#}"));
                    let _ = event_tx
                        .send(AppEvent::Http(HttpResult {
                            kind: HttpResultKind::Tasks(r),
                        }))
                        .await;
                }
                Action::Http(HttpReq::GetTask { task_id }) => {
                    let r = http.get_task(&task_id).await.map_err(|e| format!("{e:#}"));
                    let _ = event_tx
                        .send(AppEvent::Http(HttpResult {
                            kind: HttpResultKind::TaskDetail { task_id, result: r },
                        }))
                        .await;
                }
                Action::Http(HttpReq::ListPlans) => {
                    let r = http.list_plans().await.map_err(|e| format!("{e:#}"));
                    let _ = event_tx
                        .send(AppEvent::Http(HttpResult {
                            kind: HttpResultKind::Plans(r),
                        }))
                        .await;
                }
                Action::Http(HttpReq::GetPlan { plan_id }) => {
                    let r = http.get_plan(&plan_id).await.map_err(|e| format!("{e:#}"));
                    let _ = event_tx
                        .send(AppEvent::Http(HttpResult {
                            kind: HttpResultKind::PlanDetail { plan_id, result: r },
                        }))
                        .await;
                }
                Action::Http(HttpReq::ListSkills) => {
                    let r = http.list_skills().await.map_err(|e| format!("{e:#}"));
                    let _ = event_tx
                        .send(AppEvent::Http(HttpResult {
                            kind: HttpResultKind::Skills(r),
                        }))
                        .await;
                }
                Action::Http(HttpReq::GetSkill { skill_id }) => {
                    let r = http
                        .get_skill(&skill_id)
                        .await
                        .map_err(|e| format!("{e:#}"));
                    let _ = event_tx
                        .send(AppEvent::Http(HttpResult {
                            kind: HttpResultKind::SkillDetail {
                                skill_id,
                                result: r,
                            },
                        }))
                        .await;
                }
                Action::Http(HttpReq::ListNotifications) => {
                    let r = http
                        .list_notifications()
                        .await
                        .map_err(|e| format!("{e:#}"));
                    let _ = event_tx
                        .send(AppEvent::Http(HttpResult {
                            kind: HttpResultKind::Notifications(r),
                        }))
                        .await;
                }
                Action::Http(HttpReq::AnswerNotification { id, answer }) => {
                    let result = http
                        .answer_notification(&id, &answer)
                        .await
                        .map_err(|e| format!("{e:#}"));
                    let _ = event_tx
                        .send(AppEvent::Http(HttpResult {
                            kind: HttpResultKind::NotificationAnswered { id, answer, result },
                        }))
                        .await;
                }
                Action::Http(HttpReq::DismissNotification { id }) => {
                    let result = http
                        .dismiss_notification(&id)
                        .await
                        .map_err(|e| format!("{e:#}"));
                    let _ = event_tx
                        .send(AppEvent::Http(HttpResult {
                            kind: HttpResultKind::NotificationDismissed { id, result },
                        }))
                        .await;
                }
                Action::Http(HttpReq::GetModifiedFiles { session_id }) => {
                    let result = http
                        .modified_files(&session_id)
                        .await
                        .map_err(|e| format!("{e:#}"));
                    let _ = event_tx
                        .send(AppEvent::Http(HttpResult {
                            kind: HttpResultKind::ModifiedFiles { session_id, result },
                        }))
                        .await;
                }
                Action::Http(HttpReq::GetFileDiff { session_id, path }) => {
                    let result = http
                        .file_diff(&session_id, &path)
                        .await
                        .map_err(|e| format!("{e:#}"));
                    let _ = event_tx
                        .send(AppEvent::Http(HttpResult {
                            kind: HttpResultKind::FileDiff {
                                session_id,
                                path,
                                result,
                            },
                        }))
                        .await;
                }
                Action::Ws(msg) => {
                    let _ = ws_tx.send(msg).await;
                }
                Action::Quit => break,
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

fn init_logging(verbose: bool) -> Result<()> {
    let path = config::log_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .context("open log file")?;
    let filter = if verbose {
        EnvFilter::new("debug,hyper=info,reqwest=info,h2=info,tungstenite=info")
    } else {
        EnvFilter::new("info,hyper=warn,reqwest=warn,h2=warn,tungstenite=warn")
    };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::sync::Mutex::new(file))
        .with_ansi(false)
        .init();
    info!(
        version = env!("CARGO_PKG_VERSION"),
        log = %path.display(),
        "dogma starting"
    );
    Ok(())
}
