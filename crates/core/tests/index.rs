use std::io::Write;
use usage_core::{query, Filter, Index};

fn line(msg: &str, req: &str, ts: &str, out: u64, block: &str) -> String {
    format!(
        r#"{{"type":"assistant","timestamp":"{ts}","cwd":"/r/app","sessionId":"s1","requestId":"{req}","message":{{"id":"{msg}","model":"claude-opus-5-5","content":[{{"type":"{block}"}}],"usage":{{"input_tokens":1,"output_tokens":{out},"cache_creation_input_tokens":10,"cache_read_input_tokens":100}}}}}}"#
    )
}

#[test]
fn incremental_refresh_counts_each_response_once() {
    let dir = tempfile::tempdir().unwrap();
    let proj = dir.path().join("-r-app");
    std::fs::create_dir_all(proj.join("s1/subagents")).unwrap();
    let main = proj.join("s1.jsonl");
    let mut f = std::fs::File::create(&main).unwrap();
    // One response written as two lines, plus a title.
    writeln!(
        f,
        "{}",
        line("m1", "r1", "2026-11-16T08:00:00Z", 50, "thinking")
    )
    .unwrap();
    writeln!(
        f,
        "{}",
        line("m1", "r1", "2026-11-16T08:00:00Z", 50, "text")
    )
    .unwrap();
    writeln!(
        f,
        r#"{{"type":"custom-title","customTitle":"Usage app","sessionId":"s1"}}"#
    )
    .unwrap();
    // A line still being written: no trailing newline.
    write!(
        f,
        "{}",
        &line("m2", "r2", "2026-11-16T09:00:00Z", 7, "text")[..40]
    )
    .unwrap();
    drop(f);
    std::fs::write(
        proj.join("s1/subagents/agent-a.jsonl"),
        line("m3", "r3", "2026-11-13T09:00:00Z", 5, "text") + "\n",
    )
    .unwrap();

    let mut idx = Index::open_in_memory().unwrap();
    let s = idx.refresh(dir.path(), &|_, _| {}).unwrap();
    assert_eq!((s.files_seen, s.files_read, s.new_requests), (2, 2, 2));

    // Nothing changed: nothing read.
    let s = idx.refresh(dir.path(), &|_, _| {}).unwrap();
    assert_eq!(s.files_read, 0);

    // Finish the partial line and add a repeat of m1's block.
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&main)
        .unwrap();
    let full = line("m2", "r2", "2026-11-16T09:00:00Z", 7, "text");
    writeln!(f, "{}", &full[40..]).unwrap();
    writeln!(
        f,
        "{}",
        line("m1", "r1", "2026-11-16T08:00:00Z", 50, "tool_use")
    )
    .unwrap();
    drop(f);
    let s = idx.refresh(dir.path(), &|_, _| {}).unwrap();
    assert_eq!((s.files_read, s.new_requests), (1, 1));

    let t = query::totals(&idx.conn, &Filter::default()).unwrap();
    assert_eq!(t.requests, 3);
    assert_eq!(t.output, 62);

    let sub = Filter {
        subagent: Some(true),
        ..Default::default()
    };
    assert_eq!(query::totals(&idx.conn, &sub).unwrap().requests, 1);

    let g = query::breakdown(&idx.conn, &Filter::default(), usage_core::Dim::Session, 5).unwrap();
    assert_eq!(g[0].label, "Usage app");
    assert_eq!(g[0].detail.as_deref(), Some("app"));
}
