// Facts store + filtering/aggregation used by Overview insights and the
// Explore / Sessions / Models tabs. All grouping happens client-side.

import { invoke } from "@tauri-apps/api/core";
import { dayStart, p2, parseYmd, parts, prefs, ymdStr } from "./prefs";

export type FactRow = number[];
export const F = { H: 0, A: 1, M: 2, C: 3, IN: 4, OUT: 5, CC: 6, CR: 7, COST: 8, SAVED: 9 } as const;

export interface SessionRow {
  agent: string;
  model: string;
  ts: number;
  first_ts: number;
  title: string;
  cwd: string;
  input: number;
  output: number;
  cache_creation: number;
  cache_read: number;
  cost: number;
}

export interface Facts {
  strings: string[];
  rows: FactRow[];
  sessions: SessionRow[];
}

export async function fetchFacts(): Promise<Facts> {
  return invoke<Facts>("usage_facts");
}

// ---------------------------------------------------------------------------
// view (filters + chart options)
// ---------------------------------------------------------------------------

export type RangeId = "today" | "7d" | "30d" | "90d" | "month" | "all" | "custom";
export type Metric = "cost" | "tokens" | "input" | "output" | "cache_read" | "cache_creation";
export type Stack = "agent" | "model" | "project" | "none";
export type BucketId = "auto" | "hour" | "day" | "week" | "month";
export type ChartType = "bar" | "line" | "area";

export interface View {
  range: RangeId;
  from: string;
  to: string;
  agents: string[];
  models: string[];
  projects: string[];
  metric: Metric;
  stack: Stack;
  bucket: BucketId;
  chart: ChartType;
}

export const DEFAULT_VIEW: View = {
  range: "30d",
  from: "",
  to: "",
  agents: [],
  models: [],
  projects: [],
  metric: "cost",
  stack: "agent",
  bucket: "auto",
  chart: "bar",
};

const RANGES: RangeId[] = ["today", "7d", "30d", "90d", "month", "all", "custom"];
const METRICS: Metric[] = ["cost", "tokens", "input", "output", "cache_read", "cache_creation"];
const STACKS: Stack[] = ["agent", "model", "project", "none"];
const BUCKETS: BucketId[] = ["auto", "hour", "day", "week", "month"];
const CHARTS: ChartType[] = ["bar", "line", "area"];

function pick<T extends string>(v: unknown, allowed: T[], fallback: T): T {
  return typeof v === "string" && (allowed as string[]).includes(v) ? (v as T) : fallback;
}

function strList(v: unknown): string[] {
  return Array.isArray(v) ? v.filter((s): s is string => typeof s === "string").slice(0, 500) : [];
}

export function sanitizeView(x: unknown): View {
  const o = (x && typeof x === "object" ? x : {}) as Record<string, unknown>;
  return {
    range: pick(o.range, RANGES, DEFAULT_VIEW.range),
    from: typeof o.from === "string" ? o.from : "",
    to: typeof o.to === "string" ? o.to : "",
    agents: strList(o.agents),
    models: strList(o.models),
    projects: strList(o.projects),
    metric: pick(o.metric, METRICS, DEFAULT_VIEW.metric),
    stack: pick(o.stack, STACKS, DEFAULT_VIEW.stack),
    bucket: pick(o.bucket, BUCKETS, DEFAULT_VIEW.bucket),
    chart: pick(o.chart, CHARTS, DEFAULT_VIEW.chart),
  };
}

// ---------------------------------------------------------------------------
// ranges + filtering
// ---------------------------------------------------------------------------

/** [from, to) in unix seconds. */
export function rangeBounds(facts: Facts, view: View): [number, number] {
  const now = Math.floor(Date.now() / 1000);
  const p = parts(now);
  const today = dayStart(p.y, p.m, p.d);
  const tomorrow = dayStart(p.y, p.m, p.d + 1);
  switch (view.range) {
    case "today":
      return [today, tomorrow];
    case "7d":
      return [dayStart(p.y, p.m, p.d - 6), tomorrow];
    case "90d":
      return [dayStart(p.y, p.m, p.d - 89), tomorrow];
    case "month":
      return [dayStart(p.y, p.m, 1), tomorrow];
    case "all": {
      const first = facts.rows.length ? facts.rows[0][F.H] * 3600 : today;
      return [Math.min(first, today), tomorrow];
    }
    case "custom": {
      const a = parseYmd(view.from);
      const b = parseYmd(view.to);
      if (a && b) {
        let from = dayStart(a.y, a.m, a.d);
        let to = dayStart(b.y, b.m, b.d + 1);
        if (to <= from) {
          from = dayStart(b.y, b.m, b.d);
          to = dayStart(a.y, a.m, a.d + 1);
        }
        return [from, to];
      }
      return [dayStart(p.y, p.m, p.d - 29), tomorrow];
    }
    case "30d":
    default:
      return [dayStart(p.y, p.m, p.d - 29), tomorrow];
  }
}

function mask(strings: string[], names: string[]): boolean[] | null {
  if (!names.length) return null;
  const set = new Set(names);
  return strings.map((s) => set.has(s));
}

