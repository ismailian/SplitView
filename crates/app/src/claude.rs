//! Claude Code usage for the status bar: the context window of each running
//! session and the account's 5-hour / weekly limits.
//!
//! A `sv-claude` thread looks every 5 s at `~/.claude/sessions/<pid>.json`
//! (written by each running Claude Code), keeps the sessions whose process is
//! alive, and reads each one's context use from the tail of its transcript
//! (`projects/<cwd with non-alphanumerics as '-'>/<sessionId>.jsonl`, last
//! assistant message: input + cache creation + cache read tokens). While any
//! session runs, the limits come from `api.anthropic.com/api/oauth/usage` (the
//! endpoint Claude Code's `/usage` reads) every 2 min, with the access token
//! from `.credentials.json`. The UI is woken only when something changed.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::hub::{Hub, UiEvent};

const POLL: Duration = Duration::from_secs(5);
const LIMITS_EVERY: Duration = Duration::from_secs(120);
const TAIL: u64 = 512 * 1024;
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Session {
    pub cwd: String,
    pub name: String,
    /// `busy` / `idle` as Claude Code reports it.
    pub status: String,
    pub model: String,
    /// (tokens in context, context window).
    pub context: Option<(u64, u64)>,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limit {
    /// 0–100.
    pub percent: f32,
    /// Unix seconds.
    pub resets_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Usage {
    pub sessions: Vec<Session>,
    pub five_hour: Option<Limit>,
    pub seven_day: Option<Limit>,
    /// Why the limits are missing or stale.
    pub limits_error: Option<String>,
}

impl Usage {
    /// `--shot scratch`: sample numbers for the meters.
    pub fn demo(cwd: String) -> Self {
        let now = unix_now();
        Self {
            sessions: vec![Session {
                cwd,
                name: "splitview".into(),
                status: "busy".into(),
                model: "claude-opus-5-5".into(),
                context: Some((114_700, 200_000)),
                updated_at: 0,
            }],
            five_hour: Some(Limit {
                percent: 89.0,
                resets_at: Some(now + 22 * 60),
            }),
            seven_day: Some(Limit {
                percent: 48.0,
                resets_at: Some(now + 5 * 86_400),
            }),
            limits_error: None,
        }
    }
}

fn claude_dir() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".claude")))
}

pub fn watch(hub: &Arc<Hub>) {
    let hub = Arc::downgrade(hub);
    let _ = std::thread::Builder::new()
        .name("sv-claude".into())
        .spawn(move || run(hub));
}

fn run(hub: Weak<Hub>) {
    let Some(dir) = claude_dir() else {
        return;
    };
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .user_agent(format!("SplitView/{}", crate::theme::VERSION))
        .build()
        .into();
    let mut last = Usage::default();
    let mut limits: (Option<Limit>, Option<Limit>, Option<String>) = (None, None, None);
    let mut limits_at: Option<Instant> = None;
    let mut contexts: HashMap<PathBuf, (SystemTime, Option<(u64, String)>)> = HashMap::new();
    loop {
        let mut sessions = live_sessions(&dir.join("sessions"));
        let wide = settings_model(&dir).contains("[1m]");
        for (s, sid) in &mut sessions {
            let path = dir
                .join("projects")
                .join(project_dir_name(&s.cwd))
                .join(format!("{sid}.jsonl"));
            let Ok(mtime) = std::fs::metadata(&path).and_then(|m| m.modified()) else {
                continue;
            };
            let entry = contexts.entry(path.clone()).or_insert((UNIX_EPOCH, None));
            if entry.0 != mtime {
                *entry = (mtime, read_tail(&path).and_then(|t| last_context(&t)));
            }
            if let Some((used, model)) = &entry.1 {
                let window = if wide || *used > 200_000 {
                    1_000_000
                } else {
                    200_000
                };
                s.context = Some((*used, window));
                s.model = model.clone();
            }
        }
        if sessions.is_empty() {
            limits_at = None; // fetch again as soon as one starts
        } else if limits_at.is_none_or(|t| t.elapsed() >= LIMITS_EVERY) {
            limits_at = Some(Instant::now());
            match fetch_limits(&agent, &dir) {
                Ok((five, week)) => limits = (five, week, None),
                Err(e) => limits.2 = Some(e),
            }
        }
        let mut now = Usage {
            sessions: sessions.into_iter().map(|(s, _)| s).collect(),
            five_hour: limits.0,
            seven_day: limits.1,
            limits_error: limits.2.clone(),
        };
        now.sessions
            .sort_by_key(|s| std::cmp::Reverse(s.updated_at));
        if now != last {
            let Some(h) = hub.upgrade() else {
                return;
            };
            h.emit(UiEvent::Claude(Box::new(now.clone())));
            last = now;
        } else if hub.strong_count() == 0 {
            return;
        }
        std::thread::sleep(POLL);
    }
}

