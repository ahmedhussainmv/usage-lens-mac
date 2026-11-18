//! Synthetic usage for screenshots and trying the app without any real history.
//! Nothing here reads from the user's machine.
use crate::store;
use anyhow::Result;
use chrono::Utc;
use rusqlite::params;
use std::path::Path;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

/// (project, source, category, relative weight)
const PROJECTS: [(&str, &str, &str, u64); 8] = [
    ("storefront-web", "claude", "work", 10),
    ("billing-api", "claude", "work", 7),
    ("mobile-app", "codex", "work", 9),
    ("data-pipeline", "codex", "work", 5),
    ("dotfiles", "claude", "personal", 2),
    ("recipe-tracker", "codex", "personal", 4),
    ("garden-planner", "claude", "personal", 3),
    ("blog", "claude", "personal", 2),
];
/// (source, model, effort)
const MODELS: [(&str, &str, &str); 5] = [
    ("claude", "claude-sonnet-5-5", "medium"),
    ("claude", "claude-opus-5-5", "high"),
    ("claude", "claude-haiku-5-5", "low"),
    ("codex", "gpt-5-codex", "high"),
    ("codex", "gpt-5-mini", "medium"),
];

pub fn seed(db: &Path) -> Result<()> {
    let mut conn = store::open(db)?;
    let tx = conn.transaction()?;
    let now = Utc::now().timestamp();
    let mut rng = Rng(42);
    for day in 0..30i64 {
        // Busier on weekdays, with a few quiet days.
        let weekday = ((now / 86_400 - day) + 4) % 7;
        let busy = if weekday >= 5 { 1 } else { 3 };
        for (project, source, _, weight) in PROJECTS {
            let count = rng.below(busy * weight / 2 + 1);
            let models: Vec<_> = MODELS.iter().filter(|m| m.0 == source).collect();
            for k in 0..count {
                let (_, model, effort) = models[rng.below(models.len() as u64) as usize];
                let thread = format!("demo-thread-{project}-{day}-{}", k % 3);
                let turn = format!("demo-turn-{project}-{day}-{k}");
                let uncached = 200 + rng.below(4_000) as i64;
                let cache_read = 20_000 + rng.below(400_000) as i64;
                let cache_write = if source == "claude" { rng.below(30_000) as i64 } else { 0 };
                let output = 300 + rng.below(6_000) as i64;
                let ts = now - day * 86_400 - rng.below(80_000) as i64;
                tx.execute(
                    "INSERT OR IGNORE INTO events VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,0,1)",
                    params![
                        format!("demo-{project}-{day}-{k}"),
                        source,
                        if source == "claude" { "claude" } else { "structured" },
                        thread,
                        turn,
                        ts,
                        model,
                        effort,
                        project,
                        uncached,
                        cache_read,
                        cache_write,
                        output,
                        output / 4
                    ],
                )?;
            }
        }
    }
    for (project, source, category, _) in PROJECTS {
        if category == "personal" {
            store::set_category(&tx, "project", source, project, category)?;
        }
    }
    // Round example rates (USD per million tokens), only so the estimate has something to show.
    for (source, model, rates) in [
        ("claude", "claude-sonnet-5-5", [3.0, 0.3, 3.75, 15.0]),
        ("claude", "claude-opus-5-5", [15.0, 1.5, 18.75, 75.0]),
        ("claude", "claude-haiku-5-5", [1.0, 0.1, 1.25, 5.0]),
        ("codex", "gpt-5-codex", [1.25, 0.125, 0.0, 10.0]),
        ("codex", "gpt-5-mini", [0.25, 0.025, 0.0, 2.0]),
    ] {
        store::save_rate(
            &tx,
            &store::Rate {
                source: source.into(),
                model: model.into(),
                input: rates[0],
                cache_read: rates[1],
                cache_write: rates[2],
                output: rates[3],
            },
        )?;
    }
    store::set_default_category(&tx, "work")?;
    store::set_setting(&tx, "onboarded", "1")?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn seeds_both_categories_and_sources() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("demo.sqlite");
        super::seed(&db).unwrap();
        let conn = crate::store::open(&db).unwrap();
        let s = crate::store::query(&conn, &crate::store::Filter::default()).unwrap();
        assert!(s.events > 100 && s.days.iter().any(|d| d.tokens > 0));
        assert!(s.split.iter().all(|x| x.tokens > 0));
        assert_eq!(s.unpriced, 0);
    }
}
