mod auth;
mod cache;
mod live;
mod model;
mod pricing;
mod sources;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chrono::{Datelike, Local, NaiveDate};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};

use cache::FileCache;
use model::{AgentSummary, DayBucket, RefreshResult, SessionInfo, Totals, UsageRecord};

pub struct AppState {
    cache: Mutex<FileCache>,
    result: Mutex<Option<RefreshResult>>,
    last_refresh: Mutex<Option<Instant>>,
    quotas: Mutex<Option<(Instant, Vec<model::QuotaProvider>)>>,
    busy: AtomicBool,
}

impl Default for AppState {
    fn default() -> Self {
        AppState {
            cache: Mutex::new(FileCache::default()),
            result: Mutex::new(None),
            last_refresh: Mutex::new(None),
            quotas: Mutex::new(None),
            busy: AtomicBool::new(false),
        }
    }
}

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn fmt_compact_tokens(n: u64) -> String {
    if n >= 1_000_000_000 {
        format!("{:.1}B", n as f64 / 1_000_000_000.0)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

// ---------------------------------------------------------------------------
// aggregation
// ---------------------------------------------------------------------------

fn date_str(ts: i64) -> NaiveDate {
    chrono::DateTime::<chrono::Utc>::from_timestamp(ts, 0)
        .map(|d| d.with_timezone(&Local).date_naive())
        .unwrap_or_default()
}

fn totals_for(records: &[UsageRecord], pred: impl Fn(&UsageRecord) -> bool) -> Totals {
    let mut t = Totals::default();
    for r in records {
        if pred(r) {
            t.cost += r.cost;
            t.input += r.input;
            t.output += r.output;
            t.cache_creation += r.cache_creation;
            t.cache_read += r.cache_read;
        }
    }
    t
}

fn distinct_sessions(records: &[UsageRecord], pred: impl Fn(&UsageRecord) -> bool) -> u64 {
    let mut seen: HashSet<(&'static str, &str)> = HashSet::new();
    for r in records {
        if r.ts == 0 || !pred(r) {
            continue;
        }
        seen.insert((r.agent, r.session_id.as_str()));
    }
    seen.len() as u64
}

fn build_result(records: Vec<UsageRecord>, quotas: Vec<model::QuotaProvider>, errors: Vec<String>) -> RefreshResult {
    let today = Local::now().date_naive();

    let is_today = |r: &UsageRecord| date_str(r.ts) == today;
    let is_week = |r: &UsageRecord| {
        (today - date_str(r.ts)).num_days() <= 6 && date_str(r.ts) <= today
    };
    let is_month = |r: &UsageRecord| {
        let d = date_str(r.ts);
        d.year() == today.year() && d.month() == today.month()
    };

    let mut today_t = totals_for(&records, is_today);
    let mut week_t = totals_for(&records, is_week);
    let mut month_t = totals_for(&records, is_month);
    let mut all_t = totals_for(&records, |_| true);
    today_t.sessions = distinct_sessions(&records, is_today);
    week_t.sessions = distinct_sessions(&records, is_week);
    month_t.sessions = distinct_sessions(&records, is_month);
    all_t.sessions = distinct_sessions(&records, |_| true);

    // last 14 days, oldest first
    let mut days: Vec<DayBucket> = Vec::new();
    for back in (0..14).rev() {
        let date = today - chrono::Days::new(back as u64);
        let day_records: Vec<&UsageRecord> = records.iter().filter(|r| date_str(r.ts) == date).collect();
        let mut per_agent: Vec<(String, f64)> = Vec::new();
        let mut cost = 0.0;
        let mut input = 0;
        let mut output = 0;
        for r in &day_records {
            cost += r.cost;
            input += r.input;
            output += r.output;
            if let Some((_, c)) = per_agent.iter_mut().find(|(a, _)| a == r.agent) {
                *c += r.cost;
            } else {
                per_agent.push((r.agent.to_string(), r.cost));
            }
        }
        days.push(DayBucket {
            date: date.format("%Y-%m-%d").to_string(),
            cost,
            input,
            output,
            per_agent,
        });
    }

    // per-agent summaries
    let mut agent_order: Vec<&'static str> = vec![];
    for r in &records {
        if !agent_order.contains(&r.agent) {
            agent_order.push(r.agent);
        }
    }
    let mut agents = Vec::new();
    for agent in agent_order {
        let agent_records: Vec<UsageRecord> =
            records.iter().filter(|r| r.agent == agent).cloned().collect();
        let mut models: Vec<String> = Vec::new();
        let mut unpriced: Vec<String> = Vec::new();
        let mut last_activity = 0;
        for r in &agent_records {
            if !models.contains(&r.model) && models.len() < 8 {
                models.push(r.model.clone());
            }
            if r.input + r.output > 0
                && r.cost <= 0.0
                && pricing::lookup(&r.model).is_none()
                && !unpriced.contains(&r.model)
            {
                unpriced.push(r.model.clone());
            }
            if r.ts > last_activity {
                last_activity = r.ts;
            }
        }
        let day_costs: Vec<(String, f64)> =
            days.iter().map(|d| (d.date.clone(), d.per_agent.iter().find(|(a, _)| a == agent).map(|(_, c)| *c).unwrap_or(0.0))).collect();
        let mut agent_t = totals_for(&agent_records, |_| true);
        agent_t.sessions = distinct_sessions(&agent_records, |_| true);
        agents.push(AgentSummary {
            agent: agent.to_string(),
            status: String::new(),
            data_dir: sources::data_dir_for(agent),
            totals: agent_t,
            today_cost: agent_records.iter().filter(|r| is_today(r)).map(|r| r.cost).sum(),
            today_tokens: agent_records.iter().filter(|r| is_today(r)).map(|r| r.input + r.output).sum(),
            models,
            unpriced_models: unpriced,
            last_activity,
            day_costs,
        });
    }

    // recent sessions, grouped per (agent, session)
    let mut sessions: Vec<SessionInfo> = Vec::new();
    let mut by_key: HashMap<(&'static str, &str), (SessionInfo, usize)> = HashMap::new();
    for r in &records {
        if r.ts == 0 {
            continue;
        }
        let key = (r.agent, r.session_id.as_str());
        let entry = by_key.entry(key).or_insert_with(|| {
            (
                SessionInfo {
                    agent: r.agent.to_string(),
                    model: r.model.clone(),
                    ts: r.ts,
                    title: r.title.clone(),
                    cwd: r.cwd.clone(),
                    input: 0,
                    output: 0,
                    cache_creation: 0,
                    cache_read: 0,
                    cost: 0.0,
                    path: r.path.clone(),
                },
                0,
            )
        });
        let (info, _) = entry;
        info.input += r.input;
        info.output += r.output;
        info.cache_creation += r.cache_creation;
        info.cache_read += r.cache_read;
        info.cost += r.cost;
        if r.ts > info.ts {
            info.ts = r.ts;
        }
        if !info.title.is_empty() && info.title != r.title {
            // prefer first non-empty title
        }
        if info.title.is_empty() {
            info.title = r.title.clone();
        }
    }
    sessions.extend(by_key.into_values().map(|(s, _)| s));
    sessions.sort_by(|a, b| b.ts.cmp(&a.ts));
    sessions.truncate(100);

    let mut t_all = all_t.clone();
    t_all.sessions = sessions.len() as u64;

    RefreshResult {
        generated_at: now_secs(),
        today: today_t,
        week: week_t,
        month: month_t,
        all: t_all,
        agents,
        days,
        sessions,
        quotas,
        errors,
    }
}

// ---------------------------------------------------------------------------
// scan
// ---------------------------------------------------------------------------

fn scan_all(state: &AppState) -> RefreshResult {
    let mut errors: Vec<String> = Vec::new();

    if pricing::refresh_if_due() {
        state.cache.lock().unwrap().clear();
    }

    let mut cache = state.cache.lock().unwrap();
    let mut records: Vec<UsageRecord> = Vec::new();

    let claude = sources::claude::scan(&mut cache, &mut errors);
    records.extend(claude.records);

    let codex = sources::codex::scan(&mut cache, &mut errors);
    records.extend(codex.records);

    let opencode = sources::opencode::scan(&mut cache, &mut errors);
    records.extend(opencode.records);

    drop(cache);

    let quotas = {
        const QUOTA_REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);
        let mut cached = state.quotas.lock().unwrap();
        if let Some((at, quotas)) = cached.as_ref().filter(|(at, _)| at.elapsed() < QUOTA_REFRESH_INTERVAL) {
            let _ = at;
            quotas.clone()
        } else {
            let quotas = live::fetch_all();
            *cached = Some((Instant::now(), quotas.clone()));
            quotas
        }
    };
    build_result(records, quotas, errors)
}

// ---------------------------------------------------------------------------
// tray
// ---------------------------------------------------------------------------

fn tray_menu(app: &AppHandle, result: Option<&RefreshResult>) -> tauri::menu::Menu<tauri::Wry> {
    let label = match result {
        Some(r) => format!(
            "TokenTracker · Today ${:.2} · {} tok",
            r.today.cost,
            fmt_compact_tokens(r.today.input + r.today.output)
        ),
        None => "TokenTracker · scanning…".to_string(),
    };

    let title = MenuItem::with_id(app, "title", label, true, None::<&str>).unwrap();
    let refresh = MenuItem::with_id(app, "refresh", "Refresh now", true, None::<&str>).unwrap();
    let open = MenuItem::with_id(app, "open", "Open dashboard", true, None::<&str>).unwrap();
    let quit = MenuItem::with_id(app, "quit", "Exit", true, None::<&str>).unwrap();
    let sep = PredefinedMenuItem::separator(app).unwrap();

    let items: Vec<&dyn tauri::menu::IsMenuItem<tauri::Wry>> =
        vec![&title, &sep, &open, &refresh, &sep, &quit];
    Menu::with_items(app, &items).unwrap()
}

fn short_window_label(label: &str) -> String {
    let trimmed = label.trim();
    let short: String = match trimmed {
        "Rolling" => "R".into(),
        "Weekly" => "W".into(),
        "Monthly" => "M".into(),
        "5-Hour" => "5H".into(),
        "7-Day (All)" => "7D".into(),
        "7-Day (OAuth Apps)" => "7D-OA".into(),
        "7-Day (Sonnet)" => "7D-S".into(),
        "7-Day (Opus)" => "7D-O".into(),
        "7-Day (Co-work)" => "7D-C".into(),
        "Primary" => "P".into(),
        "Secondary" => "S".into(),
        other if !other.is_empty() => other.chars().take(6).collect(),
        _ => "Usage".into(),
    };
    short
}

fn update_tray(app: &AppHandle, result: &RefreshResult) {
    if let Some(tray) = app.tray_by_id("main") {
        let _ = tray.set_menu(Some(tray_menu(app, Some(result))));
        let mut tooltip = format!(
            "TokenTracker · Today ${:.2} · Week ${:.2}",
            result.today.cost, result.week.cost
        );
        for q in &result.quotas {
            if q.windows.is_empty() {
                continue;
            }
            let parts: Vec<String> = q
                .windows
                .iter()
                .take(3)
                .map(|w| format!("{} {:.0}%", short_window_label(&w.label), w.used_percent))
                .collect();
            tooltip.push_str(&format!("\n{}: {}", q.name, parts.join(" · ")));
        }
        // Windows tray tooltips are capped at 128 chars.
        if tooltip.chars().count() > 125 {
            tooltip = tooltip.chars().take(122).collect::<String>() + "...";
        }
        let _ = tray.set_tooltip(Some(tooltip.as_str()));
    }
}

/// Windows can fire the tray click event twice per physical click; debounce
/// so the show/hide toggle only runs once.
static LAST_TOGGLE_MS: AtomicU64 = AtomicU64::new(0);

fn toggle_window(app: &AppHandle) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let prev = LAST_TOGGLE_MS.swap(now, Ordering::Relaxed);
    if now.saturating_sub(prev) < 300 {
        return;
    }
    if let Some(window) = app.get_webview_window("main") {
        if window.is_visible().unwrap_or(false) {
            let _ = window.hide();
        } else {
            let _ = window.show();
            let _ = window.unminimize();
            let _ = window.set_focus();
            // refresh on open if stale
            let state = app.state::<Arc<AppState>>();
            if state.last_refresh.lock().unwrap().map(|i| i.elapsed() > Duration::from_secs(60)).unwrap_or(true) {
                let handle = app.clone();
                let state = state.inner().clone();
                std::thread::spawn(move || {
                    let _ = refresh_blocking(&handle, &state, false);
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// refresh
// ---------------------------------------------------------------------------

fn refresh_blocking(app: &AppHandle, state: &AppState, force: bool) -> Result<RefreshResult, String> {
    if state.busy.swap(true, Ordering::SeqCst) {
        // a refresh is already running; return the last good result
        return state
            .result
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| "refresh already in progress".to_string());
    }

    if !force {
        if let Some(cached) = state.result.lock().unwrap().clone() {
            let fresh = state
                .last_refresh
                .lock()
                .unwrap()
                .map(|i| i.elapsed() < Duration::from_secs(60))
                .unwrap_or(false);
            if fresh {
                state.busy.store(false, Ordering::SeqCst);
                return Ok(cached);
            }
        }
    }

    let result = scan_all(state);

    {
        let mut last = state.result.lock().unwrap();
        *last = Some(result.clone());
    }
    {
        let mut last = state.last_refresh.lock().unwrap();
        *last = Some(Instant::now());
    }
    state.busy.store(false, Ordering::SeqCst);

    update_tray(app, &result);
    let _ = app.emit("refreshed", ());

    Ok(result)
}

#[tauri::command]
async fn refresh(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    force: bool,
) -> Result<RefreshResult, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || refresh_blocking(&app, &state, force))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
fn open_data_dir(agent: String) -> Result<(), String> {
    let path = sources::data_dir_for(&agent);
    if path.is_empty() {
        return Err("no data dir for agent".into());
    }
    open::that(path).map_err(|e| e.to_string())
}

/// Store the user's opencode.ai console `_server` curl command so the live
/// quota card can fetch real subscription usage (wiscaksono/opencode-usage).
#[tauri::command]
fn save_opencode_curl(curl: String) -> Result<(), String> {
    let path = live::curl_config_path().ok_or("cannot resolve config path")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, curl.trim()).map_err(|e| e.to_string())
}

#[tauri::command]
fn clear_opencode_curl() -> Result<(), String> {
    if let Some(path) = live::curl_config_path() {
        if path.exists() {
            std::fs::remove_file(path).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[tauri::command]
fn quit_app(app: AppHandle) {
    app.exit(0);
}

// ---------------------------------------------------------------------------
// app
// ---------------------------------------------------------------------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(Arc::new(AppState::default()))
        .setup(|app| {
            pricing::initialize();
            let tray = TrayIconBuilder::with_id("main")
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("TokenTracker")
                .menu(&tray_menu(&app.handle(), None))
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "open" => toggle_window(app),
                    "refresh" => {
                        let handle = app.clone();
                        let state = handle.state::<Arc<AppState>>().inner().clone();
                        std::thread::spawn(move || {
                            let _ = refresh_blocking(&handle, &state, true);
                        });
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click { button: MouseButton::Left, .. } = event {
                        toggle_window(&tray.app_handle());
                    }
                })
                .build(app)?;
            let _ = tray;
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            refresh,
            open_data_dir,
            quit_app,
            save_opencode_curl,
            clear_opencode_curl
        ])
        .run(tauri::generate_context!())
        .expect("error while running TokenTracker");
}

#[cfg(test)]
mod real_data_tests {
    use super::*;

    #[test]
    #[ignore = "requires real agent data on this machine"]
    fn scan_real_user_data() {
        let state = AppState::default();
        let result = scan_all(&state);
        eprintln!("agents: {}", result.agents.len());
        for a in &result.agents {
            eprintln!(
                "{}: ${:.2} all-time | ${:.2} today | {} sessions | models {:?} | unpriced {:?}",
                a.agent, a.totals.cost, a.today_cost, a.totals.sessions, a.models, a.unpriced_models
            );
        }
        for q in &result.quotas {
            eprintln!("quota {}: status={} stats={:?}", q.id, q.status, q.stats);
        }
        eprintln!("day buckets: {:?}", result.days.iter().map(|d| (d.date.as_str(), d.cost)).collect::<Vec<_>>());
        eprintln!("quota providers: {:?}", result.quotas.iter().map(|q| (q.id.as_str(), q.status.as_str())).collect::<Vec<_>>());
        eprintln!("errors: {:?}", result.errors);
        assert!(!result.agents.is_empty(), "expected at least one agent with data");
        assert!(result.all.cost > 0.0, "expected non-zero lifetime cost");
    }
}
