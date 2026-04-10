use comfy_table::{Attribute, Cell, Color, Table};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Deserialize)]
struct Message {
    r#type: String,
    message: Option<ApiMessage>,
}

#[derive(Deserialize)]
struct ApiMessage {
    id: Option<String>,
    model: Option<String>,
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Usage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
}

#[derive(Clone, Copy)]
enum Period {
    Total,
    Week,
    Month,
}

impl Period {
    fn label(self) -> &'static str {
        match self {
            Period::Total => "all time",
            Period::Week => "past 7 days",
            Period::Month => "past 30 days",
        }
    }

    fn cutoff_secs(self) -> Option<u64> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        match self {
            Period::Total => None,
            Period::Week => Some(now - 7 * 86400),
            Period::Month => Some(now - 30 * 86400),
        }
    }
}

/// Per-million-token pricing: (input, output, cache_write, cache_read)
fn model_pricing(model: &str) -> (f64, f64, f64, f64) {
    match model {
        m if m.contains("haiku-4-5") => (1.00, 5.00, 1.25, 0.10),
        m if m.contains("haiku") => (0.80, 4.00, 1.00, 0.08),
        m if m.contains("sonnet") => (3.00, 15.00, 3.75, 0.30),
        // Opus 4.5+ ($5/$25), Opus 4/4.1 ($15/$75)
        m if m.contains("opus-4-5") || m.contains("opus-4-6") => (5.00, 25.00, 6.25, 0.50),
        _ => (15.00, 75.00, 18.75, 1.50),
    }
}

fn usage_cost(usage: &Usage, model: &str) -> f64 {
    let (inp, out, _, _) = model_pricing(model);
    let m = 1_000_000.0;
    usage.input_tokens as f64 * inp / m + usage.output_tokens as f64 * out / m
}

#[derive(Default)]
struct ProjectStats {
    input_tokens: u64,
    output_tokens: u64,
    cache_write_tokens: u64,
    cache_read_tokens: u64,
    cost: f64,
    sessions: u64,
}

impl ProjectStats {
    fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }

    fn accumulate(&mut self, other: &ProjectStats) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_write_tokens += other.cache_write_tokens;
        self.cache_read_tokens += other.cache_read_tokens;
        self.cost += other.cost;
        self.sessions += other.sessions;
    }
}

/// Build a map from encoded dir name to display name.
/// The encoding is: absolute path with `/` replaced by `-`.
/// We use .claude.json project keys as the source of truth for decoding.
fn build_name_map() -> HashMap<String, String> {
    let config_path = dirs::home_dir().unwrap().join(".claude.json");
    let Ok(content) = fs::read_to_string(&config_path) else {
        return HashMap::new();
    };
    let Ok(config) = serde_json::from_str::<serde_json::Value>(&content) else {
        return HashMap::new();
    };
    let Some(projects) = config.get("projects").and_then(|p| p.as_object()) else {
        return HashMap::new();
    };

    let mut map = HashMap::new();
    for real_path in projects.keys() {
        let encoded = real_path.replace('/', "-");
        let display = shorten_path(real_path);
        map.insert(encoded, display);
    }
    // Archive-only projects not in .claude.json (deleted but have history)
    add_archive_names(&mut map);
    map
}

fn add_archive_names(map: &mut HashMap<String, String>) {
    let extras = [
        "/syncthing/Sync/Projects/globalcomix/gc-sentry",
        "/syncthing/Sync/Projects/globalcomix/gc-phpstan-baseline-fixes-5",
        "/syncthing/Sync/Projects/globalcomix/gc-api-v2",
        "/syncthing/Sync/Projects/claude/claude-sessions-blinc",
        "/syncthing/Sync/Projects/claude/claude-sessions-tauri",
        "/syncthing/Sync/Projects/claude/claude-architect-viewer",
        "/syncthing/Sync/Projects/claude/agent-bus",
    ];
    for path in extras {
        let encoded = path.replace('/', "-");
        map.insert(encoded, shorten_path(path));
    }
}

