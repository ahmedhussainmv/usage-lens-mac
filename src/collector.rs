use crate::store;
use anyhow::{Context, Result};
use chrono::DateTime;
use memchr::memmem;
use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    thread,
};
use walkdir::WalkDir;

/// Ask macOS to schedule background collection below interactive work.
#[cfg(target_os = "macos")]
pub fn background_priority() {
    extern "C" {
        fn pthread_set_qos_class_self_np(class: u32, relative_priority: i32) -> i32;
    }
    // QOS_CLASS_BACKGROUND from the macOS SDK. Failure leaves the OS default priority.
    unsafe {
        let _ = pthread_set_qos_class_self_np(0x09, 0);
    }
}
#[cfg(not(target_os = "macos"))]
pub fn background_priority() {}

#[derive(Clone, Debug)]
pub struct Roots {
    pub codex: PathBuf,
    pub archive: PathBuf,
    pub claude: PathBuf,
}
impl Roots {
    pub fn for_home(home: &Path) -> Self {
        Self {
            codex: home.join(".codex/sessions"),
            archive: home.join(".codex/archived_sessions"),
            claude: home.join(".claude/projects"),
        }
    }
}

#[derive(Default, Clone, Debug)]
pub struct Scan {
    pub codex_present: bool,
    pub archive_present: bool,
    pub claude_present: bool,
    pub files_read: u64,
    pub bytes_read: u64,
    pub malformed: u64,
    pub partial: u64,
    pub errors: u64,
}

#[derive(Default, Clone, Serialize, Deserialize)]
struct State {
    thread: String,
    turn: String,
    model: String,
    effort: String,
    project: String,
    previous: Option<[i64; 5]>,
}

#[derive(Clone)]
struct Usage {
    values: [i64; 5],
    quality: i64,
    incomplete: bool,
}
struct Event {
    id: String,
    source: &'static str,
    kind: &'static str,
    thread: String,
    turn: String,
    timestamp: Option<i64>,
    model: String,
    effort: String,
    project: String,
    usage: Usage,
}

fn hash(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for s in parts {
        hasher.update((s.len() as u64).to_be_bytes());
        hasher.update(s.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}
fn string<'a>(v: &'a Value, name: &str) -> Option<&'a str> {
    v.get(name)?.as_str().filter(|s| !s.is_empty())
}
fn number(v: &Value, name: &str) -> Option<i64> {
    let x = v.get(name)?.as_i64()?;
    (0..=i64::MAX / 16).contains(&x).then_some(x)
}
fn timestamp(v: &Value) -> Option<i64> {
    DateTime::parse_from_rfc3339(string(v, "timestamp")?)
        .ok()
        .map(|d| d.timestamp())
}
fn project(v: &str) -> String {
    Path::new(v)
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("Unknown project")
        .to_owned()
}
fn display(s: &str, default: &str) -> String {
    if s.is_empty() {
        default.into()
    } else {
        s.to_owned()
    }
}