export function filterRows(facts: Facts, view: View, range: [number, number]): FactRow[] {
  const ma = mask(facts.strings, view.agents);
  const mm = mask(facts.strings, view.models);
  const mp = mask(facts.strings, view.projects);
  const [from, to] = range;
  const out: FactRow[] = [];
  for (const r of facts.rows) {
    const t = r[F.H] * 3600;
    if (t < from || t >= to) continue;
    if (ma && !ma[r[F.A]]) continue;
    if (mm && !mm[r[F.M]]) continue;
    if (mp && !mp[r[F.C]]) continue;
    out.push(r);
  }
  return out;
}

export function filterSessions(facts: Facts, view: View, range: [number, number]): SessionRow[] {
  const a = view.agents.length ? new Set(view.agents) : null;
  const m = view.models.length ? new Set(view.models) : null;
  const p = view.projects.length ? new Set(view.projects) : null;
  const [from, to] = range;
  return facts.sessions.filter(
    (s) =>
      s.ts >= from &&
      s.ts < to &&
      (!a || a.has(s.agent)) &&
      (!m || m.has(s.model)) &&
      (!p || p.has(s.cwd)),
  );
}

// ---------------------------------------------------------------------------
// sums / derived metrics
// ---------------------------------------------------------------------------

export interface Sum {
  cost: number;
  input: number;
  output: number;
  cc: number;
  cr: number;
  saved: number;
}

export const emptySum = (): Sum => ({ cost: 0, input: 0, output: 0, cc: 0, cr: 0, saved: 0 });

export function addRow(s: Sum, r: FactRow): void {
  s.cost += r[F.COST];
  s.input += r[F.IN];
  s.output += r[F.OUT];
  s.cc += r[F.CC];
  s.cr += r[F.CR];
  s.saved += r[F.SAVED];
}

export function sumRows(rows: FactRow[]): Sum {
  const s = emptySum();
  for (const r of rows) addRow(s, r);
  return s;
}

export const totalTokens = (s: Sum): number => s.input + s.output + s.cc + s.cr;

/** Share of prompt tokens served from cache. */
export function cacheHit(s: Sum): number {
  const denom = s.input + s.cc + s.cr;
  return denom > 0 ? s.cr / denom : 0;
}

export function metricValue(r: FactRow, m: Metric): number {
  switch (m) {
    case "cost":
      return r[F.COST];
    case "tokens":
      return r[F.IN] + r[F.OUT] + r[F.CC] + r[F.CR];
    case "input":
      return r[F.IN];
    case "output":
      return r[F.OUT];
    case "cache_read":
      return r[F.CR];
    case "cache_creation":
      return r[F.CC];
  }
}

export const METRIC_LABELS: Record<Metric, string> = {
  cost: "Cost",
  tokens: "All tokens",
  input: "Input tokens",
  output: "Output tokens",
  cache_read: "Cache reads",
  cache_creation: "Cache writes",
};

export function projectLabel(cwd: string): string {
  if (!cwd) return "(no project)";
  const parts = cwd.replace(/[\\/]+$/, "").split(/[\\/]/).filter(Boolean);
  return parts.slice(-2).join("/") || cwd;
}

// ---------------------------------------------------------------------------
// time series
// ---------------------------------------------------------------------------

export function pickBucket(view: View, range: [number, number]): Exclude<BucketId, "auto"> {
  if (view.bucket !== "auto") return view.bucket;
  const days = (range[1] - range[0]) / 86400;
  if (days <= 2.01) return "hour";
  if (days <= 92) return "day";
  if (days <= 400) return "week";
  return "month";
}

export function bucketKey(secs: number, bucket: Exclude<BucketId, "auto">): string {
  const p = parts(secs);
  switch (bucket) {
    case "hour":
      return `${ymdStr(p.y, p.m, p.d)} ${p2(p.h)}:00`;
    case "day":
      return ymdStr(p.y, p.m, p.d);
    case "week": {
      const off = (p.dow - prefs.weekStart + 7) % 7;
      return ymdStr(p.y, p.m, p.d - off);
    }
    case "month":
      return `${p.y}-${p2(p.m + 1)}`;
  }
}

/** Inclusive day range [from, to] (YYYY-MM-DD) covered by a bucket key, or null for hours. */
export function bucketDays(key: string, bucket: Exclude<BucketId, "auto">): [string, string] | null {
  if (bucket === "hour") return null;
  if (bucket === "day") return [key, key];
  if (bucket === "week") {
    const s = parseYmd(key);
    return s ? [key, ymdStr(s.y, s.m, s.d + 6)] : null;
  }
  const mt = /^(\d{4})-(\d{2})$/.exec(key);
  if (!mt) return null;
  const y = +mt[1];
  const m = +mt[2] - 1;
  return [ymdStr(y, m, 1), ymdStr(y, m + 1, 0)];
}

export interface Series {
  bucket: Exclude<BucketId, "auto">;
  buckets: string[];
  names: string[];
  keys: string[]; // raw key per series ("" for none/other)
  values: number[][]; // [series][bucket]
  totals: number[]; // per bucket
}