fn shorten_path(path: &str) -> String {
    // Strip common prefixes for readability
    for prefix in [
        "/syncthing/Sync/Projects/",
        "/home/osso/Projects/",
        "/home/osso/Repos/",
        "/home/osso/",
        "/syncthing/Sync/",
    ] {
        if let Some(rest) = path.strip_prefix(prefix) {
            let base = prefix
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or("");
            if ["Projects", "Repos"].contains(&base) {
                return rest.to_string();
            }
            return format!("{base}/{rest}");
        }
    }
    path.to_string()
}

fn is_orchestrator(dir_name: &str) -> bool {
    dir_name.starts_with("-tmp")
        || dir_name.starts_with("-work")
        || dir_name == "subagents"
        || dir_name.contains("-claude-sessions-worktrees-")
        || dir_name.contains("-orch-dev-")
}

const ORCHESTRATOR_LABEL: &str = "[orchestrator]";

fn project_name_from_dir(dir_name: &str, name_map: &HashMap<String, String>) -> String {
    if is_orchestrator(dir_name) {
        ORCHESTRATOR_LABEL.to_string()
    } else {
        resolve_name(dir_name, name_map)
    }
}

/// Renamed/merged projects: map old encoded prefix to new one
const RENAMES: &[(&str, &str)] = &[
    (
        "-syncthing-Sync-Projects-wow-wow-engine",
        "-syncthing-Sync-Projects-world-of-osso-game-engine",
    ),
    (
        "-syncthing-Sync-Projects-wow-game-engine",
        "-syncthing-Sync-Projects-world-of-osso-game-engine",
    ),
    (
        "-syncthing-Sync-Projects-wow-game-launcher",
        "-syncthing-Sync-Projects-world-of-osso-game-launcher",
    ),
    (
        "-syncthing-Sync-Projects-wow-website",
        "-syncthing-Sync-Projects-world-of-osso-website",
    ),
];

fn apply_renames(dir_name: &str) -> &str {
    for (old, new) in RENAMES {
        if dir_name == *old || dir_name.starts_with(&format!("{old}--claude-worktrees-")) {
            return new;
        }
    }
    dir_name
}

fn resolve_name(dir_name: &str, name_map: &HashMap<String, String>) -> String {
    let dir_name = apply_renames(dir_name);
    if let Some(display) = name_map.get(dir_name) {
        return display.clone();
    }
    if let Some(pos) = dir_name.find("--claude-worktrees-") {
        let parent_encoded = &dir_name[..pos];
        return name_map
            .get(parent_encoded)
            .cloned()
            .unwrap_or_else(|| decode_fallback(parent_encoded));
    }
    decode_fallback(dir_name)
}

fn decode_fallback(encoded: &str) -> String {
    let path = format!("/{}", encoded.trim_start_matches('-').replace('-', "/"));
    // Clean up double slashes from dot-directories (e.g., .claude → //claude)
    let path = path.replace("//", "/.");
    shorten_path(&path)
}

fn collect_jsonl_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return files;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "jsonl") {
            files.push(path);
        } else if path.is_dir() {
            files.extend(collect_jsonl_files(&path));
        }
    }
    files
}

/// Check if a file was modified on or after the cutoff timestamp.
fn file_in_range(path: &Path, cutoff_secs: Option<u64>) -> bool {
    let Some(cutoff) = cutoff_secs else {
        return true;
    };
    let Ok(meta) = path.metadata() else {
        return false;
    };
    let Ok(mtime) = meta.modified() else {
        return false;
    };
    let Ok(mtime_secs) = mtime.duration_since(UNIX_EPOCH) else {
        return false;
    };
    mtime_secs.as_secs() >= cutoff
}

fn process_zst(path: &Path, stats: &mut ProjectStats) {
    let Ok(file) = fs::File::open(path) else {
        return;
    };
    let Ok(decoder) = zstd::Decoder::new(file) else {
        return;
    };
    let mut content = String::new();
    if std::io::BufReader::new(decoder)
        .read_to_string(&mut content)
        .is_err()
    {
        return;
    }
    process_content(&content, stats);
}

fn process_jsonl(path: &Path, stats: &mut ProjectStats) {
    let Ok(content) = fs::read_to_string(path) else {
        return;
    };
    process_content(&content, stats);
}

