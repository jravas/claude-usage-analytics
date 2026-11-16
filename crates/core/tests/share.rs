use std::collections::BTreeMap;
use std::io::Write;
use usage_core::parse::Row;
use usage_core::share::{self, CompareOptions, ExportOptions, Loaded, Metrics, Projects, Summary};
use usage_core::Index;

const SALT: &[u8] = b"0123456789abcdef0123456789abcdef";

/// Strings planted in paths, titles, branches, ids, prompts and tool
/// arguments. None may reach a summary.
const SECRETS: [&str; 12] = [
    "secretuser-bravo",
    "secret-project-charlie",
    "secret-wt-delta",
    "secret-branch-echo",
    "secret title foxtrot",
    "secret-session-golf",
    "secret-entry-hotel",
    "secret-model-india",
    "secret-effort-juliet",
    "secret prompt kilo",
    "secret-arg-lima",
    "secret-agent-mike",
];

fn assistant(sess: &str, cwd: &str, msg: &str, ts: &str, model: &str, extra: &str) -> String {
    format!(
        r#"{{"type":"assistant","timestamp":"{ts}","cwd":"{cwd}","sessionId":"{sess}","gitBranch":"feature/secret-branch-echo","entrypoint":"secret-entry-hotel","effort":"secret-effort-juliet","requestId":"r-{msg}","message":{{"id":"{msg}","model":"{model}","content":[{{"type":"text","text":"secret prompt kilo"}},{{"type":"tool_use","id":"t1","name":"Bash","input":{{"command":"echo secret-arg-lima"}}}}],"usage":{{"input_tokens":3,"output_tokens":40,"cache_creation_input_tokens":100,"cache_read_input_tokens":1000}}}}{extra}}}"#
    )
}

/// A transcript tree full of secrets, indexed through the real parser.
fn secret_index() -> (tempfile::TempDir, Index) {
    let dir = tempfile::tempdir().unwrap();
    let home = "/Users/secretuser-bravo/Code/secret-project-charlie";
    let proj = dir
        .path()
        .join("-Users-secretuser-bravo-Code-secret-project-charlie");
    let sess = "secret-session-golf";
    std::fs::create_dir_all(proj.join(format!("{sess}/subagents"))).unwrap();
    let mut f = std::fs::File::create(proj.join(format!("{sess}.jsonl"))).unwrap();
    writeln!(
        f,
        r#"{{"type":"user","sessionId":"{sess}","cwd":"{home}","message":{{"role":"user","content":"secret prompt kilo"}}}}"#
    )
    .unwrap();
    writeln!(
        f,
        "{}",
        assistant(
            sess,
            home,
            "m1",
            "2026-11-10T12:00:00Z",
            "claude-opus-5-5",
            ""
        )
    )
    .unwrap();
    let wt = format!("{home}/.claude/worktrees/secret-wt-delta");
    writeln!(
        f,
        "{}",
        assistant(
            sess,
            &wt,
            "m2",
            "2026-11-11T12:00:00Z",
            "claude-secret-model-india",
            ""
        )
    )
    .unwrap();
    writeln!(
        f,
        r#"{{"type":"custom-title","customTitle":"Secret title foxtrot","sessionId":"{sess}"}}"#
    )
    .unwrap();
    drop(f);
    std::fs::write(
        proj.join(format!("{sess}/subagents/agent-secret-agent-mike.jsonl")),
        assistant(
            sess,
            home,
            "m3",
            "2026-11-12T12:00:00Z",
            "claude-sonnet-5-5",
            r#","agentId":"secret-agent-mike""#,
        ) + "\n",
    )
    .unwrap();
    let mut idx = Index::open_in_memory().unwrap();
    let s = idx.refresh(dir.path(), &|_, _| {}).unwrap();
    assert_eq!(s.new_requests, 3);
    (dir, idx)
}

