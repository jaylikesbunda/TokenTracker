import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./styles.css";

interface Totals {
  cost: number;
  input: number;
  output: number;
  cache_creation: number;
  cache_read: number;
  sessions: number;
}

interface AgentSummary {
  agent: string;
  status: string;
  data_dir: string;
  totals: Totals;
  today_cost: number;
  today_tokens: number;
  models: string[];
  unpriced_models: string[];
  last_activity: number;
  day_costs: [string, number][];
}

interface DayBucket {
  date: string;
  cost: number;
  input: number;
  output: number;
  per_agent: [string, number][];
}

interface SessionInfo {
  agent: string;
  model: string;
  ts: number;
  title: string;
  cwd: string;
  input: number;
  output: number;
  cache_creation: number;
  cache_read: number;
  cost: number;
  path: string;
}

interface QuotaWindow {
  label: string;
  used_percent: number;
  resets_at: number | null;
}

interface QuotaProvider {
  id: string;
  name: string;
  status: string;
  message: string;
  plan: string | null;
  windows: QuotaWindow[];
  credits: string | null;
  credits_unlimited: boolean;
  stats: [string, string][];
}

interface RefreshResult {
  generated_at: number;
  today: Totals;
  week: Totals;
  month: Totals;
  all: Totals;
  agents: AgentSummary[];
  days: DayBucket[];
  sessions: SessionInfo[];
  quotas: QuotaProvider[];
  errors: string[];
}

const AGENT_COLORS: Record<string, string> = {
  "Claude Code": "#b08a5a",
  "Codex CLI": "#6a9b80",
  OpenCode: "#8f9bb8",
};

const AGENT_MONOGRAMS: Record<string, string> = {
  "Claude Code": "CC",
  "Codex CLI": "CX",
  OpenCode: "OC",
};

let result: RefreshResult | null = null;

const $ = <T extends HTMLElement = HTMLElement>(id: string): T =>
  document.getElementById(id) as T;

const esc = (s: string): string =>
  s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

function fmtMoney(v: number, digits = 2): string {
  if (v >= 1000) return `$${v.toLocaleString("en-US", { maximumFractionDigits: 0 })}`;
  return `$${v.toFixed(digits)}`;
}

