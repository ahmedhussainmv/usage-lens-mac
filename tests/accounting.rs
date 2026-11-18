use serde_json::{json, Value};
use std::{fs, io::Write, path::Path};
use tempfile::TempDir;
use usage_lens_mac::{
    collector::{collect, Roots},
    store::{open, query, save_rate, Filter, Rate},
};

fn write_row(file: &mut fs::File, value: Value) {
    writeln!(file, "{}", value).unwrap();
}
fn setup() -> (TempDir, Roots, std::path::PathBuf) {
    let temp = TempDir::new().unwrap();
    let codex = temp.path().join("codex");
    let archive = temp.path().join("archive");
    let claude = temp.path().join("claude");
    fs::create_dir_all(&codex).unwrap();
    fs::create_dir_all(&archive).unwrap();
    fs::create_dir_all(&claude).unwrap();
    let db = temp.path().join("db.sqlite");
    (
        temp,
        Roots {
            codex,
            archive,
            claude,
        },
        db,
    )
}
fn totals(db: &Path) -> usage_lens_mac::store::Summary {
    query(&open(db).unwrap(), &Filter::default()).unwrap()
}
fn claude(usage: Value, id: &str, request: &str) -> Value {
    json!({"type":"assistant", "uuid":"block", "sessionId":"session", "requestId":request,
      "timestamp":"2026-09-01T00:00:00Z", "cwd":"/work/app", "effort":"high",
      "message":{"id":id,"model":"claude-model","content":[{"text":"PRIVATE RESPONSE"}],"usage":usage}})
}

#[test]
fn claude_deduplicates_snapshots_and_accounts_cache_separately() {
    let (_tmp, roots, db) = setup();
    let path = roots.claude.join("s.jsonl");
    let mut f = fs::File::create(&path).unwrap();
    let u = json!({"input_tokens":10,"cache_read_input_tokens":20,"cache_creation_input_tokens":30,"output_tokens":5});
    write_row(&mut f, claude(u.clone(), "m", "r"));
    write_row(&mut f, claude(u.clone(), "m", "r"));
    collect(&roots, &db).unwrap();
    let s = totals(&db);
    assert_eq!(
        (
            s.events,
            s.threads,
            s.uncached,
            s.cache_read,
            s.cache_write,
            s.output
        ),
        (1, 1, 10, 20, 30, 5)
    );
    let second = collect(&roots, &db).unwrap();
    assert_eq!(second.bytes_read, 0);
    write!(f,"{}",claude(json!({"input_tokens":10,"cache_read_input_tokens":20,"cache_creation_input_tokens":30,"output_tokens":8}),"m","r")).unwrap();
    collect(&roots, &db).unwrap();
    assert_eq!(totals(&db).output, 5);
    writeln!(f).unwrap();
    collect(&roots, &db).unwrap();
    assert_eq!(totals(&db).output, 8);
    drop(f);
    let all = fs::read(&db).unwrap();
    assert!(!String::from_utf8_lossy(&all).contains("PRIVATE RESPONSE"));
}

#[test]
fn codex_structured_and_legacy_overlap_is_not_double_counted() {
    let (_tmp, roots, db) = setup();
    let mut f = fs::File::create(roots.codex.join("rollout.jsonl")).unwrap();
    write_row(
        &mut f,
        json!({"type":"session_meta","payload":{"id":"thread","cwd":"/work/app","base_instructions":"SECRET"}}),
    );
    write_row(
        &mut f,
        json!({"type":"turn_context","payload":{"turn_id":"turn","model":"model-a","effort":"high"}}),
    );
    write_row(
        &mut f,
        json!({"type":"token_usage_record","timestamp":"2026-09-01T00:00:00Z","payload":{"turn_id":"turn","response_id":"r1","usage":{"input_tokens":100,"cached_input_tokens":20,"cache_write_input_tokens":10,"output_tokens":5}}}),
    );
    write_row(
        &mut f,
        json!({"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":20,"cache_write_input_tokens":10,"output_tokens":5},"last_token_usage":{"input_tokens":100,"cached_input_tokens":20,"cache_write_input_tokens":10,"output_tokens":5}}}}),
    );
    collect(&roots, &db).unwrap();
    collect(&roots, &db).unwrap();
    let s = totals(&db);
    assert_eq!(
        (s.events, s.uncached, s.cache_read, s.cache_write, s.output),
        (1, 70, 20, 10, 5)
    );
    assert!(!String::from_utf8_lossy(&fs::read(&db).unwrap()).contains("SECRET"));
}

