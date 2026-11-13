//! Scheduled prompts — the reference's CronCreate/CronList/CronDelete tools.
//! Crons persist under `<data_dir>/crons/<id>.json` with a precomputed
//! `next_fire_ms`; the REPL tick enqueues due prompts like the reference's
//! scheduler.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::default_data_dir;
use clawedcode_tools::ToolResult;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CronRecord {
    pub id: String,
    pub cron: String,
    pub prompt: String,
    pub recurring: bool,
    pub durable: bool,
    pub created_ms: u64,
    pub next_fire_ms: u64,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn crons_dir() -> Result<PathBuf, String> {
    std::env::var_os("CLAWEDCODE_DATA_DIR")
        .map(PathBuf::from)
        .or_else(default_data_dir)
        .map(|dir| dir.join("crons"))
        .ok_or_else(|| "No data directory available".to_string())
}

pub fn list_crons() -> Result<Vec<CronRecord>, String> {
    let dir = crons_dir()?;
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(format!("Failed to read {}: {err}", dir.display())),
    };
    let mut crons = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Ok(raw) = fs::read_to_string(&path) {
            if let Ok(cron) = serde_json::from_str::<CronRecord>(&raw) {
                crons.push(cron);
            }
        }
    }
    crons.sort_by(|a, b| a.next_fire_ms.cmp(&b.next_fire_ms));
    Ok(crons)
}

fn save_cron(cron: &CronRecord) -> Result<(), String> {
    let dir = crons_dir()?;
    fs::create_dir_all(&dir).map_err(|e| format!("Failed to create {}: {e}", dir.display()))?;
    let path = dir.join(format!("{}.json", cron.id));
    fs::write(
        &path,
        serde_json::to_string_pretty(cron).unwrap_or_default(),
    )
    .map_err(|e| format!("Failed to write {}: {e}", path.display()))
}

pub fn delete_cron(id: &str) -> Result<bool, String> {
    let path = crons_dir()?.join(format!("{}.json", id));
    match fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(format!("Failed to delete cron {id}: {err}")),
    }
}

// --- 5-field cron matching: `M H DoM Mon DoW` ---

fn field_matches(field: &str, value: i64, max: i64) -> bool {
    for part in field.split(',') {
        let part = part.trim();
        if part == "*" {
            return true;
        }
        if let Some(step) = part.strip_prefix("*/") {
            if let Ok(n) = step.parse::<i64>() {
                if n > 0 && value % n == 0 {
                    return true;
                }
            }
            continue;
        }
        if let Some((lo, hi)) = part.split_once('-') {
            if let (Ok(lo), Ok(hi)) = (lo.parse::<i64>(), hi.parse::<i64>()) {
                if value >= lo && value <= hi {
                    return true;
                }
            }
            continue;
        }
        if let Ok(v) = part.parse::<i64>() {
            if v == value || (v == 0 && value == max) {
                return true;
            }
        }
    }
    false
}

/// Does `cron` match the given local time components?
fn cron_matches(cron: &str, minute: i64, hour: i64, dom: i64, month: i64, dow: i64) -> bool {
    let fields: Vec<&str> = cron.split_whitespace().collect();
    if fields.len() != 5 {
        return false;
    }
    field_matches(fields[0], minute, 59)
        && field_matches(fields[1], hour, 23)
        && field_matches(fields[2], dom, 31)
        && field_matches(fields[3], month, 12)
        && field_matches(fields[4], dow, 7)
}

/// Scan forward minute-by-minute for the next fire time (up to ~1 year).
/// Uses UTC components — close enough to local time when the process TZ is
/// the local zone for the common case.
pub fn next_fire_ms(cron: &str, from_ms: u64) -> Option<u64> {
    // Validate the expression shape first.
    if cron.split_whitespace().count() != 5 {
        return None;
    }
    let mut t = (from_ms / 60_000 + 1) * 60_000;
    let end = from_ms + 366 * 24 * 60 * 60_000;
    while t < end {
        let secs = (t / 1000) as i64;
        // Days since epoch → civil date (Howard Hinnant's algorithm).
        let days = secs.div_euclid(86_400);
        let secs_of_day = secs.rem_euclid(86_400);
        let (hour, minute) = (secs_of_day / 3600, (secs_of_day % 3600) / 60);
        let z = days + 719_468;
        let _era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let dom = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        // Jan 1 1970 was a Thursday.
        let dow = (days % 7 + 4) % 7;
        if cron_matches(cron, minute, hour, dom, month, dow) {
            return Some(t);
        }
        t += 60_000;
    }
    None
}

