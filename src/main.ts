import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./styles.css";
import { F, Sum, cacheHit, fetchFacts, sumRows, totalTokens, projectLabel } from "./data";
import {
  AGENT_COLORS,
  AGENT_MONOGRAMS,
  DEFAULT_PREFS,
  dayStart,
  esc,
  fmtDate,
  fmtMoney,
  fmtPct,
  fmtTokens,
  parts,
  prefs,
  updatePrefs,
} from "./prefs";
import { TabId, currentFacts, getTab, renderActive, setActiveTab, setFacts, toast } from "./explore";

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

let result: RefreshResult | null = null;

const $ = <T extends HTMLElement = HTMLElement>(id: string): T =>
  document.getElementById(id) as T;

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

function deltaHtml(cur: number, prev: number | null, what: string): string {
  if (prev === null || !(prev > 0)) return "";
  const pct = (cur - prev) / prev;
  const cls = Math.abs(pct) < 0.005 ? "flat" : pct > 0 ? "up" : "down";
  const arrow = cls === "flat" ? "•" : pct > 0 ? "▲" : "▼";
  return `<span class="delta ${cls}" title="vs ${what}">${arrow} ${Math.abs(pct * 100).toFixed(0)}%</span>`;
}

/** Sum of usage facts in [from, to) unix seconds, or null when facts aren't loaded. */
function factsSum(from: number, to: number): Sum | null {
  const f = currentFacts();
  if (!f) return null;
  return sumRows(f.rows.filter((r) => r[F.H] * 3600 >= from && r[F.H] * 3600 < to));
}

function periodDeltas(): (string | null)[] {
  const now = parts(Math.floor(Date.now() / 1000), false);
  const d = (off: number) => dayStart(now.y, now.m, now.d + off, false);
  const pairs: [[number, number], [number, number], string][] = [
    [[d(0), d(1)], [d(-1), d(0)], "yesterday"],
    [[d(-6), d(1)], [d(-13), d(-6)], "the previous 7 days"],
    [
      [dayStart(now.y, now.m, 1, false), d(1)],
      [dayStart(now.y, now.m - 1, 1, false), dayStart(now.y, now.m - 1, 1 + now.d, false)],
      "last month, same days",
    ],
  ];
  return pairs.map(([cur, prev, what]) => {
    const c = factsSum(cur[0], cur[1]);
    const p = factsSum(prev[0], prev[1]);
    return c && p ? deltaHtml(c.cost, p.cost, what) : null;
  });
}

function totalsCard(label: string, t: Totals, delta = ""): string {
  const total = t.input + t.output + t.cache_creation + t.cache_read;
  return `
    <div class="stat-card">
      <div class="stat-label">${label}</div>
      <div class="stat-cost">${fmtMoney(t.cost)}${delta}</div>
      <div class="stat-sub">${fmtTokens(total)} tokens · ${t.sessions} sessions</div>
      <div class="stat-bar">
        <div class="stat-bar-in" style="width:${Math.min(100, total / 1_000_000)}%"></div>
      </div>
    </div>`;
}

function renderStats() {
  const [dToday, dWeek, dMonth] = periodDeltas();
  $("stats-grid").innerHTML = [
    totalsCard("Today", result!.today, dToday ?? ""),
    totalsCard("Last 7 days", result!.week, dWeek ?? ""),
    totalsCard("This month", result!.month, dMonth ?? ""),
    totalsCard("All time", result!.all),
  ].join("");
}

function insightCard(label: string, value: string, sub = ""): string {
  return `<div class="kpi"><div class="kpi-label">${esc(label)}</div><div class="kpi-val">${esc(value)}</div>${sub ? `<div class="kpi-sub" title="${esc(sub)}">${esc(sub)}</div>` : ""}</div>`;
}