#[test]
fn legacy_counters_resets_moves_and_model_changes() {
    let (_tmp, roots, db) = setup();
    let file = roots.codex.join("rollout.jsonl");
    let mut f = fs::File::create(&file).unwrap();
    write_row(&mut f, json!({"type":"session_meta","payload":{"id":"t"}}));
    for (turn, model, total, last) in [
        ("a", "m1", 100, 10),
        ("b", "m2", 125, 25),
        ("c", "m2", 4, 4),
    ] {
        write_row(
            &mut f,
            json!({"type":"turn_context","payload":{"turn_id":turn,"model":model,"effort":"low"}}),
        );
        write_row(
            &mut f,
            json!({"type":"event_msg","timestamp":"2026-09-01T00:00:00Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":total,"cached_input_tokens":0,"output_tokens":0},"last_token_usage":{"input_tokens":last,"cached_input_tokens":0,"output_tokens":0}}}}),
        );
    }
    collect(&roots, &db).unwrap();
    assert_eq!(totals(&db).uncached, 39);
    fs::rename(&file, roots.archive.join("moved.jsonl")).unwrap();
    collect(&roots, &db).unwrap();
    assert_eq!(totals(&db).uncached, 39);
    let s = totals(&db);
    assert_eq!(
        s.models.iter().find(|x| x.label == "m1").unwrap().uncached,
        10
    );
    assert_eq!(
        s.models.iter().find(|x| x.label == "m2").unwrap().uncached,
        29
    );
}

#[test]
fn missing_fields_undated_and_absent_sources_are_visible() {
    let (_tmp, roots, db) = setup();
    let mut f = fs::File::create(roots.claude.join("s.jsonl")).unwrap();
    let mut row = claude(json!({"input_tokens":7}), "m", "r");
    row.as_object_mut().unwrap().remove("timestamp");
    write_row(&mut f, row);
    fs::remove_dir_all(&roots.archive).unwrap();
    let stats = collect(&roots, &db).unwrap();
    assert!(!stats.archive_present);
    let s = totals(&db);
    assert_eq!(s.incomplete, 1);
    assert_eq!(s.undated, 1);
    assert_eq!(s.uncached, 7);
    let recent = query(
        &open(&db).unwrap(),
        &Filter {
            since: Some(1),
            ..Filter::default()
        },
    )
    .unwrap();
    assert_eq!(recent.events, 0);
    assert_eq!(recent.undated, 1);
}

#[test]
fn copied_files_deduplicate_and_new_response_keeps_its_model_and_effort() {
    let (_tmp, roots, db) = setup();
    let path = roots.claude.join("a.jsonl");
    let mut f = fs::File::create(&path).unwrap();
    let u = json!({"input_tokens":10,"cache_read_input_tokens":20,"cache_creation_input_tokens":30,"output_tokens":5});
    write_row(&mut f, claude(u.clone(), "m1", "r1"));
    fs::copy(&path, roots.claude.join("duplicate.jsonl")).unwrap();
    collect(&roots, &db).unwrap();
    assert_eq!(totals(&db).events, 1);
    let mut b = claude(u, "m2", "r2");
    b["message"]["model"] = json!("another-model");
    b["effort"] = json!("low");
    write_row(&mut f, b);
    collect(&roots, &db).unwrap();
    let s = totals(&db);
    assert_eq!(s.events, 2);
    assert!(s
        .models
        .iter()
        .any(|x| x.label == "another-model" && x.effort == "low"));
    assert!(s
        .models
        .iter()
        .any(|x| x.label == "claude-model" && x.effort == "high"));
}

