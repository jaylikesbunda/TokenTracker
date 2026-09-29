// Explore / Sessions / Models tabs: shared filter bar, chart, sortable and
// customisable tables, session drawer, saved views and exports.

import { invoke } from "@tauri-apps/api/core";
import {
  BucketId,
  Dim,
  F,
  Facts,
  GroupRow,
  Metric,
  METRIC_LABELS,
  RangeId,
  SessionRow,
  Series,
  Stack,
  View,
  bucketDays,
  buildSeries,
  cacheHit,
  DEFAULT_VIEW,
  filterOptions,
  filterRows,
  filterSessions,
  groupBy,
  heatmap,
  projectLabel,
  rangeBounds,
  sanitizeView,
  sumRows,
  totalTokens,
} from "./data";
import {
  AGENT_COLORS,
  OTHER_COLOR,
  PALETTE,
  esc,
  fmtDate,
  fmtDuration,
  fmtMoney,
  fmtPct,
  fmtTokens,
  loadJSON,
  p2,
  parts,
  prefs,
  saveJSON,
  ymdStr,
} from "./prefs";

export type TabId = "overview" | "explore" | "sessions" | "models";

let facts: Facts | null = null;
let activeTab: TabId = "overview";
let view: View = sanitizeView(loadJSON<unknown>("tt.view", {}));
let groupDim: Dim = (["agent", "model", "project"] as Dim[]).includes(loadJSON<Dim>("tt.groupdim", "agent"))
  ? loadJSON<Dim>("tt.groupdim", "agent")
  : "agent";
let search = "";
let page = 0;
const PAGE_SIZE = 50;

interface SavedView {
  name: string;
  view: View;
}
let savedViews: SavedView[] = (() => {
  const raw = loadJSON<unknown>("tt.views", []);
  if (!Array.isArray(raw)) return [];
  return raw
    .filter((v): v is { name: string; view: unknown } => !!v && typeof v.name === "string")
    .slice(0, 30)
    .map((v) => ({ name: v.name.slice(0, 40), view: sanitizeView(v.view) }));
})();

const $ = <T extends HTMLElement = HTMLElement>(id: string): T => document.getElementById(id) as T;

// ---------------------------------------------------------------------------
// toast + exports
// ---------------------------------------------------------------------------

let toastTimer: number | null = null;
export function toast(msg: string): void {
  const el = $("toast");
  el.textContent = msg;
  el.classList.remove("hidden");
  if (toastTimer !== null) window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => el.classList.add("hidden"), 4500);
}

interface ExportData {
  headers: string[];
  rows: (string | number)[][];
}
const exporters = new Map<string, () => ExportData>();