fn opts(projects: Projects) -> ExportOptions {
    ExportOptions {
        from: "2026-11-02".into(),
        to: "2026-11-16".into(),
        name: Some("Ana".into()),
        projects,
    }
}

#[test]
fn summary_contains_no_private_strings() {
    let (dir, idx) = secret_index();
    let tmp = dir.path().to_string_lossy().to_lowercase();
    for (p, allowed) in [
        (Projects::None, None),
        (
            Projects::Hashed {
                salt: SALT.to_vec(),
            },
            None,
        ),
        (
            Projects::Named {
                salt: SALT.to_vec(),
            },
            Some("secret-project-charlie"),
        ),
    ] {
        let s = share::export_summary(&idx.conn, &opts(p)).unwrap();
        assert_eq!(s.totals.requests, 3);
        let json = share::to_json(&s).to_lowercase();
        for secret in SECRETS {
            if Some(secret) == allowed {
                continue;
            }
            assert!(!json.contains(secret), "{secret:?} leaked:\n{json}");
        }
        assert!(!json.contains(&tmp), "temp path leaked");
        assert!(!json.contains("/users/"), "a path leaked");
        assert!(!json.contains("bash"), "a tool name leaked");
        if let Some(name) = allowed {
            // Opted in: the folder name, never the path above it.
            assert!(json.contains(&format!("\"name\": \"{name}\"")));
        }
    }
}

#[test]
fn unknown_values_fold_into_other() {
    let (_dir, idx) = secret_index();
    let s = share::export_summary(&idx.conn, &opts(Projects::None)).unwrap();
    let keys = |g: &str| -> Vec<String> { s.groups[g].iter().map(|x| x.key.clone()).collect() };
    assert_eq!(keys("entrypoint"), ["other"]);
    assert_eq!(keys("effort"), ["other"]);
    assert_eq!(keys("family"), ["opus", "other", "sonnet"]);
    assert_eq!(keys("agent"), ["main", "subagent"]);
    // Two projects in transcripts, one after worktrees fold into the repo.
    let h = share::export_summary(
        &idx.conn,
        &opts(Projects::Hashed {
            salt: SALT.to_vec(),
        }),
    )
    .unwrap();
    let ps = h.projects.unwrap();
    assert_eq!(ps.len(), 1);
    assert_eq!(ps[0].id.len(), 12);
    // Same salt, same id; another salt, another id.
    let again = share::export_summary(
        &idx.conn,
        &opts(Projects::Hashed {
            salt: SALT.to_vec(),
        }),
    )
    .unwrap();
    assert_eq!(again.projects.unwrap()[0].id, ps[0].id);
    let other = share::export_summary(
        &idx.conn,
        &opts(Projects::Hashed {
            salt: b"another salt, sixteen+".to_vec(),
        }),
    )
    .unwrap();
    assert_ne!(other.projects.unwrap()[0].id, ps[0].id);
}

fn row(key: &str, day: &str, dow: u8, hour: u8, model: &str, out: u64, read: u64) -> Row {
    Row {
        key: key.into(),
        session: "s".into(),
        day: day.into(),
        hour,
        dow,
        model: model.into(),
        project: "/r/app".into(),
        cwd: "/r/app".into(),
        input: 2,
        output: out,
        cache_write: 10,
        cache_read: read,
        ..Default::default()
    }
}