/// Running sessions and their ids.
fn live_sessions(dir: &Path) -> Vec<(Session, String)> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    rd.flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|s| parse_session(&s))
        .filter(|(pid, _, _)| alive(*pid))
        .map(|(_, s, id)| (s, id))
        .collect()
}

fn parse_session(json: &str) -> Option<(u32, Session, String)> {
    let v: Value = serde_json::from_str(json).ok()?;
    let pid = u32::try_from(v.get("pid")?.as_u64()?).ok()?;
    let id = v.get("sessionId")?.as_str()?.to_owned();
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
    Some((
        pid,
        Session {
            cwd: s("cwd"),
            name: s("name"),
            status: s("status"),
            updated_at: v.get("updatedAt").and_then(Value::as_u64).unwrap_or(0),
            ..Session::default()
        },
        id,
    ))
}

/// Claude Code's transcript folder name for a cwd.
pub fn project_dir_name(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn read_tail(path: &Path) -> Option<String> {
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(TAIL))).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// Tokens in context (and the model) after the last main-thread assistant
/// message in a transcript tail.
pub fn last_context(tail: &str) -> Option<(u64, String)> {
    tail.lines().rev().find_map(|line| {
        if !line.contains("\"usage\"") {
            return None;
        }
        let v: Value = serde_json::from_str(line).ok()?;
        if v.get("type")?.as_str()? != "assistant"
            || v.get("isSidechain").and_then(Value::as_bool) == Some(true)
        {
            return None;
        }
        let m = v.get("message")?;
        let u = m.get("usage")?;
        let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
        let used =
            n("input_tokens") + n("cache_creation_input_tokens") + n("cache_read_input_tokens");
        let model = m
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        (used > 0).then_some((used, model))
    })
}

fn settings_model(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("settings.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.get("model")?.as_str().map(str::to_owned))
        .unwrap_or_default()
}

type Limits = (Option<Limit>, Option<Limit>);

fn fetch_limits(agent: &ureq::Agent, dir: &Path) -> Result<Limits, String> {
    let creds = std::fs::read_to_string(dir.join(".credentials.json"))
        .map_err(|_| "no Claude Code credentials file (sign in with /login)".to_owned())?;
    let v: Value = serde_json::from_str(&creds).map_err(|e| e.to_string())?;
    let o = v
        .get("claudeAiOauth")
        .ok_or("not signed in with a Claude account")?;
    let token = o
        .get("accessToken")
        .and_then(Value::as_str)
        .ok_or("no access token")?;
    let expires = o
        .get("expiresAt")
        .and_then(Value::as_u64)
        .unwrap_or(u64::MAX);
    if expires < unix_now() as u64 * 1000 {
        return Err("token expired — Claude Code refreshes it on its next request".into());
    }
    let body = agent
        .get(USAGE_URL)
        .header("Authorization", &format!("Bearer {token}"))
        .header("anthropic-beta", "oauth-2025-04-20")
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    parse_limits(&body)
}

pub fn parse_limits(body: &str) -> Result<Limits, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let limit = |k: &str| {
        let l = v.get(k)?;
        Some(Limit {
            percent: l.get("utilization")?.as_f64()? as f32,
            resets_at: l
                .get("resets_at")
                .and_then(Value::as_str)
                .and_then(parse_utc),
        })
    };
    Ok((limit("five_hour"), limit("seven_day")))
}

pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// `2026-11-25T14:19:59.75+00:00` → unix seconds (offset honoured).
pub fn parse_utc(s: &str) -> Option<i64> {
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (h, mi, se) = (n(11..13)?, n(14..16)?, n(17..19)?);
    // Days from civil (Howard Hinnant).
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * (mo + if mo > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let tz = s.get(19..)?.find(['+', '-']).map(|i| 19 + i);
    let offset = match tz {
        Some(i) => {
            let sign = if &s[i..i + 1] == "-" { -1 } else { 1 };
            sign * (n(i + 1..i + 3)? * 3600 + n(i + 4..i + 6)? * 60)
        }
        None => 0,
    };
    Some(days * 86_400 + h * 3600 + mi * 60 + se - offset)
}

/// "2h 14m", "3d 5h", "45m".
pub fn until(secs: i64) -> String {
    let m = secs.max(0) / 60;
    match (m / 1440, (m / 60) % 24, m % 60) {
        (0, 0, m) => format!("{m}m"),
        (0, h, m) => format!("{h}h {m}m"),
        (d, h, _) => format!("{d}d {h}h"),
    }
}

#[cfg(windows)]
fn alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    // SAFETY: plain Win32 calls on a handle we open and close here.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return false;
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(h, &mut code) != 0;
        CloseHandle(h);
        ok && code == STILL_ACTIVE as u32
    }
}

#[cfg(target_os = "linux")]
fn alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

// shortcut: no cheap liveness check without libc here; a crashed session's
// file shows until Claude Code cleans it up. Add `kill(pid, 0)` if it bites.
#[cfg(not(any(windows, target_os = "linux")))]
fn alive(_pid: u32) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcript_folder_names() {
        assert_eq!(
            project_dir_name(r"D:\Projects\SplitView"),
            "D--Projects-SplitView"
        );
        assert_eq!(project_dir_name("/home/u/my.app"), "-home-u-my-app");
    }

    #[test]
    fn context_is_the_last_main_thread_assistant_turn() {
        let tail = [
            r#"{"type":"assistant","message":{"model":"m1","usage":{"input_tokens":5,"cache_creation_input_tokens":10,"cache_read_input_tokens":100}}}"#,
            r#"{"type":"assistant","isSidechain":true,"message":{"model":"sub","usage":{"input_tokens":9}}}"#,
            r#"{"type":"user","message":{"content":"usage"}}"#,
            r#"{"type":"assistant","message":{"model":"m2","usage":{"input_tokens":1,"cache_creation_input_tokens":2,"cache_read_input_tokens":3}}}"#,
            r#"{"partial line cut by the tail "usage""#,
        ]
        .join("\n");
        assert_eq!(last_context(&tail), Some((6, "m2".into())));
        assert_eq!(
            last_context(&tail[..tail.find("\n{\"type\":\"assistant\",\"isS").unwrap()]),
            Some((115, "m1".into()))
        );
        assert_eq!(last_context(""), None);
    }

    #[test]
    fn limits_and_times() {
        let (five, week) = parse_limits(
            r#"{"five_hour":{"utilization":89.0,"resets_at":"2026-11-25T14:19:59.754892+00:00"},"seven_day":null}"#,
        )
        .unwrap();
        let five = five.unwrap();
        assert_eq!(five.percent, 89.0);
        assert_eq!(five.resets_at, Some(1_795_616_399));
        assert_eq!(week, None);
        assert_eq!(parse_utc("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_utc("1970-01-01T01:00:00+01:00"), Some(0));
        assert_eq!(parse_utc("2000-03-01T00:00:00Z"), Some(951_868_800));
        assert_eq!(parse_utc("garbage"), None);
        assert_eq!(until(45 * 60), "45m");
        assert_eq!(until(2 * 3600 + 14 * 60), "2h 14m");
        assert_eq!(until(3 * 86400 + 5 * 3600), "3d 5h");
    }

    #[test]
    fn session_files() {
        let (pid, s, id) = parse_session(
            r#"{"pid":11184,"sessionId":"abc","cwd":"D:\\P","name":"n","status":"busy","updatedAt":7}"#,
        )
        .unwrap();
        assert_eq!(
            (pid, id.as_str(), s.cwd.as_str(), s.updated_at),
            (11184, "abc", r"D:\P", 7)
        );
        assert!(parse_session(r#"{"peerToken":"x"}"#).is_none());
    }
}