#[test]
fn estimate_uses_four_user_rates_and_keeps_unknown_models_unpriced() {
    let (_tmp, roots, db) = setup();
    let mut f = fs::File::create(roots.claude.join("s.jsonl")).unwrap();
    let u = json!({"input_tokens":1000000,"cache_read_input_tokens":1000000,"cache_creation_input_tokens":1000000,"output_tokens":1000000});
    write_row(&mut f, claude(u.clone(), "m1", "r1"));
    let mut other = claude(u, "m2", "r2");
    other["message"]["model"] = json!("unpriced");
    write_row(&mut f, other);
    collect(&roots, &db).unwrap();
    let conn = open(&db).unwrap();
    save_rate(
        &conn,
        &Rate {
            source: "claude".into(),
            model: "claude-model".into(),
            input: 2.0,
            cache_read: 0.1,
            cache_write: 2.5,
            output: 10.0,
        },
    )
    .unwrap();
    assert!(save_rate(
        &conn,
        &Rate {
            source: "claude".into(),
            model: "bad".into(),
            input: -1.0,
            cache_read: 0.0,
            cache_write: 0.0,
            output: 0.0
        }
    )
    .is_err());
    let s = query(&conn, &Filter::default()).unwrap();
    assert!((s.estimate - 14.6).abs() < 1e-9);
    assert_eq!(s.unpriced, 1);
}

#[test]
fn unidentifiable_response_and_inherited_counter_are_reported_once() {
    let (_tmp, roots, db) = setup();
    let mut f = fs::File::create(roots.claude.join("s.jsonl")).unwrap();
    let mut row = claude(
        json!({"input_tokens":10,"cache_read_input_tokens":1,"cache_creation_input_tokens":0,"output_tokens":2}),
        "m",
        "r",
    );
    row["message"].as_object_mut().unwrap().remove("id");
    write_row(&mut f, row);
    let mut c = fs::File::create(roots.codex.join("s.jsonl")).unwrap();
    write_row(&mut c, json!({"type":"session_meta","payload":{"id":"t"}}));
    write_row(
        &mut c,
        json!({"type":"event_msg","timestamp":"2026-09-01T00:00:00Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":900,"cached_input_tokens":0,"output_tokens":1}}}}),
    );
    collect(&roots, &db).unwrap();
    collect(&roots, &db).unwrap();
    let summary = totals(&db);
    assert_eq!(summary.events, 0);
    assert_eq!(summary.excluded, 2);
}

#[test]
fn zero_usage_snapshots_do_not_count_as_usage_but_can_be_revised() {
    let (_tmp, roots, db) = setup();
    let mut f = fs::File::create(roots.claude.join("s.jsonl")).unwrap();
    write_row(
        &mut f,
        claude(
            json!({"input_tokens":0,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"output_tokens":0}),
            "m",
            "r",
        ),
    );
    collect(&roots, &db).unwrap();
    assert_eq!((totals(&db).events, totals(&db).threads), (0, 0));
    write_row(
        &mut f,
        claude(
            json!({"input_tokens":1,"cache_read_input_tokens":2,"cache_creation_input_tokens":3,"output_tokens":4}),
            "m",
            "r",
        ),
    );
    collect(&roots, &db).unwrap();
    assert_eq!((totals(&db).events, totals(&db).threads), (1, 1));
}

