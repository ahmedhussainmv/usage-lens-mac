use anyhow::{bail, Context, Result};
use chrono::{DateTime, Days, Local, Utc};
use rusqlite::{params, Connection, OpenFlags};
use std::{
    collections::HashMap,
    fs::{DirBuilder, OpenOptions},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::Path,
    time::Duration,
};

pub fn open(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
    }
    // Create privately before SQLite opens it, including on the first run.
    OpenOptions::new()
        .write(true)
        .create(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("Cannot create usage database at {}", path.display()))?;
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
    )?;
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;
        CREATE TABLE IF NOT EXISTS events (
          id TEXT PRIMARY KEY, source TEXT NOT NULL, kind TEXT NOT NULL,
          thread TEXT NOT NULL, turn TEXT NOT NULL, ts INTEGER,
          model TEXT NOT NULL, effort TEXT NOT NULL, project TEXT NOT NULL,
          uncached INTEGER NOT NULL, cache_read INTEGER NOT NULL, cache_write INTEGER NOT NULL,
          output INTEGER NOT NULL, reasoning INTEGER NOT NULL, incomplete INTEGER NOT NULL,
          quality INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS event_turn ON events(source, thread, turn, kind);
        CREATE INDEX IF NOT EXISTS event_time ON events(ts, source);
        CREATE TABLE IF NOT EXISTS cursors (
          id TEXT PRIMARY KEY, offset INTEGER NOT NULL, anchor TEXT NOT NULL, state TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS ingest_issues (
          id TEXT PRIMARY KEY, source TEXT NOT NULL, ts INTEGER, reason TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS rates (
          source TEXT NOT NULL, model TEXT NOT NULL, input REAL NOT NULL,
          cache_read REAL NOT NULL, cache_write REAL NOT NULL, output REAL NOT NULL,
          PRIMARY KEY(source,model)
        );
        CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS categories (
          scope TEXT NOT NULL, source TEXT NOT NULL, key TEXT NOT NULL, category TEXT NOT NULL,
          PRIMARY KEY(scope,source,key)
        );
        CREATE VIEW IF NOT EXISTS effective_events_v2 AS
          SELECT e.* FROM events e WHERE e.kind != 'legacy' OR NOT EXISTS
          (SELECT 1 FROM events s WHERE s.source=e.source AND s.thread=e.thread
           AND s.turn=e.turn AND s.kind='structured');
        CREATE VIEW IF NOT EXISTS counted_events AS
          SELECT * FROM effective_events_v2 WHERE NOT
          (source='claude' AND incomplete=0 AND uncached=0 AND cache_read=0 AND cache_write=0 AND output=0 AND reasoning=0);
        CREATE VIEW IF NOT EXISTS event_categories AS
          SELECT e.id AS id, COALESCE(ct.category, cp.category,
            (SELECT value FROM settings WHERE key='default_category'), 'work') AS category
          FROM events e
          LEFT JOIN categories ct ON ct.scope='thread' AND ct.source=e.source AND ct.key=e.thread
          LEFT JOIN categories cp ON cp.scope='project' AND cp.source=e.source AND cp.key=e.project;
        CREATE VIEW IF NOT EXISTS categorized_events_v2 AS
          SELECT e.*, COALESCE(ct.category, cp.category,
            (SELECT value FROM settings WHERE key='default_category'), 'work') AS category
          FROM counted_events e
          LEFT JOIN categories ct ON ct.scope='thread' AND ct.source=e.source AND ct.key=e.thread
          LEFT JOIN categories cp ON cp.scope='project' AND cp.source=e.source AND cp.key=e.project;")?;
    Ok(conn)
}

#[derive(Clone, Debug)]
pub struct Filter {
    pub source: String,
    pub since: Option<i64>,
    /// "all", "work" or "personal".
    pub category: String,
}
impl Default for Filter {
    fn default() -> Self {
        Self {
            source: "both".into(),
            since: None,
            category: "all".into(),
        }
    }
}

#[derive(Default, Debug, Clone)]
pub struct Group {
    pub label: String,
    pub source: String,
    pub effort: String,
    pub events: i64,
    pub uncached: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub output: i64,
}

#[derive(Default, Debug, Clone)]
pub struct Day {
    pub date: String,
    pub tokens: i64,
}

#[derive(Default, Debug, Clone)]
pub struct Summary {
    pub events: i64,
    pub threads: i64,
    pub uncached: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub output: i64,
    pub reasoning: i64,
    pub incomplete: i64,
    pub undated: i64,
    pub unpriced: i64,
    pub excluded: i64,
    pub conflicting_turns: i64,
    pub estimate: f64,
    pub models: Vec<Group>,
    pub projects: Vec<Group>,
    pub efforts: Vec<Group>,
    pub days: Vec<Day>,
    /// Totals per category for the same source and period, ignoring the category filter.
    pub split: Vec<Split>,
    pub default_category: String,
    pub onboarded: bool,
    /// Explicit project assignments keyed by (source, project folder name).
    pub project_cats: HashMap<(String, String), String>,
}

#[derive(Default, Debug, Clone)]
pub struct Split {
    pub category: String,
    pub tokens: i64,
    pub events: i64,
    pub threads: i64,
    pub estimate: f64,
    pub unpriced: i64,
}

pub const CATEGORIES: [&str; 2] = ["work", "personal"];

pub fn setting(conn: &Connection, key: &str) -> Result<Option<String>> {
    use rusqlite::OptionalExtension;
    Ok(conn
        .query_row("SELECT value FROM settings WHERE key=?1", [key], |r| r.get(0))
        .optional()?)
}
pub fn set_setting(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO settings VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![key, value],
    )?;
    Ok(())
}
fn check_category(category: &str) -> Result<()> {
    if !CATEGORIES.contains(&category) {
        bail!("Unknown category.");
    }
    Ok(())
}
/// Sets the category used for events that have no explicit assignment.
pub fn set_default_category(conn: &Connection, category: &str) -> Result<()> {
    check_category(category)?;
    set_setting(conn, "default_category", category)
}
/// Assigns a project (`scope` = "project", key = folder name) or a thread
/// (`scope` = "thread", key = thread id). A thread assignment overrides its project's.
pub fn set_category(
    conn: &Connection,
    scope: &str,
    source: &str,
    key: &str,
    category: &str,
) -> Result<()> {
    check_category(category)?;
    if !matches!(scope, "project" | "thread") || !matches!(source, "codex" | "claude") {
        bail!("Unknown assignment target.");
    }
    conn.execute(
        "INSERT INTO categories VALUES (?1,?2,?3,?4) ON CONFLICT(scope,source,key) DO UPDATE SET category=excluded.category",
        params![scope, source, key, category],
    )?;
    Ok(())
}
/// Removes an explicit assignment so the item follows the default again.
pub fn clear_category(conn: &Connection, scope: &str, source: &str, key: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM categories WHERE scope=?1 AND source=?2 AND key=?3",
        params![scope, source, key],
    )?;
    Ok(())
}