// Codex input includes cache reads/writes; Claude's input excludes both.
fn usage(v: &Value, source: &str) -> Usage {
    let input = number(v, "input_tokens");
    let read = number(
        v,
        if source == "claude" {
            "cache_read_input_tokens"
        } else {
            "cached_input_tokens"
        },
    );
    let write = number(
        v,
        if source == "claude" {
            "cache_creation_input_tokens"
        } else {
            "cache_write_input_tokens"
        },
    );
    let output = number(v, "output_tokens");
    let reasoning = if source == "claude" {
        v.get("output_tokens_details")
            .and_then(|d| number(d, "thinking_tokens"))
    } else {
        number(v, "reasoning_output_tokens")
    };
    let quality = [input, read, write, output]
        .iter()
        .filter(|v| v.is_some())
        .count() as i64;
    let mut incomplete = quality < 4;
    let (input, read, write, output) = (
        input.unwrap_or(0),
        read.unwrap_or(0),
        write.unwrap_or(0),
        output.unwrap_or(0),
    );
    let uncached = if source == "codex" {
        if read.saturating_add(write) > input {
            incomplete = true;
        }
        input.saturating_sub(read.saturating_add(write))
    } else {
        input
    };
    Usage {
        values: [uncached, read, write, output, reasoning.unwrap_or(0)],
        quality,
        incomplete,
    }
}
fn raw(v: &Value) -> [i64; 5] {
    [
        "input_tokens",
        "cached_input_tokens",
        "cache_write_input_tokens",
        "output_tokens",
        "reasoning_output_tokens",
    ]
    .map(|k| number(v, k).unwrap_or(0))
}
fn event_from(
    state: &State,
    id: String,
    source: &'static str,
    kind: &'static str,
    ts: Option<i64>,
    u: Usage,
) -> Event {
    Event {
        id,
        source,
        kind,
        thread: state.thread.clone(),
        turn: state.turn.clone(),
        timestamp: ts,
        model: display(&state.model, "Unknown model"),
        effort: display(&state.effort, "Unknown effort"),
        project: display(&state.project, "Unknown project"),
        usage: u,
    }
}
fn insert(tx: &Transaction<'_>, e: &Event) -> Result<()> {
    let v = e.usage.values;
    // Keep one observed Claude snapshot, never independent maxima that invent a record.
    tx.execute("INSERT INTO events VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)
        ON CONFLICT(id) DO UPDATE SET ts=CASE WHEN events.ts IS NULL THEN excluded.ts WHEN excluded.ts IS NULL THEN events.ts ELSE MIN(events.ts,excluded.ts) END,
        model=excluded.model,effort=excluded.effort,project=excluded.project,uncached=excluded.uncached,
        cache_read=excluded.cache_read,cache_write=excluded.cache_write,output=excluded.output,reasoning=excluded.reasoning,
        incomplete=excluded.incomplete,quality=excluded.quality
        WHERE excluded.kind='claude' AND (excluded.quality>events.quality OR
          (excluded.quality=events.quality AND excluded.uncached+excluded.cache_read+excluded.cache_write+excluded.output > events.uncached+events.cache_read+events.cache_write+events.output))",
        params![e.id,e.source,e.kind,e.thread,e.turn,e.timestamp,e.model,e.effort,e.project,v[0],v[1],v[2],v[3],v[4],e.usage.incomplete as i64,e.usage.quality])?;
    Ok(())
}
fn issue(
    tx: &Transaction<'_>,
    source: &str,
    id: &str,
    ts: Option<i64>,
    reason: &str,
) -> Result<()> {
    tx.execute(
        "INSERT OR IGNORE INTO ingest_issues VALUES (?1,?2,?3,?4)",
        params![id, source, ts, reason],
    )?;
    Ok(())
}