/// Crons due at `now_ms`. Callers enqueue `record.prompt` and then call
/// `advance_cron` so it doesn't refire.
pub fn due_crons(now: u64) -> Vec<CronRecord> {
    list_crons()
        .unwrap_or_default()
        .into_iter()
        .filter(|c| c.next_fire_ms <= now)
        .collect()
}

/// After a cron fires: one-shots are deleted, recurring crons advance.
pub fn advance_cron(record: &CronRecord) {
    if !record.recurring {
        let _ = delete_cron(&record.id);
        return;
    }
    let mut next = record.clone();
    next.next_fire_ms =
        next_fire_ms(&record.cron, now_ms()).unwrap_or(record.next_fire_ms + 24 * 60 * 60_000);
    let _ = save_cron(&next);
}

// --- tool execution ---

pub fn execute_cron_tool(
    tool_name: &str,
    input: Value,
    _session: &crate::session::Session,
) -> Result<Option<ToolResult>, String> {
    match tool_name {
        "CronCreate" => {
            let Some(expr) = input.get("cron").and_then(Value::as_str) else {
                return Ok(Some(ToolResult {
                    content: "Missing 'cron' parameter".into(),
                    is_error: true,
                    new_cwd: None,
                }));
            };
            let Some(prompt) = input.get("prompt").and_then(Value::as_str) else {
                return Ok(Some(ToolResult {
                    content: "Missing 'prompt' parameter".into(),
                    is_error: true,
                    new_cwd: None,
                }));
            };
            if expr.split_whitespace().count() != 5 {
                return Ok(Some(ToolResult {
                    content: format!(
                        "Invalid cron expression '{expr}'. Expected 5 fields: M H DoM Mon DoW."
                    ),
                    is_error: true,
                    new_cwd: None,
                }));
            }
            let Some(next) = next_fire_ms(expr, now_ms()) else {
                return Ok(Some(ToolResult {
                    content: format!(
                        "Cron expression '{expr}' does not match any calendar date in the next year."
                    ),
                    is_error: true,
                    new_cwd: None,
                }));
            };
            let record = CronRecord {
                id: uuid::Uuid::new_v4().to_string()[..8].to_string(),
                cron: expr.to_string(),
                prompt: prompt.to_string(),
                recurring: input
                    .get("recurring")
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
                durable: input
                    .get("durable")
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
                created_ms: now_ms(),
                next_fire_ms: next,
            };
            save_cron(&record)?;
            Ok(Some(ToolResult {
                content: format!(
                    "Created cron {} ('{}'): fires next at epoch ms {}",
                    record.id, record.cron, record.next_fire_ms
                ),
                is_error: false,
                new_cwd: None,
            }))
        }
        "CronList" => {
            let crons = list_crons()?;
            let lines: Vec<String> = crons
                .iter()
                .map(|c| {
                    format!(
                        "{} '{}' → {:?} (next: ms {}{})",
                        c.id,
                        c.cron,
                        c.prompt,
                        c.next_fire_ms,
                        if c.recurring { "" } else { ", once" }
                    )
                })
                .collect();
            Ok(Some(ToolResult {
                content: if lines.is_empty() {
                    "No scheduled crons.".to_string()
                } else {
                    lines.join("\n")
                },
                is_error: false,
                new_cwd: None,
            }))
        }
        "CronDelete" => {
            let Some(id) = input.get("id").and_then(Value::as_str) else {
                return Ok(Some(ToolResult {
                    content: "Missing 'id' parameter".into(),
                    is_error: true,
                    new_cwd: None,
                }));
            };
            Ok(Some(ToolResult {
                content: if delete_cron(id)? {
                    format!("Deleted cron {id}")
                } else {
                    format!("No cron with id {id}")
                },
                is_error: false,
                new_cwd: None,
            }))
        }
        _ => Ok(None),
    }
}