function renderInsights() {
  const f = currentFacts();
  const panel = $("insights-panel");
  if (!f || !f.rows.length) {
    panel.classList.add("hidden");
    return;
  }
  panel.classList.remove("hidden");
  const now = parts(Math.floor(Date.now() / 1000), false);
  const from30 = dayStart(now.y, now.m, now.d - 29, false);
  const tomorrow = dayStart(now.y, now.m, now.d + 1, false);
  const rows = f.rows.filter((r) => r[F.H] * 3600 >= from30 && r[F.H] * 3600 < tomorrow);
  const sum = sumRows(rows);
  const sessions = f.sessions.filter((s) => s.ts >= from30 && s.ts < tomorrow);

  const last7 = factsSum(dayStart(now.y, now.m, now.d - 6, false), tomorrow);
  const mtd = factsSum(dayStart(now.y, now.m, 1, false), tomorrow);
  const daysInMonth = new Date(now.y, now.m + 1, 0).getDate();
  const burn = last7 ? last7.cost / 7 : 0;
  const projected = mtd ? mtd.cost + burn * (daysInMonth - now.d) : 0;

  const top = (idx: number) => {
    const m = new Map<number, number>();
    for (const r of rows) m.set(r[idx], (m.get(r[idx]) ?? 0) + r[F.COST]);
    const best = [...m.entries()].sort((a, b) => b[1] - a[1])[0];
    return best ? { name: f.strings[best[0]], cost: best[1] } : null;
  };
  const topModel = top(F.M);
  const topProject = top(F.C);
  const priciest = sessions.reduce<(typeof sessions)[number] | null>((a, s) => (!a || s.cost > a.cost ? s : a), null);
  const share = (c: number) => (sum.cost > 0 ? fmtPct(c / sum.cost) : "–");

  $("insights").innerHTML = [
    insightCard("Cache hit rate", fmtPct(cacheHit(sum)), "share of prompt tokens read from cache"),
    insightCard("Saved by caching", fmtMoney(sum.saved), "vs paying full input price"),
    insightCard("Daily burn (7d avg)", fmtMoney(burn), `${fmtMoney(burn * 30)} per 30 days`),
    insightCard("Projected this month", fmtMoney(projected), `${fmtMoney(mtd?.cost ?? 0)} so far`),
    insightCard("Top model", topModel?.name ?? "–", topModel ? `${fmtMoney(topModel.cost)} · ${share(topModel.cost)} of spend` : ""),
    insightCard("Top project", topProject ? projectLabel(topProject.name) : "–", topProject ? `${fmtMoney(topProject.cost)} · ${share(topProject.cost)} of spend` : ""),
    insightCard("Priciest session", priciest ? fmtMoney(priciest.cost) : "–", priciest ? priciest.title || "(untitled)" : ""),
    insightCard("Avg cost / session", sessions.length ? fmtMoney(sum.cost / sessions.length) : "–", `${sessions.length} sessions · ${fmtTokens(totalTokens(sum) / Math.max(1, sessions.length))} tok avg`),
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
  renderOverview();
  if (countdownTimer === null) {
    countdownTimer = window.setInterval(tickCountdowns, 1000);
  }
  // usage facts feed the deltas, insights and the other tabs
  try {
    setFacts(await fetchFacts());
    renderStats();
    renderInsights();
  } catch (e) {
    toast(`Could not load usage details: ${String(e)}`);
  }
}

function renderOverview() {
  if (!result) return;
  renderStats();
  renderInsights();
  renderQuotas();
  renderChart();
  renderAgentCards();
  renderSessions();
  renderErrors();
  renderStatus();
  tickCountdowns();
}

// ---------------------------------------------------------------------------
// settings popover
// ---------------------------------------------------------------------------

const SYMBOLS = ["$", "€", "£", "¥", "₹", "kr", "CHF", "R$"];

function renderSettings() {
  const opt = (v: string, l: string, cur: string) => `<option value="${esc(v)}"${v === cur ? " selected" : ""}>${esc(l)}</option>`;
  const syms = SYMBOLS.includes(prefs.symbol) ? SYMBOLS : [...SYMBOLS, prefs.symbol];
  $("settings-pop").innerHTML = `
    <div class="pop-row"><label for="pf-sym">Currency symbol</label>
      <select id="pf-sym">${syms.map((s) => opt(s, s, prefs.symbol)).join("")}</select></div>
    <div class="pop-row"><label for="pf-rate">Rate (1 USD = )</label>
      <input id="pf-rate" type="number" step="any" min="0.0001" value="${prefs.rate}"></div>
    <p class="pop-note">Costs are computed in USD; the rate is a manual multiplier for display and doesn't affect exports.</p>
    <div class="pop-row"><label for="pf-tok">Token numbers</label>
      <select id="pf-tok">${opt("compact", "Compact (1.2M)", prefs.tokens)}${opt("full", "Full (1,234,567)", prefs.tokens)}</select></div>
    <div class="pop-row"><label for="pf-tz">Time zone</label>
      <select id="pf-tz">${opt("local", "Local time", prefs.utc ? "utc" : "local")}${opt("utc", "UTC", prefs.utc ? "utc" : "local")}</select></div>
    <div class="pop-row"><label for="pf-ws">Week starts on</label>
      <select id="pf-ws">${opt("1", "Monday", String(prefs.weekStart))}${opt("0", "Sunday", String(prefs.weekStart))}</select></div>
    <div class="pop-row"><button class="btn btn-ghost btn-sm" id="pf-reset">Reset to defaults</button></div>`;
}

function applyPrefsChange() {
  renderOverview();
  renderActive();
}

function wireSettings() {
  const pop = $("settings-pop");
  $("btn-settings").addEventListener("click", (e) => {
    e.stopPropagation();
    if (pop.classList.contains("hidden")) {
      renderSettings();
      pop.classList.remove("hidden");
    } else {
      pop.classList.add("hidden");
    }
  });
  document.addEventListener("click", (e) => {
    if (!pop.classList.contains("hidden") && !pop.contains(e.target as Node)) pop.classList.add("hidden");
  });
  pop.addEventListener("change", (e) => {
    const t = e.target as HTMLInputElement | HTMLSelectElement;
    switch (t.id) {
      case "pf-sym":
        updatePrefs({ symbol: t.value });
        break;
      case "pf-rate": {
        const v = parseFloat(t.value);
        updatePrefs({ rate: isFinite(v) && v > 0 ? v : 1 });
        t.value = String(prefs.rate);
        break;
      }
      case "pf-tok":
        updatePrefs({ tokens: t.value === "full" ? "full" : "compact" });
        break;
      case "pf-tz":
        updatePrefs({ utc: t.value === "utc" });
        break;
      case "pf-ws":
        updatePrefs({ weekStart: t.value === "0" ? 0 : 1 });
        break;
      default:
        return;
    }
    applyPrefsChange();
  });
  pop.addEventListener("click", (e) => {
    if ((e.target as HTMLElement).id === "pf-reset") {
      updatePrefs(DEFAULT_PREFS);
      renderSettings();
      applyPrefsChange();
    }
  });
}

// ---------------------------------------------------------------------------
// tabs
// ---------------------------------------------------------------------------

function wireTabs() {
  $("tabs").addEventListener("click", (e) => {
    const b = (e.target as HTMLElement).closest<HTMLElement>("[data-tab]");
    if (b) setActiveTab(b.dataset.tab as TabId);
  });
  $("btn-all-sessions").addEventListener("click", () => setActiveTab("sessions"));
}

async function init() {
  wireTabs();
  wireSettings();
  $("btn-refresh").addEventListener("click", () => {
    refreshUI().catch(() => {});
  });
  await listen("refreshed", () => {
    const el = $("last-updated");
    el.textContent = "updated just now";
  });
  await refreshUI();
  let resizeTimer: number | null = null;
  window.addEventListener("resize", () => {
    if (resizeTimer !== null) window.clearTimeout(resizeTimer);
    resizeTimer = window.setTimeout(() => {
      if (!result) return;
      if (getTab() === "overview") renderChart();
      else renderActive();
    }, 150);
  });
  window.setInterval(() => {
    refreshUI().catch(() => {});
  }, 60_000);
}

init();