fn codex(
    tx: &Transaction<'_>,
    state: &mut State,
    entry: &Value,
    line: &[u8],
    filename: &str,
) -> Result<()> {
    let kind = string(entry, "type").unwrap_or("");
    let p = entry
        .get("payload")
        .filter(|p| p.is_object())
        .unwrap_or(&Value::Null);
    if kind == "session_meta" {
        state.thread = hash(&[
            "codex-thread",
            string(p, "id")
                .or_else(|| string(p, "session_id"))
                .unwrap_or(filename),
        ]);
        if let Some(cwd) = string(p, "cwd") {
            state.project = project(cwd);
        }
        return Ok(());
    }
    if state.thread.is_empty() {
        state.thread = hash(&["codex-thread", string(p, "thread_id").unwrap_or(filename)]);
    }
    if kind == "turn_context" {
        if let Some(turn) = string(p, "turn_id") {
            state.turn = hash(&["turn", &state.thread, turn]);
        }
        state.model = string(p, "model").unwrap_or("").into();
        state.effort = string(p, "effort")
            .or_else(|| string(p, "model_reasoning_effort"))
            .unwrap_or("")
            .into();
        if let Some(cwd) = string(p, "cwd") {
            state.project = project(cwd);
        }
    } else if kind == "event_msg" && string(p, "type") == Some("task_started") {
        let id = string(p, "turn_id")
            .or_else(|| string(entry, "timestamp"))
            .unwrap_or("unknown");
        state.turn = hash(&["turn", &state.thread, id]);
        state.model.clear();
        state.effort.clear();
    } else if kind == "token_usage_record" {
        let mut current = state.clone();
        if let Some(thread) = string(p, "thread_id") {
            current.thread = hash(&["codex-thread", thread]);
        }
        if let Some(turn) = string(p, "turn_id") {
            current.turn = hash(&["turn", &current.thread, turn]);
        }
        if current.turn.is_empty() {
            current.turn = hash(&["turn", &current.thread, "unknown"]);
        }
        let response = string(p, "response_id")
            .map(str::to_owned)
            .unwrap_or_else(|| hash(&[&String::from_utf8_lossy(line)]));
        let id = hash(&["codex-response", &current.thread, &current.turn, &response]);
        let mut u = usage(p.get("usage").unwrap_or(&Value::Null), "codex");
        if string(p, "response_id").is_none() {
            u.incomplete = true;
        }
        insert(
            tx,
            &event_from(&current, id, "codex", "structured", timestamp(entry), u),
        )?;
    } else if kind == "event_msg" && string(p, "type") == Some("token_count") {
        let info = p.get("info").unwrap_or(&Value::Null);
        let Some(total) = info.get("total_token_usage").filter(|v| v.is_object()) else {
            return Ok(());
        };
        let current = raw(total);
        let last = info.get("last_token_usage").filter(|v| v.is_object());
        if (state.previous.is_none() || state.previous.is_some_and(|p| current[0] < p[0]))
            && last.is_none()
            && current.iter().any(|x| *x > 0)
        {
            let id = hash(&[
                "codex-missing-last",
                &state.thread,
                string(entry, "timestamp").unwrap_or("unknown"),
            ]);
            issue(
                tx,
                "codex",
                &id,
                timestamp(entry),
                "missing last usage for initial or reset counter",
            )?;
        }
        let (delta, quality_source) = match state.previous {
            None => (
                last.map(raw).unwrap_or([0; 5]),
                last.unwrap_or(&Value::Null),
            ),
            Some(prev) if current[0] < prev[0] => (
                last.map(raw).unwrap_or([0; 5]),
                last.unwrap_or(&Value::Null),
            ),
            Some(prev) => (
                std::array::from_fn(|i| current[i].saturating_sub(prev[i])),
                total,
            ),
        };
        state.previous = Some(current);
        if delta.iter().any(|x| *x > 0) {
            let mut u = usage(quality_source, "codex");
            u.values = [
                delta[0].saturating_sub(delta[1].saturating_add(delta[2])),
                delta[1],
                delta[2],
                delta[3],
                delta[4],
            ];
            if delta[1].saturating_add(delta[2]) > delta[0] {
                u.incomplete = true;
            }
            let mut current_state = state.clone();
            if current_state.turn.is_empty() {
                current_state.turn = hash(&["turn", &current_state.thread, "unknown"]);
            }
            let line_hash = hash(&[&String::from_utf8_lossy(line)]);
            let id = hash(&["codex-legacy", &current_state.thread, &line_hash]);
            insert(
                tx,
                &event_from(&current_state, id, "codex", "legacy", timestamp(entry), u),
            )?;
        }
    }
    Ok(())
}

fn claude(tx: &Transaction<'_>, entry: &Value, file_key: &str, offset: u64) -> Result<()> {
    if string(entry, "type") != Some("assistant") {
        return Ok(());
    }
    let m = entry.get("message").unwrap_or(&Value::Null);
    let Some(usage_value) = m.get("usage").filter(|u| u.is_object()) else {
        return Ok(());
    };
    let (Some(session), Some(message)) = (string(entry, "sessionId"), string(m, "id")) else {
        let id = hash(&[
            "claude-missing-identity",
            file_key,
            string(entry, "uuid").unwrap_or(&offset.to_string()),
        ]);
        issue(
            tx,
            "claude",
            &id,
            timestamp(entry),
            "missing response or session identifier",
        )?;
        return Ok(());
    };
    let request = string(entry, "requestId").unwrap_or("unknown-request");
    let thread = hash(&["claude-thread", session]);
    let turn = hash(&["claude-response", session, message, request]);
    let state = State {
        thread,
        turn: turn.clone(),
        model: string(m, "model").unwrap_or("").into(),
        effort: string(entry, "perTurnEffort")
            .or_else(|| string(entry, "effort"))
            .unwrap_or("")
            .into(),
        project: string(entry, "cwd").map(project).unwrap_or_default(),
        previous: None,
    };
    insert(
        tx,
        &event_from(
            &state,
            turn,
            "claude",
            "claude",
            timestamp(entry),
            usage(usage_value, "claude"),
        ),
    )
}

