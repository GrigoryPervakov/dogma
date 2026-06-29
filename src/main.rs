//! dogma — thin binary that wires the library into a tokio runtime + terminal.

use dogma::{api, app, auth, cli, config, instance, ui};

use std::io;
use std::time::Duration;

use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, SetTitle, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::info;
use tracing_subscriber::EnvFilter;

use crate::api::HttpClient;
use crate::api::types::{HttpReq, HttpResult, HttpResultKind, Token, WsClientMsg};
use crate::api::ws::WireOut;
use crate::app::action::Action;
use crate::app::event::{AppEvent, ConnEvent};
use crate::app::state::{App, InstanceMeta};
use crate::cli::Args;
use crate::config::Config;
use crate::instance::InstanceId;

/// What a per-instance worker consumes: either a WS frame to forward or an HTTP
/// request to run. The runtime routes instance-tagged `Action`s into the right
/// worker's channel.
enum WorkerMsg {
    Ws(WsClientMsg),
    Http(HttpReq),
}

fn main() -> ExitCode {
    let args = Args::parse();
    if let Err(e) = init_logging(args.verbose) {
        eprintln!("error: failed to init logging: {e:#}");
        return ExitCode::from(2);
    }

    let cfg = Config::load().unwrap_or_default();

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

/// Terminal title: `Dogma: <label>` for one instance, `Dogma: <label> +N` when
/// several are connected.
fn title(instances: &[InstanceMeta]) -> String {
    match instances.split_first() {
        Some((first, rest)) if !rest.is_empty() => {
            format!("Dogma: {} +{}", first.label, rest.len())
        }
        Some((first, _)) => format!("Dogma: {}", first.label),
        None => "Dogma".to_string(),
    }
}

async fn run(args: Args, cfg: Config) -> std::result::Result<(), u8> {
    // 1. Resolve the instance list and authenticate each (plain stdio, pre-TUI).
    let servers = cfg.resolve_servers(&args.server);
    let mut instances: Vec<InstanceMeta> = Vec::with_capacity(servers.len());
    let mut auths = Vec::with_capacity(servers.len());
    for (i, sc) in servers.iter().enumerate() {
        // A single instance that's unreachable or rejects auth must not abort
        // the whole app — bring it up offline (workers keep retrying and log in
        // via the stored password once it's reachable). Only a genuine config
        // error (bad URL) is fatal.
        let auth = match auth::authenticate(&sc.url, sc.password.as_deref()).await {
            Ok(a) => a,
            Err(auth::AuthError::Connect(msg)) => {
                eprintln!(
                    "warning: {} unreachable ({msg}); will keep retrying",
                    sc.label()
                );
                api::AuthHandle::new(sc.url.clone(), sc.password.clone(), Token::empty())
            }
            Err(auth::AuthError::BadPassword(_n)) => {
                eprintln!(
                    "warning: authentication failed for {}; staying offline",
                    sc.label()
                );
                api::AuthHandle::new(sc.url.clone(), sc.password.clone(), Token::empty())
            }
            Err(auth::AuthError::Other(msg)) => {
                eprintln!("error: {msg}");
                return Err(2);
            }
        };
        instances.push(InstanceMeta::new(InstanceId(i), sc.label(), sc.url.clone()));
        auths.push(auth);
    }

    // 2. Channels + per-instance background tasks (one WS task + one worker each).
    let (event_tx, event_rx) = mpsc::channel::<AppEvent>(512);
    let mut workers: Vec<mpsc::Sender<WorkerMsg>> = Vec::with_capacity(instances.len());

    for (meta, auth) in instances.iter().zip(auths.iter()) {
        let id = meta.id;

        // WS task — bridges WS frames into AppEvent::Inst{Wire/WireConn}.
        let (ws_bridge_tx, mut ws_bridge_rx) = mpsc::channel::<WireOut>(512);
        let ws_action_tx = api::ws::spawn(meta.server.clone(), auth.clone(), ws_bridge_tx);
        {
            let event_tx = event_tx.clone();
            tokio::spawn(async move {
                while let Some(ev) = ws_bridge_rx.recv().await {
                    let conn = match ev {
                        WireOut::Server(m) => ConnEvent::Wire(m),
                        WireOut::Conn(c) => ConnEvent::WireConn(c),
                    };
                    if event_tx
                        .send(AppEvent::Inst {
                            instance: id,
                            ev: Box::new(conn),
                        })
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            });
        }

        // HTTP worker — shares the AuthHandle so it reissues the token on 401.
        let http_client = HttpClient::with_auth(&meta.server, auth.clone()).map_err(|e| {
            eprintln!("error: {e:#}");
            2u8
        })?;
        let (worker_tx, worker_rx) = mpsc::channel::<WorkerMsg>(256);
        spawn_instance_worker(id, http_client, ws_action_tx, worker_rx, event_tx.clone());
        workers.push(worker_tx);
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

    // Initial session list from every instance.
    for w in &workers {
        let _ = w.send(WorkerMsg::Http(HttpReq::ListSessions)).await;
    }

    // 3. Initialize terminal.
    if let Err(e) = enable_raw_mode() {
        eprintln!("error: enable raw mode: {e:#}");
        return Err(2);
    }
    let mut stdout = io::stdout();
    if let Err(e) = execute!(stdout, EnterAlternateScreen, SetTitle(title(&instances))) {
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
    let mut app = App::with_instances(instances);
    let loop_result = main_loop(&mut app, &mut terminal, event_tx, event_rx, workers, args).await;

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
    workers: Vec<mpsc::Sender<WorkerMsg>>,
    args: Args,
) -> anyhow::Result<()> {
    // Spawn a thread to read crossterm events and forward to event_tx.
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

    // Optional --session: switch right after connect, on the primary instance.
    if let Some(sid) = args.session.clone()
        && let Some(w) = workers.first()
    {
        w.send(WorkerMsg::Ws(WsClientMsg::SwitchSession {
            session_id: sid,
        }))
        .await
        .ok();
    }

    // Periodically refresh notifications on every instance so the nav-rail alert
    // and the notifs tab stay current. The first tick fires immediately.
    {
        let notif_workers = workers.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_secs(10));
            loop {
                iv.tick().await;
                for w in &notif_workers {
                    if w.send(WorkerMsg::Http(HttpReq::ListNotifications))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            }
        });
    }

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
                Action::Ws { instance, msg } => {
                    if let Some(w) = workers.get(instance.index()) {
                        let _ = w.send(WorkerMsg::Ws(msg)).await;
                    }
                }
                Action::Http { instance, req } => {
                    if let Some(w) = workers.get(instance.index()) {
                        let _ = w.send(WorkerMsg::Http(req)).await;
                    }
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
// Per-instance worker
// ---------------------------------------------------------------------------

fn spawn_instance_worker(
    instance: InstanceId,
    http: HttpClient,
    ws_tx: mpsc::Sender<WsClientMsg>,
    mut rx: mpsc::Receiver<WorkerMsg>,
    event_tx: mpsc::Sender<AppEvent>,
) {
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            match msg {
                WorkerMsg::Ws(m) => {
                    let _ = ws_tx.send(m).await;
                }
                WorkerMsg::Http(req) => {
                    let kind = run_http(&http, req).await;
                    let _ = event_tx
                        .send(AppEvent::Inst {
                            instance,
                            ev: Box::new(ConnEvent::Http(HttpResult { kind })),
                        })
                        .await;
                }
            }
        }
    });
}

/// Run one HTTP request and map it to its typed `HttpResultKind`. The lazy
/// "create + send" flow carries the first message on the `CreateSession`
/// request itself; the App sends it once the session id is known.
async fn run_http(http: &HttpClient, req: HttpReq) -> HttpResultKind {
    match req {
        HttpReq::ListSessions => {
            HttpResultKind::Sessions(http.list_sessions().await.map_err(|e| format!("{e:#}")))
        }
        HttpReq::GetMessages { session_id, limit } => {
            let result = http
                .get_messages(&session_id, limit)
                .await
                .map_err(|e| format!("{e:#}"));
            HttpResultKind::Messages {
                session_id,
                limit,
                result,
            }
        }
        HttpReq::CreateSession { title, content } => {
            let result = http
                .create_session(title.as_deref())
                .await
                .map_err(|e| format!("{e:#}"));
            HttpResultKind::SessionCreated {
                pending_content: content,
                result,
            }
        }
        HttpReq::ListTasks => {
            HttpResultKind::Tasks(http.list_tasks().await.map_err(|e| format!("{e:#}")))
        }
        HttpReq::GetTask { task_id } => {
            let result = http.get_task(&task_id).await.map_err(|e| format!("{e:#}"));
            HttpResultKind::TaskDetail { task_id, result }
        }
        HttpReq::ListPlans => {
            HttpResultKind::Plans(http.list_plans().await.map_err(|e| format!("{e:#}")))
        }
        HttpReq::GetPlan { plan_id } => {
            let result = http.get_plan(&plan_id).await.map_err(|e| format!("{e:#}"));
            HttpResultKind::PlanDetail { plan_id, result }
        }
        HttpReq::ListSkills => {
            HttpResultKind::Skills(http.list_skills().await.map_err(|e| format!("{e:#}")))
        }
        HttpReq::GetSkill { skill_id } => {
            let result = http
                .get_skill(&skill_id)
                .await
                .map_err(|e| format!("{e:#}"));
            HttpResultKind::SkillDetail { skill_id, result }
        }
        HttpReq::ListNotifications => HttpResultKind::Notifications(
            http.list_notifications()
                .await
                .map_err(|e| format!("{e:#}")),
        ),
        HttpReq::AnswerNotification { id, answer } => {
            let result = http
                .answer_notification(&id, &answer)
                .await
                .map_err(|e| format!("{e:#}"));
            HttpResultKind::NotificationAnswered { id, answer, result }
        }
        HttpReq::DismissNotification { id } => {
            let result = http
                .dismiss_notification(&id)
                .await
                .map_err(|e| format!("{e:#}"));
            HttpResultKind::NotificationDismissed { id, result }
        }
        HttpReq::GetModifiedFiles { session_id } => {
            let result = http
                .modified_files(&session_id)
                .await
                .map_err(|e| format!("{e:#}"));
            HttpResultKind::ModifiedFiles { session_id, result }
        }
        HttpReq::GetFileDiff { session_id, path } => {
            let result = http
                .file_diff(&session_id, &path)
                .await
                .map_err(|e| format!("{e:#}"));
            HttpResultKind::FileDiff {
                session_id,
                path,
                result,
            }
        }
    }
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