function fmtTokens(n: number): string {
  if (n >= 1_000_000_000) return `${(n / 1_000_000_000).toFixed(1)}B`;
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${Math.round(n / 1000)}k`;
  return String(n);
}

function fmtDate(secs: number): string {
  const d = new Date(secs * 1000);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

function fmtCountdown(secs: number): string {
  if (secs <= 0) return "resetting…";
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  if (h > 0) return `resets in ${h}h ${m}m`;
  if (m > 0) return `resets in ${m}m`;
  return `resets in ${secs}s`;
}

// ---------------------------------------------------------------------------
// stats cards
// ---------------------------------------------------------------------------

function totalsCard(label: string, t: Totals): string {
  const total = t.input + t.output + t.cache_creation + t.cache_read;
  return `
    <div class="stat-card">
      <div class="stat-label">${label}</div>
      <div class="stat-cost">${fmtMoney(t.cost)}</div>
      <div class="stat-sub">${fmtTokens(total)} tokens · ${t.sessions} sessions</div>
      <div class="stat-bar">
        <div class="stat-bar-in" style="width:${Math.min(100, total / 1_000_000)}%"></div>
      </div>
    </div>`;
}

function renderStats() {
  $("stats-grid").innerHTML = [
    totalsCard("Today", result!.today),
    totalsCard("This week", result!.week),
    totalsCard("This month", result!.month),
    totalsCard("All time", result!.all),
  ].join("");
}

// ---------------------------------------------------------------------------
// live quotas
// ---------------------------------------------------------------------------

function quotaCard(q: QuotaProvider): string {
  const statusDot =
    q.status === "ok"
      ? "dot-ok"
      : q.status === "local"
        ? "dot-neutral"
        : q.status === "no-auth"
          ? "dot-warn"
          : "dot-err";
  const badge =
    q.status === "ok"
      ? `<span class="quota-status"><span class="dot ${statusDot}"></span>live</span>`
      : q.status === "local"
        ? `<span class="quota-status"><span class="dot ${statusDot}"></span>local estimate</span>`
        : q.status === "no-auth"
          ? `<span class="quota-status"><span class="dot ${statusDot}"></span>not signed in</span>`
          : `<span class="quota-status"><span class="dot ${statusDot}"></span>unavailable</span>`;

  const plan = q.plan ? `<span class="quota-plan">${esc(q.plan)}</span>` : "";
  const credits =
    q.credits !== null
      ? `<div class="quota-credits">${
          q.credits_unlimited ? "Unlimited credits" : `Credits: ${esc(q.credits)}`
        }</div>`
      : "";

  const windows =
    q.windows.length > 0
      ? q.windows
          .map((w) => {
            const pct = Math.max(0, Math.min(100, w.used_percent));
            const color = pct >= 90 ? "var(--danger)" : pct >= 70 ? "var(--warn)" : "var(--accent)";
            return `
            <div class="quota-window" data-resets="${w.resets_at ?? ""}">
              <div class="quota-window-head">
                <span>${esc(w.label)}</span>
                <span class="quota-pct">${pct.toFixed(0)}%</span>
              </div>
              <div class="quota-bar"><div class="quota-bar-fill" style="width:${pct}%;background:${color}"></div></div>
              <div class="quota-reset"></div>
            </div>`;
          })
          .join("")
      : q.message
        ? `<div class="quota-empty">${esc(q.message)}</div>`
        : `<div class="quota-empty">No usage windows reported.</div>`;

  const statsRows = q.stats.length
    ? `<div class="quota-stats">${q.stats
        .map(
          ([l, v]) =>
            `<div class="quota-stat-row"><span>${esc(l)}</span><span>${esc(v)}</span></div>`,
        )
        .join("")}</div>`
    : "";

  const setup =
    q.id === "opencode" && q.status !== "ok"
      ? `<details class="quota-setup">
          <summary>Connect subscription usage</summary>
          <p class="quota-setup-hint">Open the <strong>opencode.ai console</strong> in your browser, go to your workspace&rsquo;s <strong>Go page</strong> (the one showing Rolling / Weekly / Monthly usage), then DevTools &rarr; Network &rarr; find the <code>_server</code> request &rarr; right-click &rarr; Copy &rarr; Copy as cURL. Paste it below.</p>
          <textarea class="quota-setup-input" spellcheck="false" placeholder="curl 'https://opencode.ai/_server' -X POST ... --data-raw '{...}'"></textarea>
          <div class="quota-setup-actions">
            <button class="btn btn-sm" data-curl-save>Save session</button>
            <button class="btn btn-sm btn-ghost" data-curl-clear>Remove</button>
          </div>
        </details>`
      : "";

  return `
    <div class="quota-card" data-quota="${q.id}">
      <div class="quota-head">
        <span class="quota-name">${esc(q.name)}</span>
        <span>${badge}${plan}</span>
      </div>
      ${credits}
      ${windows}
      ${statsRows}
      ${setup}
    </div>`;
}

function renderQuotas() {
  const grid = $("quota-grid");
  if (!result!.quotas.length) {
    grid.innerHTML = `<div class="quota-empty-wide">Live quotas are off — sign in to an agent CLI to enable.</div>`;
    return;
  }
  grid.innerHTML = result!.quotas.map(quotaCard).join("");

  const refresh = () => refreshUI().catch(() => {});
  grid.querySelectorAll<HTMLElement>("[data-curl-save]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const card = btn.closest(".quota-card");
      const ta = card?.querySelector<HTMLTextAreaElement>(".quota-setup-input");
      btn.setAttribute("disabled", "disabled");
      try {
        await invoke("save_opencode_curl", { curl: ta?.value ?? "" });
        await refresh();
      } catch (e) {
        const msg = card?.querySelector(".quota-empty");
        if (msg) msg.textContent = `Save failed: ${String(e)}`;
      } finally {
        btn.removeAttribute("disabled");
      }
    });
  });
  grid.querySelectorAll<HTMLElement>("[data-curl-clear]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      btn.setAttribute("disabled", "disabled");
      try {
        await invoke("clear_opencode_curl");
        await refresh();
      } finally {
        btn.removeAttribute("disabled");
      }
    });
  });
}

let countdownTimer: number | null = null;

function tickCountdowns() {
  const now = Math.floor(Date.now() / 1000);
  document.querySelectorAll<HTMLElement>(".quota-window").forEach((el) => {
    const resets = el.dataset.resets;
    const out = el.querySelector(".quota-reset");
    if (!out) return;
    if (!resets) {
      out.textContent = "—";
      return;
    }
    const secs = parseInt(resets, 10) - now;
    out.textContent = fmtCountdown(secs);
  });
}

// ---------------------------------------------------------------------------
// chart
// ---------------------------------------------------------------------------

function renderChart() {
  const days = result!.days;
  if (!days.length) return;
  const max = Math.max(...days.map((d) => d.cost), 0.0001);
  const agents = [...new Set(days.flatMap((d) => d.per_agent.map(([a]) => a)))];

  const wrap = $("chart");
  const W = Math.max(260, wrap.clientWidth - 10);
  const H = 150;
  const PAD = 8;
  const bw = (W - PAD * 2) / days.length;
  const barW = Math.max(4, bw * 0.62);
  const step = Math.max(1, Math.ceil(54 / bw));

  let svg = `<svg viewBox="0 0 ${W} ${H}" class="chart-svg">`;
  for (let i = 0; i < days.length; i++) {
    const d = days[i];
    let y = H - PAD;
    let segs = "";
    for (const [agent, cost] of d.per_agent) {
      const h = Math.max(0, (cost / max) * (H - PAD * 2 - 12));
      segs += `<rect x="${PAD + i * bw + (bw - barW) / 2}" y="${y - h}" width="${barW}" height="${h}" rx="2" fill="${AGENT_COLORS[agent] ?? "#64748b"}">
        <title>${esc(agent)} · ${fmtMoney(cost)}</title></rect>`;
      y -= h;
    }
    svg += segs;
  }
  for (let i = 0; i < days.length; i += step) {
    if (days[i]) {
      const label = days[i].date.slice(5);
      svg += `<text x="${PAD + i * bw + bw / 2}" y="${H - 4}" text-anchor="middle" class="chart-label">${label}</text>`;
    }
  }
  svg += `</svg>`;

  const legend = agents
    .map((a) => `<span class="legend-item"><span class="legend-dot" style="background:${AGENT_COLORS[a] ?? "#64748b"}"></span>${esc(a)}</span>`)
    .join("");

  wrap.innerHTML = `<div class="chart-inner">${svg}</div><div class="chart-legend">${legend}</div>`;
}