fn process_content(content: &str, stats: &mut ProjectStats) {
    // Collect last occurrence of each message ID (snapshots repeat with same ID)
    let mut by_id: HashMap<String, (Usage, String)> = HashMap::new();
    let mut anonymous: Vec<(Usage, String)> = Vec::new();

    for line in content.lines() {
        let Ok(msg) = serde_json::from_str::<Message>(line) else {
            continue;
        };
        if msg.r#type != "assistant" {
            continue;
        }
        let Some(api_msg) = msg.message else {
            continue;
        };
        let Some(usage) = api_msg.usage else {
            continue;
        };
        let model = api_msg.model.as_deref().unwrap_or("opus").to_string();
        match api_msg.id {
            Some(id) => {
                by_id.insert(id, (usage, model));
            }
            None => anonymous.push((usage, model)),
        }
    }

    let all_msgs = by_id.into_values().chain(anonymous);
    let mut found = false;
    for (usage, model) in all_msgs {
        stats.input_tokens += usage.input_tokens;
        stats.output_tokens += usage.output_tokens;
        stats.cache_write_tokens += usage.cache_creation_input_tokens;
        stats.cache_read_tokens += usage.cache_read_input_tokens;
        stats.cost += usage_cost(&usage, &model);
        found = true;
    }
    if found {
        stats.sessions += 1;
    }
}