#[test]
fn daily_chart_uses_actual_dates_and_the_selected_period() {
    let (_tmp, roots, db) = setup();
    let mut f = fs::File::create(roots.claude.join("s.jsonl")).unwrap();
    let u = json!({"input_tokens":10,"cache_read_input_tokens":20,"cache_creation_input_tokens":30,"output_tokens":5});
    let now = chrono::Utc::now();
    let mut today = claude(u.clone(), "today", "r1");
    today["timestamp"] = json!(now.to_rfc3339());
    let mut older = claude(u, "older", "r2");
    older["timestamp"] = json!((now - chrono::Duration::days(3)).to_rfc3339());
    write_row(&mut f, today);
    write_row(&mut f, older);
    collect(&roots, &db).unwrap();
    let conn = open(&db).unwrap();
    let all = query(&conn, &Filter::default()).unwrap();
    assert_eq!(all.days.len(), 14);
    assert_eq!(all.days.iter().map(|d| d.tokens).sum::<i64>(), 130);
    let recent = query(
        &conn,
        &Filter {
            source: "claude".into(),
            since: Some(now.timestamp() - 86400),
            ..Filter::default()
        },
    )
    .unwrap();
    assert_eq!(recent.days.iter().map(|d| d.tokens).sum::<i64>(), 65);
    let codex = query(
        &conn,
        &Filter {
            source: "codex".into(),
            since: None,
            ..Filter::default()
        },
    )
    .unwrap();
    assert_eq!(codex.days.iter().map(|d| d.tokens).sum::<i64>(), 0);
}

#[test]
fn conflicting_codex_sources_are_flagged_without_adding_them_together() {
    let (_tmp, roots, db) = setup();
    let mut f = fs::File::create(roots.codex.join("s.jsonl")).unwrap();
    write_row(
        &mut f,
        json!({"type":"session_meta","payload":{"id":"thread"}}),
    );
    write_row(
        &mut f,
        json!({"type":"turn_context","payload":{"turn_id":"turn","model":"m"}}),
    );
    write_row(
        &mut f,
        json!({"type":"token_usage_record","timestamp":"2026-09-01T00:00:00Z","payload":{"turn_id":"turn","response_id":"r","usage":{"input_tokens":50,"cached_input_tokens":0,"cache_write_input_tokens":0,"output_tokens":0}}}),
    );
    write_row(
        &mut f,
        json!({"type":"event_msg","timestamp":"2026-09-01T00:00:01Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":0,"cache_write_input_tokens":0,"output_tokens":0},"last_token_usage":{"input_tokens":100,"cached_input_tokens":0,"cache_write_input_tokens":0,"output_tokens":0}}}}),
    );
    collect(&roots, &db).unwrap();
    let s = totals(&db);
    assert_eq!(s.uncached, 50);
    assert_eq!(s.conflicting_turns, 1);
}

#[test]
fn categories_default_to_work_and_follow_assignments() {
    use usage_lens_mac::store::{clear_category, set_category, set_default_category};
    let (_tmp, roots, db) = setup();
    let mut f = fs::File::create(roots.claude.join("s.jsonl")).unwrap();
    let u = json!({"input_tokens":10,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"output_tokens":5});
    write_row(&mut f, claude(u, "m", "r"));
    collect(&roots, &db).unwrap();
    let conn = open(&db).unwrap();
    let by = |category: &str| {
        query(
            &conn,
            &Filter {
                category: category.into(),
                ..Filter::default()
            },
        )
        .unwrap()
    };
    // Unassigned usage follows the default, which starts as work.
    assert_eq!(by("work").events, 1);
    assert_eq!(by("personal").events, 0);
    assert_eq!(by("all").default_category, "work");
    // Project assignment moves it; the split ignores the category filter.
    set_category(&conn, "project", "claude", "app", "personal").unwrap();
    assert_eq!(by("work").events, 0);
    let personal = by("personal");
    assert_eq!(personal.events, 1);
    assert_eq!(personal.split.iter().map(|s| s.tokens).sum::<i64>(), 15);
    // A thread assignment overrides its project; clearing it falls back again.
    let thread: String = conn
        .query_row("SELECT thread FROM events LIMIT 1", [], |r| r.get(0))
        .unwrap();
    set_category(&conn, "thread", "claude", &thread, "work").unwrap();
    assert_eq!(by("work").events, 1);
    clear_category(&conn, "thread", "claude", &thread).unwrap();
    assert_eq!(by("personal").events, 1);
    // Clearing the project assignment follows the changed default.
    clear_category(&conn, "project", "claude", "app").unwrap();
    set_default_category(&conn, "personal").unwrap();
    assert_eq!(by("personal").events, 1);
    assert!(set_default_category(&conn, "nonsense").is_err());
}