function csvCell(v: string | number): string {
  let s = typeof v === "number" ? (Number.isFinite(v) ? String(v) : "") : v;
  // neutralise spreadsheet formula injection in text cells
  if (typeof v === "string" && /^[=+\-@\t\r]/.test(s)) s = `'${s}`;
  if (/[",\n\r]/.test(s)) s = `"${s.replace(/"/g, '""')}"`;
  return s;
}

function stamp(): string {
  const d = new Date();
  return `${d.getFullYear()}${p2(d.getMonth() + 1)}${p2(d.getDate())}-${p2(d.getHours())}${p2(d.getMinutes())}${p2(d.getSeconds())}`;
}

async function doExport(kind: string, name: string): Promise<void> {
  const make = exporters.get(name);
  if (!make) return;
  const { headers, rows } = make();
  try {
    if (kind === "copy") {
      const tsv = [headers, ...rows]
        .map((r) => r.map((c) => String(c).replace(/[\t\r\n]+/g, " ")).join("\t"))
        .join("\n");
      await navigator.clipboard.writeText(tsv);
      toast(`Copied ${rows.length} rows to clipboard`);
      return;
    }
    let content: string;
    let ext: string;
    if (kind === "json") {
      ext = "json";
      content = JSON.stringify(
        rows.map((r) => Object.fromEntries(headers.map((h, i) => [h, r[i]]))),
        null,
        2,
      );
    } else {
      ext = "csv";
      content = [headers, ...rows].map((r) => r.map(csvCell).join(",")).join("\r\n") + "\r\n";
    }
    const path = await invoke<string>("save_export", { filename: `tokentracker-${name}-${stamp()}.${ext}`, content });
    toast(`Saved ${path}`);
  } catch (e) {
    toast(`Export failed: ${String(e)}`);
  }
}

function expButtons(name: string): string {
  return `<span class="exp">
    <button class="btn btn-ghost btn-sm" data-exp-kind="csv" data-exp-name="${name}">CSV</button>
    <button class="btn btn-ghost btn-sm" data-exp-kind="json" data-exp-name="${name}">JSON</button>
    <button class="btn btn-ghost btn-sm" data-exp-kind="copy" data-exp-name="${name}">Copy</button>
  </span>`;
}

// ---------------------------------------------------------------------------
// generic customisable table
// ---------------------------------------------------------------------------

interface Col<T> {
  id: string;
  label: string;
  num?: boolean;
  on?: boolean; // default visibility (default true)
  exp?: string; // export header
  get: (r: T) => number | string;
  fmt?: (r: T) => string;
  color?: (r: T) => string | undefined;
}

interface TState {
  order: string[];
  hidden: string[];
  sort: string;
  dir: 1 | -1;
}

const tstates: Record<string, TState> = (() => {
  const raw = loadJSON<unknown>("tt.tables", {});
  return raw && typeof raw === "object" ? (raw as Record<string, TState>) : {};
})();

function tstate<T>(id: string, cols: Col<T>[], defSort: string): TState {
  const ids = cols.map((c) => c.id);
  let s = tstates[id];
  if (!s || typeof s !== "object") {
    s = { order: ids, hidden: cols.filter((c) => c.on === false).map((c) => c.id), sort: defSort, dir: -1 };
  }
  const order = Array.isArray(s.order) ? s.order.filter((i, n, a) => ids.includes(i) && a.indexOf(i) === n) : [];
  s.order = [...order, ...ids.filter((i) => !order.includes(i))];
  s.hidden = Array.isArray(s.hidden) ? s.hidden.filter((i) => ids.includes(i)) : [];
  if (!ids.includes(s.sort)) s.sort = defSort;
  s.dir = s.dir === 1 ? 1 : -1;
  tstates[id] = s;
  return s;
}

function visibleCols<T>(cols: Col<T>[], s: TState): Col<T>[] {
  const out = s.order
    .filter((i) => !s.hidden.includes(i))
    .map((i) => cols.find((c) => c.id === i)!)
    .filter(Boolean);
  return out.length ? out : cols.slice(0, 1);
}

function sortRows<T>(rows: T[], cols: Col<T>[], s: TState): T[] {
  const col = cols.find((c) => c.id === s.sort) ?? cols[0];
  return [...rows].sort((a, b) => {
    const x = col.get(a);
    const y = col.get(b);
    const c = typeof x === "number" && typeof y === "number" ? x - y : String(x).localeCompare(String(y));
    return c * s.dir;
  });
}

interface TableOpts<T> {
  defSort: string;
  rerender: () => void;
  onRow?: (r: T) => void;
  onSort?: () => void;
  pageStart?: number;
  pageSize?: number;
  emptyText?: string;
}

/** Renders toolbar + table into `el`; returns the total number of (sorted) rows. */
function renderTable<T>(el: HTMLElement, id: string, cols: Col<T>[], rows: T[], o: TableOpts<T>): number {
  const s = tstate(id, cols, o.defSort);
  const vis = visibleCols(cols, s);
  const sorted = sortRows(rows, cols, s);
  const slice = o.pageSize ? sorted.slice(o.pageStart ?? 0, (o.pageStart ?? 0) + o.pageSize) : sorted;

  exporters.set(id, () => ({
    headers: vis.map((c) => c.exp ?? c.label),
    rows: sorted.map((r) => vis.map((c) => c.get(r))),
  }));

  const wasOpen = el.querySelector("details.colmenu")?.hasAttribute("open") ?? false;

  const menu = `<details class="colmenu"${wasOpen ? " open" : ""}>
    <summary class="btn btn-ghost btn-sm">Columns</summary>
    <div class="colmenu-list">${s.order
      .map((cid, i) => {
        const c = cols.find((x) => x.id === cid)!;
        return `<div class="colmenu-row">
          <label><input type="checkbox" data-cv="${esc(cid)}"${s.hidden.includes(cid) ? "" : " checked"}> ${esc(c.label)}</label>
          <span><button class="btn btn-ghost btn-sm" data-mv="up" data-c="${esc(cid)}"${i === 0 ? " disabled" : ""}>▲</button><button class="btn btn-ghost btn-sm" data-mv="down" data-c="${esc(cid)}"${i === s.order.length - 1 ? " disabled" : ""}>▼</button></span>
        </div>`;
      })
      .join("")}
      <button class="btn btn-ghost btn-sm" data-cols-reset>Reset columns</button>
    </div>
  </details>`;

  const head = vis
    .map((c) => {
      const sorted = c.id === s.sort;
      return `<th class="${c.num ? "num" : ""}${sorted ? " sorted" : ""}" data-sort="${esc(c.id)}">${esc(c.label)}${sorted ? (s.dir === 1 ? " ▲" : " ▼") : ""}</th>`;
    })
    .join("");

  const body = slice.length
    ? slice
        .map(
          (r, i) =>
            `<tr data-i="${i}"${o.onRow ? ' class="clickable"' : ""}>${vis
              .map((c) => {
                const text = c.fmt ? c.fmt(r) : String(c.get(r));
                const color = c.color?.(r);
                return `<td class="${c.num ? "num" : ""}"${color ? ` style="color:${color}"` : ""} title="${esc(text)}">${esc(text)}</td>`;
              })
              .join("")}</tr>`,
        )
        .join("")
    : `<tr><td colspan="${vis.length}" class="empty">${esc(o.emptyText ?? "No data for the current filters.")}</td></tr>`;

  el.innerHTML = `
    <div class="table-tools">${menu}${expButtons(id)}</div>
    <div class="table-wrap"><table class="dt"><thead><tr>${head}</tr></thead><tbody>${body}</tbody></table></div>`;

  const save = () => saveJSON("tt.tables", tstates);
  el.querySelectorAll<HTMLElement>("th[data-sort]").forEach((th) =>
    th.addEventListener("click", () => {
      const cid = th.dataset.sort!;
      if (s.sort === cid) s.dir = (s.dir * -1) as 1 | -1;
      else {
        s.sort = cid;
        s.dir = cols.find((c) => c.id === cid)?.num ? -1 : 1;
      }
      save();
      o.onSort?.();
      o.rerender();
    }),
  );
  el.querySelectorAll<HTMLInputElement>("input[data-cv]").forEach((cb) =>
    cb.addEventListener("change", () => {
      const cid = cb.dataset.cv!;
      s.hidden = cb.checked ? s.hidden.filter((h) => h !== cid) : [...s.hidden, cid];
      if (s.hidden.length >= cols.length) s.hidden = s.hidden.slice(1);
      save();
      o.rerender();
    }),
  );
  el.querySelectorAll<HTMLElement>("button[data-mv]").forEach((b) =>
    b.addEventListener("click", () => {
      const i = s.order.indexOf(b.dataset.c!);
      const j = b.dataset.mv === "up" ? i - 1 : i + 1;
      if (i < 0 || j < 0 || j >= s.order.length) return;
      [s.order[i], s.order[j]] = [s.order[j], s.order[i]];
      save();
      o.rerender();
    }),
  );
  el.querySelector("[data-cols-reset]")?.addEventListener("click", () => {
    delete tstates[id];
    save();
    o.rerender();
  });
  if (o.onRow) {
    el.querySelectorAll<HTMLElement>("tbody tr[data-i]").forEach((tr) =>
      tr.addEventListener("click", () => o.onRow!(slice[Number(tr.dataset.i)])),
    );
  }
  return sorted.length;
}

// ---------------------------------------------------------------------------
// formatting helpers
// ---------------------------------------------------------------------------

const fmtMetric = (m: Metric, v: number, precise = false): string =>
  m === "cost" ? fmtMoney(v, precise && v < 1 ? 3 : 2) : fmtTokens(v);

function perMillion(cost: number, tokens: number): number {
  return tokens > 0 ? (cost / tokens) * 1_000_000 : 0;
}

function deltaChip(cur: number, prev: number): string {
  if (!(prev > 0)) return "";
  const pct = (cur - prev) / prev;
  if (!isFinite(pct)) return "";
  const cls = Math.abs(pct) < 0.005 ? "flat" : pct > 0 ? "up" : "down";
  const arrow = cls === "flat" ? "•" : pct > 0 ? "▲" : "▼";
  return `<span class="delta ${cls}" title="vs previous period of equal length">${arrow} ${Math.abs(pct * 100).toFixed(0)}%</span>`;
}

const agentColor = (a: string): string => AGENT_COLORS[a] ?? "#64748b";

function seriesColors(s: Series, stack: Stack): string[] {
  return s.names.map((n, i) => (n === "Other" ? OTHER_COLOR : stack === "agent" && AGENT_COLORS[n] ? AGENT_COLORS[n] : PALETTE[i % PALETTE.length]));
}

// ---------------------------------------------------------------------------
// filter bar
// ---------------------------------------------------------------------------

const RANGE_LABELS: [RangeId, string][] = [
  ["today", "Today"],
  ["7d", "Last 7 days"],
  ["30d", "Last 30 days"],
  ["90d", "Last 90 days"],
  ["month", "This month"],
  ["all", "All time"],
  ["custom", "Custom…"],
];

type MsDim = "agents" | "models" | "projects";
const MS_LABEL: Record<MsDim, string> = { agents: "Agents", models: "Models", projects: "Projects" };
let msOptions: Record<MsDim, string[]> = { agents: [], models: [], projects: [] };

function msSummary(dim: MsDim): string {
  const sel = view[dim];
  if (!sel.length) return `${MS_LABEL[dim]}: all`;
  if (sel.length === 1) return `${MS_LABEL[dim]}: ${dim === "projects" ? projectLabel(sel[0]) : sel[0]}`;
  return `${MS_LABEL[dim]}: ${sel.length} selected`;
}

function msHtml(dim: MsDim): string {
  const opts = msOptions[dim].slice(0, 300);
  const search =
    opts.length > 10 ? `<input class="ms-search" type="search" placeholder="Filter…" data-ms-search="${dim}">` : "";
  return `<details class="ms" data-dim="${dim}">
    <summary class="btn btn-ghost btn-sm" data-ms-sum="${dim}">${esc(msSummary(dim))}</summary>
    <div class="ms-pop">
      ${search}
      <div class="ms-list">${opts
        .map(
          (o, i) =>
            `<label data-lbl="${esc((dim === "projects" ? o + " " + projectLabel(o) : o).toLowerCase())}"><input type="checkbox" data-i="${i}"${view[dim].includes(o) ? " checked" : ""}> <span>${esc(dim === "projects" ? projectLabel(o) : o)}</span></label>`,
        )
        .join("")}</div>
      <button class="btn btn-ghost btn-sm" data-ms-clear="${dim}">Clear</button>
    </div>
  </details>`;
}

function persistView(): void {
  saveJSON("tt.view", view);
}

function buildFilterBar(): void {
  if (!facts) return;
  const o = filterOptions(facts);
  msOptions = { agents: o.agents, models: o.models, projects: o.projects };
  // drop selections that no longer exist so a stale filter can't hide everything
  view.agents = view.agents.filter((v) => o.agents.includes(v));
  view.models = view.models.filter((v) => o.models.includes(v));
  view.projects = view.projects.filter((v) => o.projects.includes(v));

  $("filterbar").innerHTML = `
    <select id="fb-range" title="Date range">${RANGE_LABELS.map(([v, l]) => `<option value="${v}">${l}</option>`).join("")}</select>
    <span id="fb-custom" class="fb-custom"><input type="date" id="fb-from"><span>→</span><input type="date" id="fb-to"></span>
    ${msHtml("agents")}${msHtml("models")}${msHtml("projects")}
    <button class="btn btn-ghost btn-sm" id="fb-reset">Reset</button>
    <span class="spacer"></span>
    <select id="fb-saved" title="Saved views"><option value="">Saved views…</option>${savedViews
      .map((v, i) => `<option value="${i}">${esc(v.name)}</option>`)
      .join("")}</select>
    <button class="btn btn-ghost btn-sm" id="fb-del" title="Delete selected saved view">✕</button>
    <span id="fb-savebox" class="hidden"><input id="fb-name" type="text" maxlength="40" placeholder="View name"><button class="btn btn-sm" id="fb-ok">Save</button></span>
    <button class="btn btn-ghost btn-sm" id="fb-save">Save view</button>`;
  syncFilterInputs();
}

function syncFilterInputs(): void {
  const range = $<HTMLSelectElement>("fb-range");
  if (!range) return;
  range.value = view.range;
  $<HTMLInputElement>("fb-from").value = view.from;
  $<HTMLInputElement>("fb-to").value = view.to;
  $("fb-custom").classList.toggle("hidden", view.range !== "custom");
  (["agents", "models", "projects"] as MsDim[]).forEach((d) => {
    const sum = document.querySelector(`[data-ms-sum="${d}"]`);
    if (sum) sum.textContent = msSummary(d);
  });
}

function onViewChanged(): void {
  persistView();
  page = 0;
  syncFilterInputs();
  renderActive();
}

function wireFilterBar(): void {
  const bar = $("filterbar");
  bar.addEventListener("change", (e) => {
    const t = e.target as HTMLElement;
    if (t.id === "fb-range") {
      const next = (t as HTMLSelectElement).value as RangeId;
      if (next === "custom" && facts && (!view.from || !view.to)) {
        const [f, to] = rangeBounds(facts, view);
        const a = parts(f);
        const b = parts(to - 1);
        view.from = ymdStr(a.y, a.m, a.d);
        view.to = ymdStr(b.y, b.m, b.d);
      }
      view.range = next;
      onViewChanged();
    } else if (t.id === "fb-from" || t.id === "fb-to") {
      view[t.id === "fb-from" ? "from" : "to"] = (t as HTMLInputElement).value;
      view.range = "custom";
      onViewChanged();
    } else if (t.id === "fb-saved") {
      const i = Number((t as HTMLSelectElement).value);
      if (Number.isInteger(i) && savedViews[i]) {
        view = sanitizeView(savedViews[i].view);
        persistView();
        buildFilterBar();
        syncExploreControls();
        page = 0;
        renderActive();
        const sel = $<HTMLSelectElement>("fb-saved");
        sel.value = String(i);
      }
    } else if (t instanceof HTMLInputElement && t.type === "checkbox") {
      const box = t.closest<HTMLElement>(".ms");
      const dim = box?.dataset.dim as MsDim | undefined;
      if (!dim) return;
      const value = msOptions[dim].slice(0, 300)[Number(t.dataset.i)];
      if (value === undefined) return;
      view[dim] = t.checked ? [...view[dim], value] : view[dim].filter((v) => v !== value);
      onViewChanged();
    }
  });
  bar.addEventListener("input", (e) => {
    const t = e.target as HTMLInputElement;
    if (t.dataset.msSearch) {
      const q = t.value.trim().toLowerCase();
      t.closest(".ms-pop")?.querySelectorAll<HTMLElement>(".ms-list label").forEach((l) => {
        l.classList.toggle("hidden", !!q && !(l.dataset.lbl ?? "").includes(q));
      });
    }
  });
  bar.addEventListener("click", (e) => {
    const t = (e.target as HTMLElement).closest<HTMLElement>("button");
    if (!t) return;
    if (t.dataset.msClear) {
      view[t.dataset.msClear as MsDim] = [];
      bar.querySelectorAll<HTMLInputElement>(`.ms[data-dim="${t.dataset.msClear}"] input[type=checkbox]`).forEach((c) => (c.checked = false));
      onViewChanged();
    } else if (t.id === "fb-reset") {
      view = { ...DEFAULT_VIEW };
      persistView();
      buildFilterBar();
      syncExploreControls();
      page = 0;
      renderActive();
    } else if (t.id === "fb-save") {
      $("fb-savebox").classList.toggle("hidden");
      $<HTMLInputElement>("fb-name").focus();
    } else if (t.id === "fb-ok") {
      commitSave();
    } else if (t.id === "fb-del") {
      const i = Number($<HTMLSelectElement>("fb-saved").value);
      if ($<HTMLSelectElement>("fb-saved").value !== "" && savedViews[i]) {
        const name = savedViews[i].name;
        savedViews.splice(i, 1);
        saveJSON("tt.views", savedViews);
        buildFilterBar();
        toast(`Deleted view “${name}”`);
      }
    }
  });
  bar.addEventListener("keydown", (e) => {
    if ((e.target as HTMLElement).id === "fb-name" && e.key === "Enter") commitSave();
  });
  document.addEventListener("click", (e) => {
    document.querySelectorAll<HTMLDetailsElement>("details.ms[open], details.colmenu[open]").forEach((d) => {
      if (!d.contains(e.target as Node)) d.removeAttribute("open");
    });
  });
}

function commitSave(): void {
  const input = $<HTMLInputElement>("fb-name");
  const name = input.value.trim().slice(0, 40);
  if (!name) return;
  const snapshot = sanitizeView(view);
  const i = savedViews.findIndex((v) => v.name === name);
  if (i >= 0) savedViews[i] = { name, view: snapshot };
  else if (savedViews.length < 30) savedViews.push({ name, view: snapshot });
  else return toast("Saved-view limit reached (30)");
  saveJSON("tt.views", savedViews);
  buildFilterBar();
  const idx = savedViews.findIndex((v) => v.name === name);
  $<HTMLSelectElement>("fb-saved").value = String(idx);
  toast(`Saved view “${name}”`);
}

// ---------------------------------------------------------------------------
// chart
// ---------------------------------------------------------------------------

function niceMax(v: number): number {
  if (!(v > 0)) return 1;
  const exp = Math.pow(10, Math.floor(Math.log10(v)));
  const f = v / exp;
  const n = f <= 1 ? 1 : f <= 2 ? 2 : f <= 2.5 ? 2.5 : f <= 5 ? 5 : 10;
  return n * exp;
}

function bucketLabel(key: string, bucket: string, narrow: boolean): string {
  if (bucket === "hour") return narrow ? key.slice(11) : key.slice(5, 16);
  if (bucket === "month") return key;
  return key.slice(5);
}

function drawChart(el: HTMLElement, s: Series, v: View, onZoom: (key: string) => void): void {
  const n = s.buckets.length;
  const W = Math.max(320, el.clientWidth);
  const H = 250;
  const padL = 54;
  const padR = 10;
  const padT = 10;
  const padB = 26;
  const plotW = W - padL - padR;
  const plotH = H - padT - padB;
  const bw = plotW / Math.max(1, n);
  const colors = seriesColors(s, v.stack);

  const stacked = v.chart !== "line";
  let rawMax = 0;
  if (stacked) rawMax = Math.max(...s.totals, 0);
  else for (const row of s.values) rawMax = Math.max(rawMax, ...row);
  const max = niceMax(rawMax);
  const y = (val: number) => padT + plotH - (val / max) * plotH;
  const cx = (i: number) => padL + (i + 0.5) * bw;

  let svg = `<svg viewBox="0 0 ${W} ${H}" width="${W}" height="${H}" class="chart-svg">`;
  for (let t = 0; t <= 4; t++) {
    const val = (max * t) / 4;
    const yy = y(val);
    const label = v.metric === "cost" ? fmtMoney(val, max < 5 ? 2 : 0) : fmtTokens(val);
    svg += `<line x1="${padL}" x2="${W - padR}" y1="${yy}" y2="${yy}" class="grid"/>`;
    svg += `<text x="${padL - 6}" y="${yy + 3.5}" text-anchor="end" class="chart-label">${esc(label)}</text>`;
  }
  const step = Math.max(1, Math.ceil(64 / bw));
  for (let i = 0; i < n; i += step) {
    svg += `<text x="${cx(i)}" y="${H - 8}" text-anchor="middle" class="chart-label">${esc(bucketLabel(s.buckets[i], s.bucket, bw * step < 90))}</text>`;
  }

  if (v.chart === "bar") {
    const barW = Math.max(1, Math.min(48, bw * 0.72));
    for (let i = 0; i < n; i++) {
      let base = 0;
      for (let k = 0; k < s.values.length; k++) {
        const val = s.values[k][i];
        if (val <= 0) continue;
        const h = (val / max) * plotH;
        svg += `<rect x="${cx(i) - barW / 2}" y="${y(base + val)}" width="${barW}" height="${Math.max(0.5, h)}" fill="${colors[k]}"/>`;
        base += val;
      }
    }
  } else if (v.chart === "area") {
    const base = new Array<number>(n).fill(0);
    for (let k = 0; k < s.values.length; k++) {
      const top = s.values[k].map((val, i) => base[i] + val);
      const pts = [
        ...top.map((val, i) => `${cx(i)},${y(val)}`),
        ...base.map((val, i) => `${cx(n - 1 - i)},${y(base[n - 1 - i])}`),
      ].join(" ");
      svg += `<polygon points="${pts}" fill="${colors[k]}" fill-opacity="0.55" stroke="${colors[k]}" stroke-width="1"/>`;
      for (let i = 0; i < n; i++) base[i] = top[i];
    }
  } else {
    for (let k = 0; k < s.values.length; k++) {
      const pts = s.values[k].map((val, i) => `${cx(i)},${y(val)}`).join(" ");
      svg += `<polyline points="${pts}" fill="none" stroke="${colors[k]}" stroke-width="2" stroke-linejoin="round"/>`;
      if (n <= 60) svg += s.values[k].map((val, i) => `<circle cx="${cx(i)}" cy="${y(val)}" r="2.5" fill="${colors[k]}"/>`).join("");
    }
  }
  svg += `<rect id="chart-hover" class="hover-col" x="0" y="${padT}" width="${bw}" height="${plotH}" style="display:none"/>`;
  svg += `<rect id="chart-hit" x="${padL}" y="${padT}" width="${plotW}" height="${plotH}" fill="transparent" style="cursor:${s.bucket === "hour" ? "default" : "zoom-in"}"/>`;
  svg += `</svg>`;

  el.innerHTML = `<div class="chart-inner">${svg}<div class="tip hidden" id="chart-tip"></div></div>
    <div class="chart-legend">${s.names
      .map((name, k) => `<span class="legend-item"><span class="legend-dot" style="background:${colors[k]}"></span>${esc(name)}</span>`)
      .join("")}</div>`;

  const hit = el.querySelector<SVGRectElement>("#chart-hit")!;
  const hov = el.querySelector<SVGRectElement>("#chart-hover")!;
  const tip = el.querySelector<HTMLElement>("#chart-tip")!;
  const inner = el.querySelector<HTMLElement>(".chart-inner")!;
  const idxAt = (ev: MouseEvent): number => {
    const r = hit.getBoundingClientRect();
    const x = ((ev.clientX - r.left) / Math.max(1, r.width)) * plotW;
    return Math.max(0, Math.min(n - 1, Math.floor(x / bw)));
  };
  hit.addEventListener("mousemove", (ev) => {
    const i = idxAt(ev);
    hov.setAttribute("x", String(padL + i * bw));
    hov.style.display = "";
    const rows = s.names
      .map((name, k) => ({ name, val: s.values[k][i], color: colors[k] }))
      .filter((r) => r.val > 0)
      .sort((a, b) => b.val - a.val);
    tip.innerHTML = `<div class="tip-head">${esc(s.buckets[i])}</div>${rows
      .map(
        (r) =>
          `<div class="tip-row"><span class="legend-dot" style="background:${r.color}"></span><span class="tip-name">${esc(r.name)}</span><span>${esc(fmtMetric(v.metric, r.val, true))}</span></div>`,
      )
      .join("")}${rows.length > 1 ? `<div class="tip-row tip-total"><span></span><span class="tip-name">Total</span><span>${esc(fmtMetric(v.metric, s.totals[i], true))}</span></div>` : ""}${rows.length ? "" : '<div class="tip-row">no usage</div>'}`;
    tip.classList.remove("hidden");
    const box = inner.getBoundingClientRect();
    const px = ev.clientX - box.left;
    const w = tip.offsetWidth;
    tip.style.left = `${px + 14 + w > box.width ? Math.max(0, px - w - 14) : px + 14}px`;
    tip.style.top = `${Math.max(0, Math.min(ev.clientY - box.top - 10, box.height - tip.offsetHeight))}px`;
  });
  hit.addEventListener("mouseleave", () => {
    hov.style.display = "none";
    tip.classList.add("hidden");
  });
  hit.addEventListener("click", (ev) => onZoom(s.buckets[idxAt(ev)]));
}

function zoomTo(key: string, bucket: Exclude<BucketId, "auto">): void {
  const days = bucketDays(key, bucket);
  if (!days) return;
  view.range = "custom";
  [view.from, view.to] = days;
  view.bucket = bucket === "day" ? "hour" : "day";
  persistView();
  syncFilterInputs();
  syncExploreControls();
  renderActive();
  toast(`Zoomed to ${key} — choose a range to reset`);
}

// ---------------------------------------------------------------------------
// Explore tab
// ---------------------------------------------------------------------------

let exploreBuilt = false;

function buildExplore(): void {
  if (exploreBuilt) return;
  exploreBuilt = true;
  const opt = (vals: [string, string][]) => vals.map(([v, l]) => `<option value="${v}">${l}</option>`).join("");
  $("tab-explore").innerHTML = `
    <div class="ctl-row">
      <label>Metric <select id="ex-metric">${opt(Object.entries(METRIC_LABELS) as [string, string][])}</select></label>
      <label>Stack by <select id="ex-stack">${opt([["agent", "Agent"], ["model", "Model"], ["project", "Project"], ["none", "Nothing (total)"]])}</select></label>
      <label>Bucket <select id="ex-bucket">${opt([["auto", "Auto"], ["hour", "Hour"], ["day", "Day"], ["week", "Week"], ["month", "Month"]])}</select></label>
      <div class="seg" id="ex-chart-type">
        <button data-ct="bar">Bars</button><button data-ct="area">Area</button><button data-ct="line">Lines</button>
      </div>
    </div>
    <section class="kpi-row" id="ex-kpis"></section>
    <section class="panel">
      <div class="panel-head"><h2 id="ex-chart-title">Usage</h2><span class="panel-sub" id="ex-chart-sub"></span><span class="spacer"></span>${expButtons("chart")}</div>
      <div class="chart-box" id="ex-chart"></div>
    </section>
    <section class="panel">
      <div class="panel-head"><h2>Breakdown</h2>
        <label class="inline">by <select id="ex-group">${opt([["agent", "Agent"], ["model", "Model"], ["project", "Project"]])}</select></label></div>
      <div id="ex-table"></div>
    </section>
    <section class="panel">
      <div class="panel-head"><h2>When you work</h2><span class="panel-sub">weekday × hour, uses the metric above</span></div>
      <div id="ex-heat"></div>
    </section>`;

  const on = (id: string, fn: (v: string) => void) =>
    $<HTMLSelectElement>(id).addEventListener("change", (e) => fn((e.target as HTMLSelectElement).value));
  on("ex-metric", (v) => ((view.metric = sanitizeView({ ...view, metric: v }).metric), onViewChanged()));
  on("ex-stack", (v) => ((view.stack = sanitizeView({ ...view, stack: v }).stack), onViewChanged()));
  on("ex-bucket", (v) => ((view.bucket = sanitizeView({ ...view, bucket: v }).bucket), onViewChanged()));
  on("ex-group", (v) => {
    groupDim = (["agent", "model", "project"] as Dim[]).includes(v as Dim) ? (v as Dim) : "agent";
    saveJSON("tt.groupdim", groupDim);
    renderExplore();
  });
  $("ex-chart-type").addEventListener("click", (e) => {
    const b = (e.target as HTMLElement).closest<HTMLElement>("[data-ct]");
    if (!b) return;
    view.chart = sanitizeView({ ...view, chart: b.dataset.ct }).chart;
    onViewChanged();
  });
  syncExploreControls();
}

function syncExploreControls(): void {
  if (!exploreBuilt) return;
  $<HTMLSelectElement>("ex-metric").value = view.metric;
  $<HTMLSelectElement>("ex-stack").value = view.stack;
  $<HTMLSelectElement>("ex-bucket").value = view.bucket;
  $<HTMLSelectElement>("ex-group").value = groupDim;
  document.querySelectorAll<HTMLElement>("#ex-chart-type [data-ct]").forEach((b) => b.classList.toggle("on", b.dataset.ct === view.chart));
}

function kpi(label: string, value: string, sub = "", delta = ""): string {
  return `<div class="kpi"><div class="kpi-label">${esc(label)}</div><div class="kpi-val">${esc(value)}${delta}</div>${sub ? `<div class="kpi-sub">${esc(sub)}</div>` : ""}</div>`;
}

function groupCols(dim: Dim, total: { cost: number }): Col<GroupRow>[] {
  const label = dim === "agent" ? "Agent" : dim === "model" ? "Model" : "Project";
  return [
    { id: "name", label, get: (r) => (dim === "project" ? r.key || "(no project)" : r.name), fmt: (r) => r.name, color: dim === "agent" ? (r) => agentColor(r.key) : undefined },
    ...(dim !== "agent" ? [{ id: "agents", label: "Agents", get: (r: GroupRow) => r.agents.join(", "), on: dim === "model" } as Col<GroupRow>] : []),
    { id: "cost", label: "Cost", num: true, exp: "Cost (USD)", get: (r) => r.cost, fmt: (r) => fmtMoney(r.cost) },
    { id: "share", label: "Share", num: true, get: (r) => (total.cost > 0 ? r.cost / total.cost : 0), fmt: (r) => fmtPct(total.cost > 0 ? r.cost / total.cost : 0, 1) },
    { id: "tokens", label: "Tokens", num: true, get: (r) => totalTokens(r), fmt: (r) => fmtTokens(totalTokens(r)) },
    { id: "input", label: "Input", num: true, on: false, get: (r) => r.input, fmt: (r) => fmtTokens(r.input) },
    { id: "output", label: "Output", num: true, on: false, get: (r) => r.output, fmt: (r) => fmtTokens(r.output) },
    { id: "cr", label: "Cache read", num: true, on: false, get: (r) => r.cr, fmt: (r) => fmtTokens(r.cr) },
    { id: "cc", label: "Cache write", num: true, on: false, get: (r) => r.cc, fmt: (r) => fmtTokens(r.cc) },
    { id: "hit", label: "Cache hit", num: true, get: (r) => cacheHit(r), fmt: (r) => fmtPct(cacheHit(r)) },
    { id: "saved", label: "Cache saved", num: true, on: false, exp: "Cache saved (USD)", get: (r) => r.saved, fmt: (r) => fmtMoney(r.saved) },
    { id: "sessions", label: "Sessions", num: true, get: (r) => r.sessions },
    { id: "per_m", label: "Cost / 1M tok", num: true, on: false, exp: "Cost per 1M tokens (USD)", get: (r) => perMillion(r.cost, totalTokens(r)), fmt: (r) => fmtMoney(perMillion(r.cost, totalTokens(r))) },
    { id: "per_s", label: "Cost / session", num: true, on: false, exp: "Cost per session (USD)", get: (r) => (r.sessions ? r.cost / r.sessions : 0), fmt: (r) => fmtMoney(r.sessions ? r.cost / r.sessions : 0) },
    ...(dim === "model"
      ? [{ id: "note", label: "Note", get: (r: GroupRow) => (r.cost <= 0 && totalTokens(r) > 0 ? "no price data" : ""), fmt: (r: GroupRow) => (r.cost <= 0 && totalTokens(r) > 0 ? "⚠ no price data" : "") } as Col<GroupRow>]
      : []),
  ];
}

function renderExplore(): void {
  if (!facts) return;
  buildExplore();
  syncExploreControls();
  const range = rangeBounds(facts, view);
  const rows = filterRows(facts, view, range);
  const sessions = filterSessions(facts, view, range);
  const sum = sumRows(rows);
  const tokens = totalTokens(sum);

  // previous period of equal length (skipped for all-time)
  let prev = null as ReturnType<typeof sumRows> | null;
  if (view.range !== "all") {
    const len = range[1] - range[0];
    const pr: [number, number] = [range[0] - len, range[0]];
    prev = sumRows(filterRows(facts, view, pr));
  }
  const days = Math.max(1, (range[1] - range[0]) / 86400);
  $("ex-kpis").innerHTML = [
    kpi("Cost", fmtMoney(sum.cost), `${fmtMoney(sum.cost / days)}/day`, prev ? deltaChip(sum.cost, prev.cost) : ""),
    kpi("Tokens", fmtTokens(tokens), `${fmtTokens(tokens / days)}/day`, prev ? deltaChip(tokens, totalTokens(prev)) : ""),
    kpi("Sessions", String(sessions.length), sessions.length ? `${fmtMoney(sum.cost / sessions.length)} avg` : ""),
    kpi("Cache hit rate", fmtPct(cacheHit(sum)), "of prompt tokens read from cache"),
    kpi("Saved by caching", fmtMoney(sum.saved), "vs paying full input price"),
    kpi("Cost / 1M tokens", fmtMoney(perMillion(sum.cost, tokens)), `output:input ${sum.input > 0 ? (sum.output / sum.input).toFixed(2) : "–"}`),
  ].join("");

  const series = buildSeries(facts, rows, view, range);
  $("ex-chart-title").textContent = `${METRIC_LABELS[view.metric]} per ${series.bucket}`;
  $("ex-chart-sub").textContent = `${series.buckets.length} buckets · click one to zoom in`;
  const chartEl = $("ex-chart");
  if (chartEl.clientWidth > 0) {
    drawChart(chartEl, series, view, (key) => (series.bucket === "hour" ? undefined : zoomTo(key, series.bucket)));
  }
  exporters.set("chart", () => ({
    headers: ["bucket", ...series.names, "total"],
    rows: series.buckets.map((b, i) => [b, ...series.values.map((v) => v[i]), series.totals[i]]),
  }));

  // breakdown
  const g = groupBy(facts, rows, sessions, groupDim);
  renderTable($("ex-table"), `breakdown-${groupDim}`, groupCols(groupDim, sum), g, {
    defSort: "cost",
    rerender: renderExplore,
    onRow: (r) => filterTo(groupDim, r.key),
  });

  // heatmap
  const grid = heatmap(rows, view.metric);
  const hmax = Math.max(...grid.flat(), 0);
  const dayNames = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
  const order = prefs.weekStart === 1 ? [1, 2, 3, 4, 5, 6, 0] : [0, 1, 2, 3, 4, 5, 6];
  $("ex-heat").innerHTML = hmax > 0
    ? `<div class="heat"><div></div>${Array.from({ length: 24 }, (_, h) => `<div class="heat-h">${h % 3 === 0 ? h : ""}</div>`).join("")}${order
        .map(
          (d) =>
            `<div class="heat-d">${dayNames[d]}</div>${grid[d]
              .map((val, h) => `<div class="heat-c" style="opacity:${val > 0 ? 0.15 + 0.85 * (val / hmax) : 0.04}" title="${dayNames[d]} ${p2(h)}:00 · ${esc(fmtMetric(view.metric, val))}"></div>`)
              .join("")}`,
        )
        .join("")}</div>`
    : `<div class="empty">No activity in this range.</div>`;
}

function filterTo(dim: Dim, key: string): void {
  const f = dim === "agent" ? "agents" : dim === "model" ? "models" : "projects";
  view[f] = [key];
  persistView();
  buildFilterBar();
  page = 0;
  renderActive();
  toast(`Filtered to ${dim === "project" ? projectLabel(key) : key}`);
}

// ---------------------------------------------------------------------------
// Sessions tab
// ---------------------------------------------------------------------------

let sessionsBuilt = false;

function sessionCols(): Col<SessionRow>[] {
  const tok = (s: SessionRow) => s.input + s.output + s.cache_creation + s.cache_read;
  const hit = (s: SessionRow) => {
    const d = s.input + s.cache_creation + s.cache_read;
    return d > 0 ? s.cache_read / d : 0;
  };
  return [
    { id: "agent", label: "Agent", get: (s) => s.agent, color: (s) => agentColor(s.agent) },
    { id: "title", label: "Title", get: (s) => s.title || "(untitled)" },
    { id: "project", label: "Project", get: (s) => s.cwd, fmt: (s) => (s.cwd ? projectLabel(s.cwd) : "") },
    { id: "model", label: "Model", get: (s) => s.model },
    { id: "tokens", label: "Tokens", num: true, get: tok, fmt: (s) => fmtTokens(tok(s)) },
    { id: "input", label: "Input", num: true, on: false, get: (s) => s.input, fmt: (s) => fmtTokens(s.input) },
    { id: "output", label: "Output", num: true, on: false, get: (s) => s.output, fmt: (s) => fmtTokens(s.output) },
    { id: "cr", label: "Cache read", num: true, on: false, get: (s) => s.cache_read, fmt: (s) => fmtTokens(s.cache_read) },
    { id: "cc", label: "Cache write", num: true, on: false, get: (s) => s.cache_creation, fmt: (s) => fmtTokens(s.cache_creation) },
    { id: "hit", label: "Cache hit", num: true, on: false, get: hit, fmt: (s) => fmtPct(hit(s)) },
    { id: "per_m", label: "Cost / 1M tok", num: true, on: false, exp: "Cost per 1M tokens (USD)", get: (s) => perMillion(s.cost, tok(s)), fmt: (s) => fmtMoney(perMillion(s.cost, tok(s))) },
    { id: "cost", label: "Cost", num: true, exp: "Cost (USD)", get: (s) => s.cost, fmt: (s) => fmtMoney(s.cost) },
    { id: "start", label: "Started", num: true, on: false, get: (s) => s.first_ts, fmt: (s) => fmtDate(s.first_ts) },
    { id: "duration", label: "Duration", num: true, on: false, get: (s) => s.ts - s.first_ts, fmt: (s) => fmtDuration(s.ts - s.first_ts) },
    { id: "last", label: "Last active", num: true, get: (s) => s.ts, fmt: (s) => fmtDate(s.ts) },
  ];
}

function buildSessions(): void {
  if (sessionsBuilt) return;
  sessionsBuilt = true;
  $("tab-sessions").innerHTML = `
    <section class="panel">
      <div class="panel-head"><h2>Sessions</h2><span class="panel-sub" id="se-sub"></span><span class="spacer"></span>
        <input id="se-search" type="search" placeholder="Search title, project, model…"></div>
      <div id="se-table"></div>
      <div class="pager" id="se-pager"></div>
    </section>`;
  $<HTMLInputElement>("se-search").addEventListener("input", (e) => {
    search = (e.target as HTMLInputElement).value.trim().toLowerCase();
    page = 0;
    renderSessions();
  });
  $("se-pager").addEventListener("click", (e) => {
    const b = (e.target as HTMLElement).closest<HTMLElement>("[data-pg]");
    if (!b || b.hasAttribute("disabled")) return;
    page = Math.max(0, page + (b.dataset.pg === "next" ? 1 : -1));
    renderSessions();
  });
}

function renderSessions(): void {
  if (!facts) return;
  buildSessions();
  const range = rangeBounds(facts, view);
  let list = filterSessions(facts, view, range);
  if (search) {
    list = list.filter((s) => `${s.title} ${s.cwd} ${s.model} ${s.agent}`.toLowerCase().includes(search));
  }
  const total = renderTable($("se-table"), "sessions", sessionCols(), list, {
    defSort: "last",
    rerender: renderSessions,
    onSort: () => (page = 0),
    onRow: openDrawer,
    pageStart: page * PAGE_SIZE,
    pageSize: PAGE_SIZE,
    emptyText: "No sessions match the current filters.",
  });
  const pages = Math.max(1, Math.ceil(total / PAGE_SIZE));
  if (page >= pages) {
    page = pages - 1;
    return renderSessions();
  }
  const cost = list.reduce((a, s) => a + s.cost, 0);
  $("se-sub").textContent = `${total.toLocaleString()} sessions · ${fmtMoney(cost)}`;
  $("se-pager").innerHTML = total > PAGE_SIZE
    ? `<button class="btn btn-ghost btn-sm" data-pg="prev"${page === 0 ? " disabled" : ""}>← Prev</button>
       <span>Page ${page + 1} / ${pages}</span>
       <button class="btn btn-ghost btn-sm" data-pg="next"${page >= pages - 1 ? " disabled" : ""}>Next →</button>`
    : "";
}

// ---------------------------------------------------------------------------
// Session drawer
// ---------------------------------------------------------------------------

export function closeDrawer(): void {
  $("drawer").classList.add("hidden");
}

function openDrawer(s: SessionRow): void {
  const total = s.input + s.output + s.cache_creation + s.cache_read;
  const denom = s.input + s.cache_creation + s.cache_read;
  const seg = (label: string, val: number, color: string) =>
    `<div class="seg-row"><span class="legend-dot" style="background:${color}"></span><span>${label}</span><span class="seg-val">${fmtTokens(val)}</span><span class="seg-pct">${total > 0 ? fmtPct(val / total, 1) : "–"}</span></div>`;
  const bar = [
    [s.input, PALETTE[0]],
    [s.output, PALETTE[2]],
    [s.cache_creation, PALETTE[3]],
    [s.cache_read, PALETTE[1]],
  ]
    .map(([v, c]) => `<span style="width:${total > 0 ? ((v as number) / total) * 100 : 0}%;background:${c}"></span>`)
    .join("");
  const facts_ = (k: string, v: string) => `<div class="fact"><span>${esc(k)}</span><span>${esc(v)}</span></div>`;
  $("drawer").innerHTML = `
    <div class="drawer-backdrop" data-close></div>
    <div class="drawer-panel">
      <div class="drawer-head"><div>
        <div class="drawer-agent" style="color:${agentColor(s.agent)}">${esc(s.agent)}</div>
        <h3>${esc(s.title || "(untitled)")}</h3></div>
        <button class="btn btn-ghost btn-sm" data-close>✕</button></div>
      <div class="drawer-cost">${esc(fmtMoney(s.cost, 3))}</div>
      <div class="seg-bar">${bar}</div>
      ${seg("Input", s.input, PALETTE[0])}${seg("Output", s.output, PALETTE[2])}${seg("Cache write", s.cache_creation, PALETTE[3])}${seg("Cache read", s.cache_read, PALETTE[1])}
      <div class="facts">
        ${facts_("Total tokens", total.toLocaleString())}
        ${facts_("Cache hit rate", denom > 0 ? fmtPct(s.cache_read / denom, 1) : "–")}
        ${facts_("Cost / 1M tokens", fmtMoney(perMillion(s.cost, total)))}
        ${facts_("Output : input", s.input > 0 ? (s.output / s.input).toFixed(2) : "–")}
        ${facts_("Model", s.model)}
        ${facts_("Project", s.cwd || "–")}
        ${facts_("Started", fmtDate(s.first_ts))}
        ${facts_("Last active", fmtDate(s.ts))}
        ${facts_("Duration", fmtDuration(s.ts - s.first_ts))}
      </div>
      <div class="drawer-actions">
        ${s.cwd ? '<button class="btn btn-sm" data-flt="project">Filter to this project</button>' : ""}
        <button class="btn btn-sm" data-flt="model">Filter to this model</button>
        <button class="btn btn-sm" data-flt="agent">Filter to this agent</button>
      </div>
    </div>`;
  $("drawer").classList.remove("hidden");
  $("drawer").querySelectorAll<HTMLElement>("[data-close]").forEach((b) => b.addEventListener("click", closeDrawer));
  $("drawer").querySelectorAll<HTMLElement>("[data-flt]").forEach((b) =>
    b.addEventListener("click", () => {
      const d = b.dataset.flt;
      closeDrawer();
      if (d === "project") filterTo("project", s.cwd);
      else if (d === "model") filterTo("model", s.model);
      else filterTo("agent", s.agent);
    }),
  );
}

// ---------------------------------------------------------------------------
// Models & projects tab
// ---------------------------------------------------------------------------

let modelsBuilt = false;

function renderModels(): void {
  if (!facts) return;
  if (!modelsBuilt) {
    modelsBuilt = true;
    $("tab-models").innerHTML = `
      <section class="panel"><div class="panel-head"><h2>Models</h2><span class="panel-sub">click a row to filter to it</span></div><div id="mo-models"></div></section>
      <section class="panel"><div class="panel-head"><h2>Projects</h2><span class="panel-sub">grouped by working folder</span></div><div id="mo-projects"></div></section>`;
  }
  const range = rangeBounds(facts, view);
  const rows = filterRows(facts, view, range);
  const sessions = filterSessions(facts, view, range);
  const sum = sumRows(rows);
  for (const dim of ["model", "project"] as Dim[]) {
    renderTable($(dim === "model" ? "mo-models" : "mo-projects"), `${dim}s`, groupCols(dim, sum), groupBy(facts, rows, sessions, dim), {
      defSort: "cost",
      rerender: renderModels,
      onRow: (r) => filterTo(dim, r.key),
    });
  }
}

// ---------------------------------------------------------------------------
// public api
// ---------------------------------------------------------------------------

const TAB_RENDER: Record<Exclude<TabId, "overview">, () => void> = {
  explore: renderExplore,
  sessions: renderSessions,
  models: renderModels,
};

export function renderActive(): void {
  if (activeTab !== "overview" && facts) TAB_RENDER[activeTab]();
}

export function setActiveTab(tab: TabId): void {
  activeTab = tab;
  $("filterbar").classList.toggle("hidden", tab === "overview");
  for (const id of ["overview", "explore", "sessions", "models"] as TabId[]) {
    $(`tab-${id}`).classList.toggle("hidden", id !== tab);
  }
  document.querySelectorAll<HTMLElement>("#tabs [data-tab]").forEach((b) => b.classList.toggle("on", b.dataset.tab === tab));
  closeDrawer();
  if (tab !== "overview" && !facts) {
    $(`tab-${tab}`).innerHTML = `<div class="empty">Loading usage data…</div>`;
    exploreBuilt = sessionsBuilt = modelsBuilt = false;
  }
  renderActive();
}

export function getTab(): TabId {
  return activeTab;
}

export function setFacts(f: Facts): void {
  const first = facts === null;
  facts = f;
  if (first) {
    exploreBuilt = sessionsBuilt = modelsBuilt = false;
    wireOnce();
  }
  // rebuilding closes open dropdowns, so only do it when the options changed
  const key = JSON.stringify(filterOptions(f));
  if (first || key !== optionsKey) {
    optionsKey = key;
    buildFilterBar();
  }
  renderActive();
}

let optionsKey = "";

let wired = false;
function wireOnce(): void {
  if (wired) return;
  wired = true;
  wireFilterBar();
  $("content").addEventListener("click", (e) => {
    const b = (e.target as HTMLElement).closest<HTMLElement>("[data-exp-kind]");
    if (b) void doExport(b.dataset.expKind!, b.dataset.expName!);
  });
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape") closeDrawer();
  });
}

export function currentFacts(): Facts | null {
  return facts;
}
