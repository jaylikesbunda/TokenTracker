//! Compact, filter-friendly views over the history store for the Explore,
//! Sessions and Models tabs. The UI does all grouping/filtering client-side,
//! so this only hands over hour-bucketed facts plus a full session list.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Serialize;

use crate::model::UsageRecord;
use crate::pricing;

const MAX_SESSIONS: usize = 20_000;
const MAX_EXPORT_BYTES: usize = 64 * 1024 * 1024;

/// (hour, agent, model, cwd, input, output, cache_creation, cache_read, cost, cache_saved)
/// `agent`, `model` and `cwd` index into `Facts::strings`; `hour` is unix seconds / 3600.
pub type FactRow = (i64, u32, u32, u32, u64, u64, u64, u64, f64, f64);

#[derive(Serialize)]
pub struct SessionRow {
    pub agent: String,
    pub model: String,
    pub ts: i64,
    pub first_ts: i64,
    pub title: String,
    pub cwd: String,
    pub input: u64,
    pub output: u64,
    pub cache_creation: u64,
    pub cache_read: u64,
    pub cost: f64,
}

#[derive(Serialize)]
pub struct Facts {
    pub strings: Vec<String>,
    pub rows: Vec<FactRow>,
    pub sessions: Vec<SessionRow>,
}

struct Interner {
    map: HashMap<String, u32>,
    list: Vec<String>,
}

impl Interner {
    fn id(&mut self, s: &str) -> u32 {
        if let Some(i) = self.map.get(s) {
            return *i;
        }
        let i = self.list.len() as u32;
        self.list.push(s.to_string());
        self.map.insert(s.to_string(), i);
        i
    }
}

/// Dollars saved by cache reads versus paying the full input rate.
fn cache_saved(model: &str, cache_read: u64) -> f64 {
    if cache_read == 0 {
        return 0.0;
    }
    match pricing::lookup(model) {
        Some(p) => match p.cache_read_input_token_cost {
            Some(read) if p.input_cost_per_token > read => {
                cache_read as f64 * (p.input_cost_per_token - read)
            }
            _ => 0.0,
        },
        None => 0.0,
    }
}