#[test]
fn export_totals_and_round_trip() {
    let mut idx = Index::open_in_memory().unwrap();
    idx.insert_rows(
        "f",
        &[
            row("a", "2026-11-09", 0, 9, "claude-opus-5-5", 100, 1000),
            row("b", "2026-11-09", 0, 14, "claude-sonnet-5-5", 20, 200),
            row("c", "2026-11-11", 2, 9, "claude-opus-5-5", 50, 500),
            row("old", "2026-10-01", 2, 9, "claude-opus-5-5", 1, 1),
        ],
    )
    .unwrap();
    let s = share::export_summary(
        &idx.conn,
        &ExportOptions {
            from: "2026-11-09".into(),
            to: "2026-11-15".into(),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(s.schema, share::SCHEMA);
    assert_eq!(s.name, None);
    assert_eq!(s.totals.requests, 3);
    assert_eq!(s.totals.output, 170);
    assert_eq!(s.days.len(), 2);
    assert_eq!(s.days[0].by_family["opus"].output, 100);
    assert_eq!(s.days[0].by_family["sonnet"].output, 20);
    assert_eq!(s.days[0].totals.output_per_request, 60.0);
    assert_eq!(s.weekday_share.len(), 7);
    assert_eq!(s.hour_share.len(), 24);
    assert!((s.weekday_share.iter().sum::<f64>() - 1.0).abs() < 1e-3);
    assert!(s.hour_share[9] > s.hour_share[14]);
    assert!(s.projects.is_none());

    let back = share::parse_summary(&share::to_json(&s)).unwrap();
    assert_eq!(back, s);

    // Unknown fields from a newer minor version are ignored.
    let mut v = serde_json::to_value(&s).unwrap();
    v["schema"] = "claude-usage-summary/1.3".into();
    v["somethingNew"] = 1.into();
    assert!(share::parse_summary(&v.to_string()).is_ok());
    // Another major version, or another file, is rejected.
    v["schema"] = "claude-usage-summary/2".into();
    assert!(share::parse_summary(&v.to_string()).is_err());
    assert!(share::parse_summary(r#"{"hello":1}"#).is_err());
}

fn m(requests: u64, output: u64, read: u64) -> Metrics {
    let mut m = Metrics {
        requests,
        output,
        cache_read: read,
        ..Default::default()
    };
    m.finish();
    m
}

fn person(name: Option<&str>, from: &str, to: &str, days: &[(&str, Metrics, Metrics)]) -> Summary {
    let mut total = Metrics::default();
    let days = days
        .iter()
        .map(|(d, opus, sonnet)| {
            let mut t = opus.clone();
            t.add(sonnet);
            total.add(&t);
            share::DaySummary {
                day: d.to_string(),
                totals: t,
                by_family: BTreeMap::from([
                    ("opus".to_string(), opus.clone()),
                    ("sonnet".to_string(), sonnet.clone()),
                ]),
            }
        })
        .collect();
    Summary {
        schema: share::SCHEMA.into(),
        app_version: "0.1.0".into(),
        generated_at: "2026-11-18T10:00:00Z".into(),
        name: name.map(Into::into),
        from: from.into(),
        to: to.into(),
        totals: total,
        days,
        weekday_share: vec![],
        hour_share: vec![],
        groups: BTreeMap::new(),
        projects: None,
    }
}

#[test]
fn compare_weeks_side_by_side() {
    let z = Metrics::default();
    // Ana: two full weeks, then Mon to Wed of the current week.
    let ana = person(
        Some("Ana"),
        "2026-11-02",
        "2026-11-18",
        &[
            ("2026-11-03", m(10, 1000, 100_000), m(10, 500, 50_000)),
            ("2026-11-10", m(20, 3000, 300_000), z.clone()),
            ("2026-11-14", m(10, 1000, 100_000), z.clone()), // Saturday
            ("2026-11-17", m(30, 3000, 600_000), z.clone()),
        ],
    );
    // Ben: starts mid-week, so his first week has no fair comparison.
    let ben = person(
        None,
        "2026-11-05",
        "2026-11-18",
        &[("2026-11-12", m(5, 500, 50_000), m(5, 500, 50_000))],
    );
    let dir = tempfile::tempdir().unwrap();
    let pa = dir.path().join("ana.json");
    let pb = dir.path().join("ben.json");
    let pc = dir.path().join("ben2.json");
    std::fs::write(&pa, share::to_json(&ana)).unwrap();
    std::fs::write(&pb, share::to_json(&ben)).unwrap();
    let mut ben_named = ben.clone();
    ben_named.name = Some("ben".into());
    std::fs::write(&pc, share::to_json(&ben_named)).unwrap();

    let people: Vec<Loaded> = share::load_summaries(&[&pa, &pb, &pc]).unwrap();
    let labels: Vec<_> = people.iter().map(|p| p.label.as_str()).collect();
    assert_eq!(labels, ["Ana", "ben", "ben (2)"]);

    let weeks = share::default_weeks(&people, 3);
    assert_eq!(weeks[0].start, "2026-11-02");
    assert_eq!(weeks[2].start, "2026-11-16");
    assert_eq!(weeks[2].end, "2026-11-22");

    let c = share::compare(&people, &weeks, &CompareOptions::default()).unwrap();
    let a = &c.people[0].cells;
    assert_eq!(a[0].totals.requests, 20);
    assert!(a[0].previous.is_none(), "no data before the first week");
    assert_eq!(a[1].totals.requests, 30);
    assert_eq!(a[1].change.requests, Some(50.0));
    assert_eq!(a[1].totals.context_per_request, 13_333.3);
    // Current week: Mon to Wed, against Mon to Wed of last week (20 requests).
    assert!(a[2].partial);
    assert_eq!(a[2].days_covered, 3);
    assert_eq!(a[2].previous.as_ref().unwrap().requests, 20);
    assert_eq!(a[2].change.requests, Some(50.0));
    assert_eq!(a[2].change.context_per_request, Some(33.3));
    assert_eq!(a[2].change.output_per_request, Some(-33.3));

    let b = &c.people[1].cells;
    assert!(b[0].partial);
    assert_eq!(b[0].days_covered, 4);
    assert!(b[1].previous.is_none(), "previous week only partly covered");
    assert_eq!(b[1].totals.requests, 10);

    // One family only.
    let opus = share::compare(
        &people,
        &weeks,
        &CompareOptions {
            family: Some("Opus".into()),
        },
    )
    .unwrap();
    assert_eq!(opus.family.as_deref(), Some("opus"));
    assert_eq!(opus.people[0].cells[0].totals.requests, 10);
    assert_eq!(opus.people[1].cells[1].totals.output, 500);
}

#[test]
fn tables_as_csv_and_json() {
    let mut idx = Index::open_in_memory().unwrap();
    idx.insert_rows(
        "f",
        &[row("a", "2026-11-09", 0, 9, "claude-opus-5-5", 100, 1000)],
    )
    .unwrap();
    idx.conn
        .execute(
            "INSERT INTO sessions(id, title) VALUES ('s', 'Fix \"auth\", again')",
            [],
        )
        .unwrap();
    let f = usage_core::Filter {
        from: Some("2026-11-09".into()),
        to: Some("2026-11-10".into()),
        ..Default::default()
    };
    let d = share::table(&idx.conn, &f, "daily", 10).unwrap().to_csv();
    let lines: Vec<_> = d.lines().collect();
    assert_eq!(lines.len(), 3);
    assert!(lines[0].starts_with("day,weekday,requests,"));
    assert!(lines[1].starts_with("2026-11-09,Mon,1,2,100,"));

    let s = share::table(&idx.conn, &f, "by-session", 10).unwrap();
    assert!(s
        .to_csv()
        .lines()
        .nth(1)
        .unwrap()
        .starts_with(r#"s,"Fix ""auth"", again",app,1,"#));
    let j: serde_json::Value = serde_json::from_str(&s.to_json()).unwrap();
    assert_eq!(j[0]["label"], "Fix \"auth\", again");

    assert!(share::table(&idx.conn, &f, "by-nothing", 10).is_err());
}

#[test]
fn salt_is_created_once() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("sub/share-salt");
    let a = share::load_or_create_salt(&p).unwrap();
    let b = share::load_or_create_salt(&p).unwrap();
    assert_eq!(a.len(), 32);
    assert_eq!(a, b);
}
