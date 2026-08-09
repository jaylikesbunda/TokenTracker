use serde::Serialize;

#[derive(Clone, Debug)]
pub struct UsageRecord {
    pub agent: &'static str,
    pub model: String,
    pub ts: i64,
    pub input: u64,
    pub output: u64,
    pub cache_creation: u64,
    pub cache_read: u64,
    pub cost: f64,
    pub session_id: String,
    pub title: String,
    pub cwd: String,
    pub path: String,
}

#[derive(Serialize, Clone, Default)]
pub struct Totals {
    pub cost: f64,
    pub input: u64,
    pub output: u64,
    pub cache_creation: u64,
    pub cache_read: u64,
    pub sessions: u64,
}

#[derive(Serialize, Clone)]
pub struct AgentSummary {
    pub agent: String,
    pub status: String,
    pub data_dir: String,
    pub totals: Totals,
    pub today_cost: f64,
    pub today_tokens: u64,
    pub models: Vec<String>,
    pub unpriced_models: Vec<String>,
    pub last_activity: i64,
    pub day_costs: Vec<(String, f64)>,
}

#[derive(Serialize, Clone)]
pub struct DayBucket {
    pub date: String,
    pub cost: f64,
    pub input: u64,
    pub output: u64,
    pub per_agent: Vec<(String, f64)>,
}

#[derive(Serialize, Clone)]
pub struct SessionInfo {
    pub agent: String,
    pub model: String,
    pub ts: i64,
    pub title: String,
    pub cwd: String,
    pub input: u64,
    pub output: u64,
    pub cache_creation: u64,
    pub cache_read: u64,
    pub cost: f64,
    pub path: String,
}

#[derive(Serialize, Clone)]
pub struct QuotaWindow {
    pub label: String,
    pub used_percent: f64,
    pub resets_at: Option<i64>,
}

#[derive(Serialize, Clone)]
pub struct QuotaProvider {
    pub id: String,
    pub name: String,
    pub status: String,
    pub message: String,
    pub plan: Option<String>,
    pub windows: Vec<QuotaWindow>,
    pub credits: Option<String>,
    pub credits_unlimited: bool,
    /// Arbitrary stat rows (label, value) for local/derived providers.
    pub stats: Vec<(String, String)>,
}

#[derive(Serialize, Clone)]
pub struct RefreshResult {
    pub generated_at: i64,
    pub today: Totals,
    pub week: Totals,
    pub month: Totals,
    pub all: Totals,
    pub agents: Vec<AgentSummary>,
    pub days: Vec<DayBucket>,
    pub sessions: Vec<SessionInfo>,
    pub quotas: Vec<QuotaProvider>,
    pub errors: Vec<String>,
}