fn clean_path(cwd: &str) -> String {
    cwd.trim()
        .trim_start_matches(r"\\?\")
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_string()
}

/// Claude Code names project dirs by replacing every non-alphanumeric
/// character of the cwd with '-' (`I:\a\b_c` -> `I--a-b-c`).
fn mangle(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect::<String>().to_lowercase()
}

fn is_slug(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() > 3 && !s.contains(['/', '\\', ':']) && b[0].is_ascii_alphabetic() && &s[1..3] == "--"
}

/// Merges different spellings of the same project folder (slash style, case,
/// trailing separator, Claude's mangled slug) into one path.
struct CwdCanon {
    by_key: HashMap<String, String>,
    by_slug: HashMap<String, String>,
}

impl CwdCanon {
    fn new(records: &[UsageRecord]) -> Self {
        let mut by_key: HashMap<String, String> = HashMap::new();
        let mut by_slug: HashMap<String, String> = HashMap::new();
        for r in records {
            if r.cwd.trim().is_empty() || is_slug(&r.cwd) {
                continue;
            }
            let clean = clean_path(&r.cwd);
            let key = clean.to_lowercase();
            if by_key.contains_key(&key) {
                continue;
            }
            by_slug.entry(mangle(&clean)).or_insert_with(|| clean.clone());
            by_key.insert(key, clean);
        }
        CwdCanon { by_key, by_slug }
    }

    fn canon(&self, cwd: &str) -> String {
        if cwd.trim().is_empty() {
            return String::new();
        }
        if is_slug(cwd) {
            return self.by_slug.get(&mangle(cwd)).cloned().unwrap_or_else(|| cwd.to_string());
        }
        let clean = clean_path(cwd);
        self.by_key.get(&clean.to_lowercase()).cloned().unwrap_or(clean)
    }
}

pub fn build_facts(records: &[UsageRecord]) -> Facts {
    let canon = CwdCanon::new(records);
    let mut strings = Interner { map: HashMap::new(), list: Vec::new() };
    let mut rows: HashMap<(i64, u32, u32, u32), [f64; 6]> = HashMap::new();
    let mut sessions: HashMap<(&str, &str), SessionRow> = HashMap::new();

    for r in records {
        if r.ts <= 0 {
            continue;
        }
        let cwd = canon.canon(&r.cwd);
        let key = (
            r.ts.div_euclid(3600),
            strings.id(r.agent),
            strings.id(&r.model),
            strings.id(&cwd),
        );
        let e = rows.entry(key).or_insert([0.0; 6]);
        e[0] += r.input as f64;
        e[1] += r.output as f64;
        e[2] += r.cache_creation as f64;
        e[3] += r.cache_read as f64;
        e[4] += r.cost;
        e[5] += cache_saved(&r.model, r.cache_read);

        let s = sessions.entry((r.agent, r.session_id.as_str())).or_insert_with(|| SessionRow {
            agent: r.agent.to_string(),
            model: r.model.clone(),
            ts: r.ts,
            first_ts: r.ts,
            title: r.title.clone(),
            cwd: cwd.clone(),
            input: 0,
            output: 0,
            cache_creation: 0,
            cache_read: 0,
            cost: 0.0,
        });
        s.input += r.input;
        s.output += r.output;
        s.cache_creation += r.cache_creation;
        s.cache_read += r.cache_read;
        s.cost += r.cost;
        if r.ts > s.ts {
            s.ts = r.ts;
        }
        if r.ts < s.first_ts {
            s.first_ts = r.ts;
        }
        if s.title.is_empty() {
            s.title = r.title.clone();
        }
        if s.cwd.is_empty() {
            s.cwd = cwd;
        }
    }

    let mut rows: Vec<FactRow> = rows
        .into_iter()
        .map(|((hour, a, m, c), v)| {
            (hour, a, m, c, v[0] as u64, v[1] as u64, v[2] as u64, v[3] as u64, v[4], v[5])
        })
        .collect();
    rows.sort_by_key(|r| r.0);

    let mut sessions: Vec<SessionRow> = sessions.into_values().collect();
    sessions.sort_by(|a, b| b.ts.cmp(&a.ts));
    sessions.truncate(MAX_SESSIONS);

    Facts { strings: strings.list, rows, sessions }
}

fn downloads_dir() -> Result<PathBuf, String> {
    let home = crate::sources::home_dir().ok_or("cannot resolve home directory")?;
    let dir = home.join("Downloads");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// Keep only a safe file name: no separators, limited charset, csv/json only.
fn sanitize_filename(name: &str) -> Result<String, String> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let clean: String = base
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .take(80)
        .collect();
    let lower = clean.to_ascii_lowercase();
    if clean.starts_with('.') || !(lower.ends_with(".csv") || lower.ends_with(".json")) {
        return Err("export must be a .csv or .json file".into());
    }
    Ok(clean)
}

/// Write an export into the user's Downloads folder without overwriting
/// anything. Returns the final path.
pub fn save_export(filename: &str, content: &str) -> Result<String, String> {
    if content.len() > MAX_EXPORT_BYTES {
        return Err("export too large".into());
    }
    let name = sanitize_filename(filename)?;
    let dir = downloads_dir()?;
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) => (s.to_string(), e.to_string()),
        None => (name.clone(), String::new()),
    };
    for n in 0..1000 {
        let candidate = if n == 0 {
            dir.join(&name)
        } else {
            dir.join(format!("{}-{}.{}", stem, n, ext))
        };
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&candidate) {
            Ok(mut f) => {
                use std::io::Write;
                f.write_all(content.as_bytes()).map_err(|e| e.to_string())?;
                return Ok(candidate.to_string_lossy().to_string());
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("could not find a free file name".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(agent: &'static str, sid: &str, ts: i64, cost: f64) -> UsageRecord {
        UsageRecord {
            agent,
            model: "claude-opus-5".into(),
            ts,
            input: 10,
            output: 5,
            cache_creation: 0,
            cache_read: 100,
            cost,
            session_id: sid.into(),
            title: String::new(),
            cwd: "/p".into(),
            path: "x".into(),
        }
    }

    #[test]
    fn groups_by_hour_and_session() {
        let recs = vec![
            rec("Claude Code", "a", 3600, 1.0),
            rec("Claude Code", "a", 3700, 2.0),
            rec("Claude Code", "b", 7300, 4.0),
            rec("Claude Code", "z", 0, 9.0),
        ];
        let f = build_facts(&recs);
        assert_eq!(f.rows.len(), 2);
        assert!((f.rows[0].8 - 3.0).abs() < 1e-9);
        assert_eq!(f.sessions.len(), 2);
        assert_eq!(f.sessions[0].ts, 7300);
    }

    #[test]
    fn merges_project_path_spellings() {
        let mut a = rec("Claude Code", "a", 3600, 1.0);
        a.cwd = r"I:\GhostESP2\Ghost_ESP".into();
        let mut b = rec("Codex CLI", "b", 3600, 1.0);
        b.cwd = "i:/ghostesp2/ghost_esp/".into();
        let mut c = rec("Claude Code", "c", 3600, 1.0);
        c.cwd = "I--GhostESP2-Ghost-ESP".into();
        let mut d = rec("Claude Code", "d", 3600, 1.0);
        d.cwd = "Z--unknown-slug".into();
        let f = build_facts(&[a, b, c, d]);
        let cwds: std::collections::HashSet<&str> = f.sessions.iter().map(|s| s.cwd.as_str()).collect();
        assert_eq!(cwds.len(), 2, "{:?}", cwds);
        assert!(cwds.contains("I:/GhostESP2/Ghost_ESP"));
        assert!(cwds.contains("Z--unknown-slug"));
    }

    #[test]
    fn filename_is_sanitized() {
        assert_eq!(sanitize_filename("../../evil/a b.csv").unwrap(), "ab.csv");
        assert!(sanitize_filename("x.exe").is_err());
        assert!(sanitize_filename(".csv").is_err());
    }
}