// ---------------------------------------------------------------------------
// agent cards
// ---------------------------------------------------------------------------

function agentCard(a: AgentSummary): string {
  const color = AGENT_COLORS[a.agent] ?? "#64748b";
  const t = a.totals;
  const total = t.input + t.output + t.cache_creation + t.cache_read;
  const models = a.models.map((m) => `<span class="model-chip">${esc(m)}</span>`).join("");
  const unpriced =
    a.unpriced_models.length > 0
      ? `<div class="unpriced" title="No pricing data for these models — costs may be understated">⚠ unpriced: ${a.unpriced_models.map(esc).join(", ")}</div>`
      : "";
  const spark = a.day_costs
    .map(([, c]) => c)
    .join(",");
  const last = a.last_activity ? fmtDate(a.last_activity) : "never";

  return `
    <div class="agent-card">
      <div class="agent-head">
        <span class="agent-mono" style="background:${color}">${AGENT_MONOGRAMS[a.agent] ?? "?"}</span>
        <div class="agent-title">
          <span class="agent-name">${esc(a.agent)}</span>
          <span class="agent-last">last activity ${last}</span>
        </div>
        <button class="btn btn-ghost btn-sm" data-open-dir="${esc(a.agent)}">📂 data</button>
      </div>
      <div class="agent-stats">
        <div class="agent-stat"><span class="agent-stat-num">${fmtMoney(t.cost)}</span><span class="agent-stat-label">all time</span></div>
        <div class="agent-stat"><span class="agent-stat-num">${fmtMoney(a.today_cost)}</span><span class="agent-stat-label">today</span></div>
        <div class="agent-stat"><span class="agent-stat-num">${fmtTokens(total)}</span><span class="agent-stat-label">tokens</span></div>
        <div class="agent-stat"><span class="agent-stat-num">${t.sessions}</span><span class="agent-stat-label">sessions</span></div>
      </div>
      <div class="agent-models">${models}</div>
      <div class="agent-spark" data-spark="${spark}" data-max="${Math.max(...a.day_costs.map(([, c]) => c), 0.001)}" data-color="${color}"></div>
      ${unpriced}
    </div>`;
}