/// One row of an exportable report: usage grouped by category, source, project and model.
#[derive(Debug, Clone)]
pub struct ReportRow {
    pub category: String,
    pub source: String,
    pub project: String,
    pub model: String,
    pub events: i64,
    pub uncached: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub output: i64,
    pub estimate: f64,
}
pub fn report_rows(conn: &Connection, filter: &Filter) -> Result<Vec<ReportRow>> {
    let mut stmt = conn.prepare(
        "SELECT e.category, e.source, e.project, e.model, COUNT(*), SUM(e.uncached), SUM(e.cache_read),
         SUM(e.cache_write), SUM(e.output),
         COALESCE(SUM((e.uncached*r.input + e.cache_read*r.cache_read + e.cache_write*r.cache_write + e.output*r.output)/1000000.0),0)
         FROM categorized_events_v2 e LEFT JOIN rates r ON r.model=e.model AND r.source=e.source
         WHERE (?1='both' OR e.source=?1) AND (?2 IS NULL OR e.ts>=?2) AND (?3='all' OR e.category=?3)
         GROUP BY e.category,e.source,e.project,e.model ORDER BY e.category,e.source,e.project,e.model",
    )?;
    let rows = stmt.query_map(params![filter.source, filter.since, filter.category], |r| {
        Ok(ReportRow {
            category: r.get(0)?,
            source: r.get(1)?,
            project: r.get(2)?,
            model: r.get(3)?,
            events: r.get(4)?,
            uncached: r.get(5)?,
            cache_read: r.get(6)?,
            cache_write: r.get(7)?,
            output: r.get(8)?,
            estimate: r.get(9)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

pub fn query(conn: &Connection, filter: &Filter) -> Result<Summary> {
    let mut s = conn.query_row("SELECT COUNT(*), COUNT(DISTINCT e.source||':'||e.thread),
        COALESCE(SUM(e.uncached),0), COALESCE(SUM(e.cache_read),0), COALESCE(SUM(e.cache_write),0),
        COALESCE(SUM(e.output),0), COALESCE(SUM(e.reasoning),0), COALESCE(SUM(e.incomplete),0),
        COALESCE(SUM(CASE WHEN r.model IS NULL THEN 1 ELSE 0 END),0),
        COALESCE(SUM((e.uncached*r.input + e.cache_read*r.cache_read + e.cache_write*r.cache_write + e.output*r.output)/1000000.0),0)
        FROM categorized_events_v2 e LEFT JOIN rates r ON r.model=e.model AND r.source=e.source
        WHERE (?1='both' OR e.source=?1) AND (?2 IS NULL OR e.ts>=?2) AND (?3='all' OR e.category=?3)",
        params![filter.source,filter.since,filter.category], |r| Ok(Summary {
            events:r.get(0)?, threads:r.get(1)?, uncached:r.get(2)?, cache_read:r.get(3)?,
            cache_write:r.get(4)?, output:r.get(5)?, reasoning:r.get(6)?, incomplete:r.get(7)?,
            unpriced:r.get(8)?, estimate:r.get(9)?, ..Summary::default()
        }))?;
    s.undated = conn.query_row(
        "SELECT COUNT(*) FROM categorized_events_v2 e WHERE e.ts IS NULL AND (?1='both' OR e.source=?1) AND (?2='all' OR e.category=?2)",
        params![filter.source, filter.category],
        |r| r.get(0),
    )?;
    s.excluded = conn.query_row(
        "SELECT COUNT(*) FROM ingest_issues WHERE (?1='both' OR source=?1)",
        [&filter.source],
        |r| r.get(0),
    )?;
    s.conflicting_turns=conn.query_row("SELECT COUNT(*) FROM (
        SELECT thread,turn,
          SUM(CASE WHEN kind='structured' THEN 1 ELSE 0 END) AS structured_count,
          SUM(CASE WHEN kind='legacy' THEN 1 ELSE 0 END) AS legacy_count,
          SUM(CASE WHEN kind='structured' THEN uncached+cache_read+cache_write+output ELSE 0 END) AS structured_tokens,
          SUM(CASE WHEN kind='legacy' THEN uncached+cache_read+cache_write+output ELSE 0 END) AS legacy_tokens
        FROM events WHERE source='codex' AND (?1='both' OR ?1='codex') AND (?2 IS NULL OR ts>=?2)
          AND (?3='all' OR EXISTS (SELECT 1 FROM event_categories c WHERE c.id=events.id AND c.category=?3))
        GROUP BY thread,turn HAVING structured_count>0 AND legacy_count>0 AND legacy_tokens>structured_tokens)",
        params![filter.source,filter.since,filter.category], |r|r.get(0))?;
    for (column, dest) in [
        ("model", &mut s.models),
        ("project", &mut s.projects),
        ("effort", &mut s.efforts),
    ] {
        // The column is selected only from this fixed list.
        let sql=format!("SELECT e.{column}, e.source, {}, COUNT(*), SUM(e.uncached), SUM(e.cache_read), SUM(e.cache_write), SUM(e.output)
            FROM categorized_events_v2 e WHERE (?1='both' OR e.source=?1) AND (?2 IS NULL OR e.ts>=?2) AND (?3='all' OR e.category=?3)
            GROUP BY e.{column},e.source{} ORDER BY SUM(e.uncached+e.cache_read+e.cache_write+e.output) DESC LIMIT 500",
            if column=="model" {"e.effort"} else {"''"}, if column=="model" {",e.effort"} else {""});
        let mut statement = conn.prepare(&sql)?;
        let rows = statement.query_map(params![filter.source, filter.since, filter.category], |r| {
            Ok(Group {
                label: r.get(0)?,
                source: r.get(1)?,
                effort: r.get(2)?,
                events: r.get(3)?,
                uncached: r.get(4)?,
                cache_read: r.get(5)?,
                cache_write: r.get(6)?,
                output: r.get(7)?,
            })
        })?;
        for row in rows {
            dest.push(row?);
        }
    }
    let mut stmt=conn.prepare("SELECT date(e.ts,'unixepoch','localtime') AS day, SUM(e.uncached+e.cache_read+e.cache_write+e.output)
        FROM categorized_events_v2 e WHERE e.ts IS NOT NULL AND (?1='both' OR e.source=?1) AND (?2 IS NULL OR e.ts>=?2) AND (?3='all' OR e.category=?3)
        AND date(e.ts,'unixepoch','localtime')>=date('now','localtime','-13 days') GROUP BY day")?;
    let daily: HashMap<String, i64> = stmt
        .query_map(params![filter.source, filter.since, filter.category], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let today = Local::now().date_naive();
    let period_start = filter
        .since
        .and_then(|s| DateTime::<Utc>::from_timestamp(s, 0))
        .map(|d| d.with_timezone(&Local).date_naive());
    for ago in (0..14u64).rev() {
        let day = today.checked_sub_days(Days::new(ago)).unwrap();
        if period_start.is_some_and(|start| day < start) {
            continue;
        }
        let date = day.to_string();
        s.days.push(Day {
            tokens: *daily.get(&date).unwrap_or(&0),
            date,
        });
    }
    let mut stmt = conn.prepare(
        "SELECT e.category, COALESCE(SUM(e.uncached+e.cache_read+e.cache_write+e.output),0), COUNT(*),
         COUNT(DISTINCT e.source||':'||e.thread),
         COALESCE(SUM((e.uncached*r.input + e.cache_read*r.cache_read + e.cache_write*r.cache_write + e.output*r.output)/1000000.0),0),
         COALESCE(SUM(CASE WHEN r.model IS NULL THEN 1 ELSE 0 END),0)
         FROM categorized_events_v2 e LEFT JOIN rates r ON r.model=e.model AND r.source=e.source
         WHERE (?1='both' OR e.source=?1) AND (?2 IS NULL OR e.ts>=?2) GROUP BY e.category",
    )?;
    let mut split: Vec<Split> = stmt
        .query_map(params![filter.source, filter.since], |r| {
            Ok(Split {
                category: r.get(0)?,
                tokens: r.get(1)?,
                events: r.get(2)?,
                threads: r.get(3)?,
                estimate: r.get(4)?,
                unpriced: r.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    for name in CATEGORIES {
        if !split.iter().any(|x| x.category == name) {
            split.push(Split {
                category: name.into(),
                ..Split::default()
            });
        }
    }
    split.sort_by_key(|x| CATEGORIES.iter().position(|c| *c == x.category));
    s.split = split;
    s.default_category = setting(conn, "default_category")?
        .filter(|c| CATEGORIES.contains(&c.as_str()))
        .unwrap_or_else(|| "work".into());
    s.onboarded = setting(conn, "onboarded")?.is_some();
    let mut stmt = conn.prepare("SELECT source,key,category FROM categories WHERE scope='project'")?;
    let rows = stmt.query_map([], |r| Ok(((r.get(0)?, r.get(1)?), r.get(2)?)))?;
    for row in rows {
        let (k, v) = row?;
        s.project_cats.insert(k, v);
    }
    Ok(s)
}

#[derive(Clone, Debug)]
pub struct Rate {
    pub source: String,
    pub model: String,
    pub input: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub output: f64,
}

pub fn save_rate(conn: &Connection, rate: &Rate) -> Result<()> {
    if !matches!(rate.source.as_str(), "codex" | "claude")
        || rate.model.trim().is_empty()
        || [rate.input, rate.cache_read, rate.cache_write, rate.output]
            .iter()
            .any(|x| !x.is_finite() || *x < 0.0)
    {
        bail!("Choose a source and model and enter four non-negative rates.");
    }
    conn.execute("INSERT INTO rates VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT(source,model) DO UPDATE SET input=excluded.input,cache_read=excluded.cache_read,cache_write=excluded.cache_write,output=excluded.output",
        params![rate.source,rate.model.trim(),rate.input,rate.cache_read,rate.cache_write,rate.output])?;
    Ok(())
}

pub fn rates(conn: &Connection) -> Result<Vec<Rate>> {
    let mut stmt = conn.prepare(
        "SELECT source,model,input,cache_read,cache_write,output FROM rates ORDER BY source,model",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(Rate {
            source: r.get(0)?,
            model: r.get(1)?,
            input: r.get(2)?,
            cache_read: r.get(3)?,
            cache_write: r.get(4)?,
            output: r.get(5)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}
