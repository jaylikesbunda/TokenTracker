// User display preferences, persisted formatters and small storage helpers.

export const AGENT_COLORS: Record<string, string> = {
  "Claude Code": "#b08a5a",
  "Codex CLI": "#6a9b80",
  OpenCode: "#8f9bb8",
  CommandCode: "#7aa2c4",
  OSAgent: "#a98bc4",
};

export const AGENT_MONOGRAMS: Record<string, string> = {
  "Claude Code": "CC",
  "Codex CLI": "CX",
  OpenCode: "OC",
  CommandCode: "CM",
  OSAgent: "OA",
};

export const PALETTE = [
  "#e8e8e8",
  "#6fae8f",
  "#c9a15f",
  "#a98bc4",
  "#cf7d7b",
  "#5fb3b3",
  "#c47ab0",
  "#8fb56a",
  "#d08f5a",
  "#7f8fd6",
];

export const OTHER_COLOR = "#5f6775";

// ---------------------------------------------------------------------------
// storage (localStorage may be unavailable; everything degrades to defaults)
// ---------------------------------------------------------------------------

export function loadJSON<T>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(key);
    if (!raw) return fallback;
    return JSON.parse(raw) as T;
  } catch {
    return fallback;
  }
}

export function saveJSON(key: string, value: unknown): void {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    /* ignore */
  }
}

// ---------------------------------------------------------------------------
// preferences
// ---------------------------------------------------------------------------

export interface Prefs {
  symbol: string;
  rate: number; // multiplier applied to USD amounts
  tokens: "compact" | "full";
  utc: boolean;
  weekStart: 0 | 1; // 0 = Sunday, 1 = Monday
}

export const DEFAULT_PREFS: Prefs = { symbol: "$", rate: 1, tokens: "compact", utc: false, weekStart: 1 };

function sanitizePrefs(x: unknown): Prefs {
  const o = (x && typeof x === "object" ? x : {}) as Record<string, unknown>;
  const rate = typeof o.rate === "number" && isFinite(o.rate) && o.rate > 0 && o.rate < 1e6 ? o.rate : 1;
  const symbol = typeof o.symbol === "string" && o.symbol.length > 0 && o.symbol.length <= 4 ? o.symbol : "$";
  return {
    symbol,
    rate,
    tokens: o.tokens === "full" ? "full" : "compact",
    utc: o.utc === true,
    weekStart: o.weekStart === 0 ? 0 : 1,
  };
}

export const prefs: Prefs = sanitizePrefs(loadJSON<unknown>("tt.prefs", {}));

export function updatePrefs(patch: Partial<Prefs>): void {
  Object.assign(prefs, sanitizePrefs({ ...prefs, ...patch }));
  saveJSON("tt.prefs", prefs);
}

// ---------------------------------------------------------------------------
// formatting
// ---------------------------------------------------------------------------

export const esc = (s: string): string =>
  s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

export function fmtMoney(usd: number, digits = 2): string {
  const v = usd * prefs.rate;
  if (Math.abs(v) >= 1000) return `${prefs.symbol}${v.toLocaleString("en-US", { maximumFractionDigits: 0 })}`;
  return `${prefs.symbol}${v.toFixed(digits)}`;
}

export function fmtTokens(n: number): string {
  if (prefs.tokens === "full") return Math.round(n).toLocaleString("en-US");
  if (n >= 1_000_000_000) return `${(n / 1_000_000_000).toFixed(1)}B`;
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${Math.round(n / 1000)}k`;
  return String(Math.round(n));
}

export function fmtPct(x: number, digits = 0): string {
  return `${(x * 100).toFixed(digits)}%`;
}

export const p2 = (n: number): string => String(n).padStart(2, "0");

// ---------------------------------------------------------------------------
// time helpers (timezone follows prefs.utc unless overridden)
// ---------------------------------------------------------------------------

export interface Parts {
  y: number;
  m: number; // 0-based
  d: number;
  h: number;
  dow: number;
}

export function parts(secs: number, utc: boolean = prefs.utc): Parts {
  const dt = new Date(secs * 1000);
  return utc
    ? { y: dt.getUTCFullYear(), m: dt.getUTCMonth(), d: dt.getUTCDate(), h: dt.getUTCHours(), dow: dt.getUTCDay() }
    : { y: dt.getFullYear(), m: dt.getMonth(), d: dt.getDate(), h: dt.getHours(), dow: dt.getDay() };
}

/** Unix seconds at 00:00 of the given (normalised) calendar day. */
export function dayStart(y: number, m: number, d: number, utc: boolean = prefs.utc): number {
  return (utc ? Date.UTC(y, m, d) : new Date(y, m, d).getTime()) / 1000;
}

export function ymdStr(y: number, m: number, d: number): string {
  const dt = new Date(Date.UTC(y, m, d));
  return `${dt.getUTCFullYear()}-${p2(dt.getUTCMonth() + 1)}-${p2(dt.getUTCDate())}`;
}

export function parseYmd(s: string): { y: number; m: number; d: number } | null {
  const mt = /^(\d{4})-(\d{2})-(\d{2})$/.exec(s);
  if (!mt) return null;
  const y = +mt[1];
  const m = +mt[2] - 1;
  const d = +mt[3];
  if (m < 0 || m > 11 || d < 1 || d > 31) return null;
  return { y, m, d };
}

export function fmtDate(secs: number): string {
  const p = parts(secs);
  return `${p.y}-${p2(p.m + 1)}-${p2(p.d)} ${p2(p.h)}:${p2(new Date(secs * 1000)[prefs.utc ? "getUTCMinutes" : "getMinutes"]())}`;
}

export function fmtDay(secs: number): string {
  const p = parts(secs);
  return `${p.y}-${p2(p.m + 1)}-${p2(p.d)}`;
}

export function fmtDuration(secs: number): string {
  if (secs < 60) return `${Math.max(0, Math.round(secs))}s`;
  const m = Math.floor(secs / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 48) return `${h}h ${m % 60}m`;
  return `${Math.floor(h / 24)}d ${h % 24}h`;
}