function renderAgentCards() {
  $("agent-grid").innerHTML = result!.agents.map(agentCard).join("");
  document.querySelectorAll<HTMLElement>("[data-open-dir]").forEach((btn) => {
    btn.addEventListener("click", () => {
      invoke("open_data_dir", { agent: btn.dataset.openDir });
    });
  });
  document.querySelectorAll<HTMLElement>(".agent-spark").forEach((el) => {
    const values = (el.dataset.spark || "").split(",").map(Number);
    const max = parseFloat(el.dataset.max || "1") || 1;
    const color = el.dataset.color || "#64748b";
    if (!values.length || values.every((v) => v === 0)) {
      el.innerHTML = `<span class="agent-spark-empty">no usage in last 14 days</span>`;
      return;
    }
    const bars = values
      .map((v) => {
        const h = Math.max(2, (v / max) * 26);
        return `<span class="spark-bar" style="height:${h}px;background:${color}" title="${fmtMoney(v)}"></span>`;
      })
      .join("");
    el.innerHTML = `<span class="spark-bars">${bars}</span>`;
  });
}

// ---------------------------------------------------------------------------
// sessions table
// ---------------------------------------------------------------------------

function renderSessions() {
  const rows = result!.sessions
    .slice(0, 30)
    .map((s) => {
      const color = AGENT_COLORS[s.agent] ?? "#64748b";
      const total = s.input + s.output + s.cache_creation + s.cache_read;
      const title = s.title || "(untitled)";
      const cwd = s.cwd || "";
      return `
      <div class="session-row" data-path="${esc(s.path)}">
        <span class="session-agent" style="color:${color}">${esc(s.agent)}</span>
        <span class="session-title" title="${esc(title)}">${esc(title)}</span>
        <span class="session-cwd" title="${esc(cwd)}">${esc(cwd)}</span>
        <span class="session-model">${esc(s.model)}</span>
        <span class="session-tokens">${fmtTokens(total)} tok</span>
        <span class="session-cost">${fmtMoney(s.cost)}</span>
        <span class="session-ts">${fmtDate(s.ts)}</span>
      </div>`;
    })
    .join("");
  $("sessions-table").innerHTML = `
    <div class="session-head">
      <span>Agent</span><span>Title</span><span>Folder</span><span>Model</span><span>Tokens</span><span>Cost</span><span>Time</span>
    </div>
    ${rows}`;
}

// ---------------------------------------------------------------------------
// errors + status
// ---------------------------------------------------------------------------

function renderErrors() {
  const el = $("errors");
  if (!result!.errors.length) {
    el.classList.add("hidden");
    el.innerHTML = "";
    return;
  }
  el.classList.remove("hidden");
  el.innerHTML = result!.errors.map((e) => `<div>${esc(e)}</div>`).join("");
}

function renderStatus() {
  const total = result!.all;
  const t = total.input + total.output + total.cache_creation + total.cache_read;
  $("status-left").textContent = `${result!.agents.length} agent source(s) · ${t.toLocaleString()} tokens all time`;
  $("status-right").textContent = `generated ${fmtDate(result!.generated_at)}`;
  $("last-updated").textContent = `updated ${fmtDate(result!.generated_at)}`;
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

async function refreshUI() {
  try {
    result = await invoke<RefreshResult>("refresh", { force: false });
  } catch (e) {
    $("loading").classList.add("hidden");
    $("dashboard").classList.remove("hidden");
    $("errors").classList.remove("hidden");
    $("errors").innerHTML = `<div>Failed to refresh: ${esc(String(e))}</div>`;
    return;
  }
  $("loading").classList.add("hidden");
  $("dashboard").classList.remove("hidden");
  renderStats();
  renderQuotas();
  renderChart();
  renderAgentCards();
  renderSessions();
  renderErrors();
  renderStatus();
  tickCountdowns();
  if (countdownTimer === null) {
    countdownTimer = window.setInterval(tickCountdowns, 1000);
  }
}

async function init() {
  $("btn-refresh").addEventListener("click", refreshUI);
  await listen("refreshed", () => {
    const el = $("last-updated");
    el.textContent = "updated just now";
  });
  await refreshUI();
  let resizeTimer: number | null = null;
  window.addEventListener("resize", () => {
    if (resizeTimer !== null) window.clearTimeout(resizeTimer);
    resizeTimer = window.setTimeout(() => {
      if (result) renderChart();
    }, 150);
  });
  window.setInterval(() => {
    invoke<RefreshResult>("refresh", { force: false }).then((r) => {
      result = r;
      renderStats();
      renderQuotas();
      renderChart();
      renderAgentCards();
      renderSessions();
      renderErrors();
      renderStatus();
      tickCountdowns();
    });
  }, 60_000);
}

init();