fn format_tokens(n: u64) -> String {
    if n >= 1_000_000_000 {
        format!("{:.1}B", n as f64 / 1_000_000_000.0)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn cost_color(cost: f64) -> Color {
    if cost > 100.0 {
        Color::Red
    } else if cost > 10.0 {
        Color::Yellow
    } else {
        Color::Green
    }
}

fn stats_row(rank: &str, name: &str, stats: &ProjectStats, bold_all: bool) -> Vec<Cell> {
    let attr = |c: Cell| {
        if bold_all {
            c.add_attribute(Attribute::Bold)
        } else {
            c
        }
    };
    vec![
        attr(Cell::new(rank)),
        attr(Cell::new(name)),
        attr(Cell::new(stats.sessions)),
        attr(Cell::new(format_tokens(stats.input_tokens))),
        attr(Cell::new(format_tokens(stats.output_tokens))),
        attr(Cell::new(format_tokens(stats.total_tokens())).add_attribute(Attribute::Bold)),
        attr(Cell::new(format!("${:.2}", stats.cost)).fg(cost_color(stats.cost))),
    ]
}

fn stats_row_dimmed(rank: &str, name: &str, stats: &ProjectStats) -> Vec<Cell> {
    let dim = |c: Cell| c.add_attribute(Attribute::Dim);
    vec![
        dim(Cell::new(rank)),
        dim(Cell::new(name)),
        dim(Cell::new(stats.sessions)),
        dim(Cell::new(format_tokens(stats.input_tokens))),
        dim(Cell::new(format_tokens(stats.output_tokens))),
        dim(Cell::new(format_tokens(stats.total_tokens()))),
        dim(Cell::new(format!("${:.2}", stats.cost))),
    ]
}

fn print_leaderboard(sorted: &[(String, ProjectStats)], period: Period) {
    println!("Claude Code token usage ({})\n", period.label());

    let mut table = Table::new();
    table.set_header(
        ["#", "Project", "Sessions", "Input", "Output", "Total", "Cost"]
        .map(|h| Cell::new(h).add_attribute(Attribute::Bold)),
    );

    let mut grand_total = ProjectStats::default();
    for (i, (name, stats)) in sorted.iter().enumerate() {
        let row = if name == ORCHESTRATOR_LABEL {
            stats_row_dimmed(&(i + 1).to_string(), name, stats)
        } else {
            stats_row(&(i + 1).to_string(), name, stats, false)
        };
        table.add_row(row);
        grand_total.accumulate(stats);
    }
    table.add_row(stats_row("", "TOTAL", &grand_total, true));

    println!("{table}");
}

fn gather_stats(period: Period) -> Vec<(String, ProjectStats)> {
    let projects_dir = dirs::home_dir()
        .expect("no home dir")
        .join(".claude/projects");

    let Ok(entries) = fs::read_dir(&projects_dir) else {
        eprintln!("Cannot read {}", projects_dir.display());
        std::process::exit(1);
    };

    let name_map = build_name_map();
    let cutoff_secs = period.cutoff_secs();
    let mut all_stats: HashMap<String, ProjectStats> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::new();

    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let dir_name = entry.file_name().to_string_lossy().to_string();
        if dir_name == "memory" {
            continue;
        }
        let project_name = project_name_from_dir(&dir_name, &name_map);
        let stats = all_stats.entry(project_name).or_default();
        for jsonl in collect_jsonl_files(&entry.path()) {
            if !file_in_range(&jsonl, cutoff_secs) {
                continue;
            }
            let id = jsonl.file_stem().unwrap().to_string_lossy().to_string();
            seen.insert(id);
            process_jsonl(&jsonl, stats);
        }
    }

    scan_archive(&name_map, &mut all_stats, &mut seen, cutoff_secs);

    let all_stats = merge_subdirs(all_stats);

    let mut sorted: Vec<(String, ProjectStats)> = all_stats
        .into_iter()
        .filter(|(_, s)| s.sessions > 0)
        .collect();
    sorted.sort_by(|a, b| b.1.total_tokens().cmp(&a.1.total_tokens()));
    sorted
}

/// Merge subdirectory entries into their closest parent project.
/// e.g., "globalcomix/gc/.git" → "globalcomix/gc" (not "globalcomix")
fn merge_subdirs(mut stats: HashMap<String, ProjectStats>) -> HashMap<String, ProjectStats> {
    let known: Vec<String> = stats.keys().cloned().collect();
    let mut merges: Vec<(String, String)> = Vec::new();

    for name in &known {
        // Find the longest (most specific) parent that is a prefix
        let best_parent = known
            .iter()
            .filter(|p| *p != name && name.starts_with(&format!("{p}/")))
            .max_by_key(|p| p.len());

        if let Some(parent) = best_parent {
            // Only merge if this entry has very few sessions (subdirectory noise)
            if let Some(s) = stats.get(name) {
                if s.sessions <= 5 {
                    merges.push((name.clone(), parent.clone()));
                }
            }
        }
    }

    for (child, parent) in merges {
        if let Some(child_stats) = stats.remove(&child) {
            stats.entry(parent).or_default().accumulate(&child_stats);
        }
    }

    stats
}

/// Extract project encoded name from archive filename.
/// Format: `{project-encoded}_{session-id}.jsonl.zst`
/// Also handles subagent files: `{project-encoded}_{agent-id}.jsonl.zst`
fn archive_project_key(filename: &str) -> Option<&str> {
    let base = filename.strip_suffix(".jsonl.zst")?;
    // Find last `_` that precedes a UUID or agent ID
    let pos = base.rfind('_')?;
    Some(&base[..pos])
}

fn archive_session_id(filename: &str) -> Option<&str> {
    let base = filename.strip_suffix(".jsonl.zst")?;
    let pos = base.rfind('_')?;
    Some(&base[pos + 1..])
}

fn scan_archive(
    name_map: &HashMap<String, String>,
    all_stats: &mut HashMap<String, ProjectStats>,
    seen: &mut HashSet<String>,
    cutoff_secs: Option<u64>,
) {
    let archive_dir = dirs::home_dir().unwrap().join(".claude/archive");
    let Ok(entries) = fs::read_dir(&archive_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let filename = entry.file_name().to_string_lossy().to_string();
        if !filename.ends_with(".jsonl.zst") {
            continue;
        }
        if !file_in_range(&entry.path(), cutoff_secs) {
            continue;
        }
        let Some(session_id) = archive_session_id(&filename) else {
            continue;
        };
        if !seen.insert(session_id.to_string()) {
            continue;
        }
        let Some(encoded) = archive_project_key(&filename) else {
            continue;
        };
        let project_name = project_name_from_dir(encoded, name_map);
        let stats = all_stats.entry(project_name).or_default();
        process_zst(&entry.path(), stats);
    }
}

fn parse_period() -> Period {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("week" | "w" | "7d") => Period::Week,
        Some("month" | "m" | "30d") => Period::Month,
        Some("total" | "all" | "a") | None => Period::Total,
        Some(other) => {
            eprintln!("Unknown period: {other}");
            eprintln!("Usage: claude-tokens [week|month|total]");
            std::process::exit(1);
        }
    }
}

fn main() {
    let period = parse_period();
    let stats = gather_stats(period);
    print_leaderboard(&stats, period);
}