fn anchor(file: &mut File, offset: u64) -> Result<String> {
    let n = offset.min(256) as usize;
    file.seek(SeekFrom::Start(offset - n as u64))?;
    let mut bytes = vec![0; n];
    file.read_exact(&mut bytes)?;
    let mut h = Sha256::new();
    h.update(bytes);
    Ok(format!("{:x}", h.finalize()))
}

fn scan_file(
    conn: &mut rusqlite::Connection,
    path: &Path,
    source: &str,
    stats: &mut Scan,
) -> Result<()> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_file() {
        return Ok(());
    }
    let key = hash(&[
        source,
        &metadata.dev().to_string(),
        &metadata.ino().to_string(),
    ]);
    let saved: Option<(i64, String, String)> = conn
        .query_row(
            "SELECT offset,anchor,state FROM cursors WHERE id=?1",
            [&key],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let mut file = File::open(path)?;
    let mut offset = 0u64;
    let mut state = State::default();
    if let Some((old, old_anchor, old_state)) = saved {
        if old >= 0 && old as u64 <= metadata.len() && anchor(&mut file, old as u64)? == old_anchor
        {
            offset = old as u64;
            state = serde_json::from_str(&old_state).unwrap_or_default();
        }
    }
    if offset == metadata.len() {
        return Ok(());
    }
    stats.files_read += 1;
    file.seek(SeekFrom::Start(offset))?;
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown-file");
    let tx = conn.transaction()?;
    let mut reader = BufReader::with_capacity(256 * 1024, file);
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = reader.read_until(b'\n', &mut line)?;
        if n == 0 {
            break;
        }
        if line.last() != Some(&b'\n') {
            stats.partial += 1;
            break;
        }
        offset += n as u64;
        stats.bytes_read += n as u64;
        let relevant = if source == "codex" {
            [
                b"session_meta".as_slice(),
                b"turn_context",
                b"token_usage_record",
                b"token_count",
                b"task_started",
            ]
            .iter()
            .any(|x| memmem::find(&line, x).is_some())
        } else {
            memmem::find(&line, b"\"usage\"").is_some()
                && memmem::find(&line, b"\"assistant\"").is_some()
        };
        if !relevant {
            continue;
        }
        match serde_json::from_slice::<Value>(&line) {
            Ok(v) if source == "codex" => codex(&tx, &mut state, &v, &line, filename)?,
            Ok(v) => claude(&tx, &v, &key, offset - n as u64)?,
            Err(_) => {
                stats.malformed += 1;
                let id = hash(&["malformed", &key, &(offset - n as u64).to_string()]);
                issue(&tx, source, &id, None, "malformed complete line")?;
            }
        }
    }
    let mut file = reader.into_inner();
    let fingerprint = anchor(&mut file, offset)?;
    tx.execute("INSERT INTO cursors VALUES (?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET offset=excluded.offset,anchor=excluded.anchor,state=excluded.state",
        params![key,offset as i64,fingerprint,serde_json::to_string(&state)?])?;
    tx.commit()?;
    Ok(())
}

pub fn collect(roots: &Roots, db: &Path) -> Result<Scan> {
    background_priority();
    let mut conn = store::open(db).context("Opening the local usage store")?;
    let mut scan = Scan {
        codex_present: roots.codex.is_dir(),
        archive_present: roots.archive.is_dir(),
        claude_present: roots.claude.is_dir(),
        ..Scan::default()
    };
    for (source, root) in [
        ("codex", &roots.codex),
        ("codex", &roots.archive),
        ("claude", &roots.claude),
    ] {
        if !root.is_dir() {
            continue;
        }
        for entry in WalkDir::new(root).follow_links(false).into_iter() {
            match entry {
                Ok(e)
                    if e.file_type().is_file()
                        && e.path().extension().is_some_and(|x| x == "jsonl") =>
                {
                    if scan_file(&mut conn, e.path(), source, &mut scan).is_err() {
                        scan.errors += 1;
                    }
                    // Cooperate without a timer: background-QoS sleeps are heavily coalesced.
                    thread::yield_now();
                }
                Err(_) => scan.errors += 1,
                _ => (),
            }
        }
    }
    Ok(scan)
}