export function stackKey(facts: Facts, r: FactRow, stack: Stack): string {
  switch (stack) {
    case "agent":
      return facts.strings[r[F.A]];
    case "model":
      return facts.strings[r[F.M]];
    case "project":
      return facts.strings[r[F.C]];
    case "none":
      return "Total";
  }
}

const MAX_SERIES = 8;

export function buildSeries(facts: Facts, rows: FactRow[], view: View, range: [number, number]): Series {
  const bucket = pickBucket(view, range);
  const step = bucket === "hour" ? 3600 : 6 * 3600;
  const buckets: string[] = [];
  const index = new Map<string, number>();
  const push = (k: string) => {
    if (!index.has(k)) {
      index.set(k, buckets.length);
      buckets.push(k);
    }
  };
  const to = Math.max(range[0] + 1, range[1] - 1);
  for (let t = range[0]; t < range[1]; t += step) push(bucketKey(t, bucket));
  push(bucketKey(to, bucket));

  // totals per stack key to choose the top series
  const totalByKey = new Map<string, number>();
  for (const r of rows) {
    const k = stackKey(facts, r, view.stack);
    totalByKey.set(k, (totalByKey.get(k) ?? 0) + metricValue(r, view.metric));
  }
  const ranked = [...totalByKey.entries()].sort((a, b) => b[1] - a[1]).map(([k]) => k);
  const top = ranked.length > MAX_SERIES + 1 ? ranked.slice(0, MAX_SERIES) : ranked;
  const topIdx = new Map(top.map((k, i) => [k, i]));
  const hasOther = ranked.length > top.length;

  const keys = [...top, ...(hasOther ? ["\u0000other"] : [])];
  const names = keys.map((k) => (k === "\u0000other" ? "Other" : view.stack === "project" ? projectLabel(k) : k));
  const values = keys.map(() => new Array<number>(buckets.length).fill(0));
  const totals = new Array<number>(buckets.length).fill(0);

  for (const r of rows) {
    const bi = index.get(bucketKey(r[F.H] * 3600, bucket));
    if (bi === undefined) continue;
    const k = stackKey(facts, r, view.stack);
    const si = topIdx.get(k) ?? keys.length - 1;
    const v = metricValue(r, view.metric);
    values[si][bi] += v;
    totals[bi] += v;
  }
  return { bucket, buckets, names, keys, values, totals };
}

// ---------------------------------------------------------------------------
// grouping (breakdown / models tables)
// ---------------------------------------------------------------------------

export type Dim = "agent" | "model" | "project";

export interface GroupRow extends Sum {
  key: string;
  name: string;
  sessions: number;
  agents: string[]; // agents that used it (for models/projects)
}

export function groupBy(facts: Facts, rows: FactRow[], sessions: SessionRow[], dim: Dim): GroupRow[] {
  const map = new Map<string, GroupRow>();
  const agentSets = new Map<string, Set<string>>();
  const get = (key: string): GroupRow => {
    let g = map.get(key);
    if (!g) {
      g = { ...emptySum(), key, name: dim === "project" ? projectLabel(key) : key, sessions: 0, agents: [] };
      map.set(key, g);
      agentSets.set(key, new Set());
    }
    return g;
  };
  for (const r of rows) {
    const key = dim === "agent" ? facts.strings[r[F.A]] : dim === "model" ? facts.strings[r[F.M]] : facts.strings[r[F.C]];
    addRow(get(key), r);
    agentSets.get(key)!.add(facts.strings[r[F.A]]);
  }
  for (const s of sessions) {
    const key = dim === "agent" ? s.agent : dim === "model" ? s.model : s.cwd;
    const g = map.get(key);
    if (g) g.sessions += 1;
  }
  for (const [k, g] of map) g.agents = [...agentSets.get(k)!].sort();
  return [...map.values()];
}

/** [weekday 0-6][hour 0-23] sums of the chosen metric. */
export function heatmap(rows: FactRow[], metric: Metric): number[][] {
  const grid = Array.from({ length: 7 }, () => new Array<number>(24).fill(0));
  for (const r of rows) {
    const p = parts(r[F.H] * 3600);
    grid[p.dow][p.h] += metricValue(r, metric);
  }
  return grid;
}

/** Distinct option lists for the filter dropdowns, most expensive first. */
export function filterOptions(facts: Facts): { agents: string[]; models: string[]; projects: string[] } {
  const totals = [new Map<number, number>(), new Map<number, number>(), new Map<number, number>()];
  for (const r of facts.rows) {
    [r[F.A], r[F.M], r[F.C]].forEach((idx, i) => totals[i].set(idx, (totals[i].get(idx) ?? 0) + r[F.COST] + 1e-9));
  }
  const list = (m: Map<number, number>) =>
    [...m.entries()].sort((a, b) => b[1] - a[1]).map(([i]) => facts.strings[i]);
  return { agents: list(totals[0]), models: list(totals[1]), projects: list(totals[2]) };
}
