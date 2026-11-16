//! `usage`: refresh the index and print token usage from the terminal.
//!
//! usage [daily|summary|by <dim>|models|cost-check] [--days N] [--from D] [--to D]
//!       [--weekdays] [--model S] [--project PATH] [--limit N] [--json]

use std::io::IsTerminal;
use usage_core::{cost, fmt, paths, query, Dim, Filter, Index};

const HELP: &str = "usage: usage [daily|summary|by <dim>|models] [options]

commands
  daily            one row per day (default)
  summary          this week against last week
  by <dim>         totals grouped by model, project, session, effort,
                   agent, hour, weekday, entrypoint, skill, plugin,
                   branch or modelFallback (requested vs actual model)
  by size          share of usage by session size (requests, peak context)
  by cache         cache hit rate and 5-minute vs 1-hour writes per day
  models           models in the index
  session <id>     what filled one session's context, and warnings
                   --turns: one row per prompt; --pieces: the 20 pieces
                   that carried the most context
  cost-check       compare the price table with Claude Code's own cost
                   estimate (cost-state) for every session that has one
  insights         what to change, ranked by tokens, and why usage moved
  export ...       shareable summary or table export; `usage export -h`
  compare a.json b.json ...
                   people side by side per week; `usage compare -h`

options
  --days N         last N days, ending today (default 14)
  --from D --to D  explicit range, YYYY-MM-DD
  --weekdays       leave out Saturdays and Sundays
  --model S        only models whose id contains S, e.g. opus
  --project PATH   only one project (repo root)
  --limit N        rows for `by` (default 20)
  --json           print JSON
  --no-refresh     query the index without reading new transcript lines

environment
  USAGE_INDEX_PATH use this index file instead of the app's";

fn main() {
    if let Err(e) = run() {
        eprintln!("usage: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    match std::env::args().nth(1).as_deref() {
        Some("export") => return share_cli::export(std::env::args().skip(2).collect()),
        Some("compare") => return share_cli::compare(std::env::args().skip(2).collect()),
        _ => {}
    }
    let mut args = std::env::args().skip(1).peekable();
    let mut cmd = String::from("daily");
    let mut dim = None;
    let mut sid = String::new();
    let mut f = Filter::default();
    let mut days: i64 = 14;
    let (mut json, mut refresh, mut limit) = (false, true, 20usize);
    let (mut turns, mut pieces) = (false, false);
    let mut first = true;
    while let Some(a) = args.next() {
        let mut val = |name: &str| args.next().ok_or(format!("{name} needs a value"));
        match a.as_str() {
            "-h" | "--help" => {
                println!("{HELP}");
                return Ok(());
            }
            "--days" => {
                days = val("--days")?
                    .parse()
                    .map_err(|_| "--days needs a number")?
            }
            "--from" => f.from = Some(val("--from")?),
            "--to" => f.to = Some(val("--to")?),
            "--weekdays" => f.weekdays_only = true,
            "--model" => f.model = Some(val("--model")?),
            "--project" => f.project = Some(val("--project")?),
            "--limit" => {
                limit = val("--limit")?
                    .parse()
                    .map_err(|_| "--limit needs a number")?
            }
            "--json" => json = true,
            "--no-refresh" => refresh = false,
            "--turns" => turns = true,
            "--pieces" => pieces = true,
            "by" if first => {
                cmd = "by".into();
                let d = val("by")?;
                match d.as_str() {
                    "size" | "sizes" => cmd = "by size".into(),
                    "cache" => cmd = "by cache".into(),
                    _ => dim = Some(Dim::parse(&d).ok_or(format!("unknown dimension {d:?}"))?),
                }
            }
            "session" if first => {
                cmd = "session".into();
                sid = val("session")?;
            }
            c if first && !c.starts_with('-') => cmd = c.to_string(),
            other => return Err(format!("unknown argument {other:?}\n\n{HELP}")),
        }
        first = false;
    }
    if f.from.is_none() {
        let to = match &f.to {
            Some(s) => s.parse().map_err(|e| format!("--to: {e}"))?,
            None => query::today(),
        };
        f.from = Some(
            to.checked_sub(jiff::Span::new().days(days - 1))
                .map_err(|e| e.to_string())?
                .to_string(),
        );
    }

    if cmd == "cost-check" {
        return cost_check(json, limit);
    }

    let mut idx = Index::open(&paths::index_path()).map_err(|e| e.to_string())?;
    if refresh {
        let tty = std::io::stderr().is_terminal();
        let s = idx
            .refresh(&paths::projects_dir(), &|d, t| {
                if tty && t > 50 && (d % 25 == 0 || d == t) {
                    eprint!("\rreading transcripts {d}/{t}");
                }
            })
            .map_err(|e| e.to_string())?;
        if tty && s.files_read > 50 {
            eprintln!();
        }
    }
    let c = &idx.conn;
    let e = |e: usage_core::Error| e.to_string();
    let out = |v: serde_json::Value| println!("{}", serde_json::to_string_pretty(&v).unwrap());

    match cmd.as_str() {
        "daily" => {
            let rows = query::daily(c, &f).map_err(e)?;
            if json {
                out(serde_json::to_value(rows).unwrap());
                return Ok(());
            }
            println!(
                "{:<14} {:>8} {:>9} {:>9} {:>11} {:>11} {:>10} {:>9} {:>9}",
                "day",
                "requests",
                "cost",
                "output",
                "cache wr",
                "cache rd",
                "total",
                "out/req",
                "ctx/req"
            );
            for r in rows {
                let t = &r.totals;
                println!(
                    "{:<14} {:>8} {:>9} {:>9} {:>11} {:>11} {:>10} {:>9.0} {:>9}",
                    format!("{} {}", r.weekday, &r.day[5..]),
                    t.requests,
                    usage_core::cost::format_usd(t.cost_usd),
                    fmt::tokens(t.output),
                    fmt::tokens(t.cache_write),
                    fmt::tokens(t.cache_read),
                    fmt::tokens(t.total),
                    t.output_per_request,
                    fmt::tokens(t.context_per_request as u64),
                );
            }
        }
        "summary" => {
            let o = query::overview(c, &f, &jiff::Zoned::now()).map_err(e)?;
            if json {
                out(serde_json::to_value(o).unwrap());
                return Ok(());
            }
            let line = |name: &str, t: &usage_core::Totals, vs: Option<&usage_core::Totals>| {
                let ch = vs
                    .and_then(|b| fmt::change(b.cost_usd, t.cost_usd))
                    .map(|p| format!("{p:+.0}%"))
                    .unwrap_or_default();
                println!(
                    "{name:<26} {:>8} req {:>9} {:>9} tokens {:>6}   {:>5.0} out/req {:>6} ctx/req",
                    t.requests,
                    usage_core::cost::format_usd(t.cost_usd),
                    fmt::tokens(t.total),
                    ch,
                    t.output_per_request,
                    fmt::tokens(t.context_per_request as u64)
                );
            };
            line("today", &o.today_totals, Some(&o.same_day_last_week));
            line(
                &format!("same day last week, to {}", o.as_of),
                &o.same_day_last_week,
                None,
            );
            line("this week so far", &o.this_week, Some(&o.last_week_to_date));
            line(
                &format!("last week, to the same {}", o.as_of),
                &o.last_week_to_date,
                None,
            );
            line("last week", &o.last_week, None);
        }
        "by" => {
            let g = query::breakdown(c, &f, dim.unwrap(), limit).map_err(e)?;
            if json {
                out(serde_json::to_value(g).unwrap());
                return Ok(());
            }
            for x in g {
                println!(
                    "{:<44} {:>8} req {:>9} {:>5.1}%  {:>5.0} out/req",
                    x.label.chars().take(44).collect::<String>(),
                    x.totals.requests,
                    fmt::tokens(x.totals.total),
                    x.share * 100.0,
                    x.totals.output_per_request
                );
            }
        }
        "by size" => {
            let s = query::session_size_buckets(c, &f).map_err(e)?;
            if json {
                out(serde_json::to_value(s).unwrap());
                return Ok(());
            }
            let table = |name: &str, bs: &[query::SizeBucket]| {
                println!(
                    "{name:<14} {:>8} {:>7} {:>9} {:>9} {:>7}",
                    "sessions", "share", "requests", "total", "share"
                );
                for b in bs {
                    println!(
                        "{:<14} {:>8} {:>6.1}% {:>9} {:>9} {:>6.1}%",
                        b.label,
                        b.sessions,
                        b.session_share * 100.0,
                        b.totals.requests,
                        fmt::tokens(b.totals.total),
                        b.share * 100.0
                    );
                }
            };
            table("requests", &s.by_requests);
            println!();
            table("peak context", &s.by_peak_context);
        }
        "by cache" => {
            let s = query::cache_stats(c, &f).map_err(e)?;
            if json {
                out(serde_json::to_value(s).unwrap());
                return Ok(());
            }
            println!(
                "{:<14} {:>8} {:>7} {:>10} {:>9} {:>9} {:>9} {:>6}",
                "day", "requests", "hit", "cache rd", "write 5m", "write 1h", "unsplit", "1h"
            );
            let line = |name: &str, d: &query::CacheDay| {
                println!(
                    "{:<14} {:>8} {:>6.1}% {:>10} {:>9} {:>9} {:>9} {:>5.0}%",
                    name,
                    d.requests,
                    d.hit_rate * 100.0,
                    fmt::tokens(d.cache_read),
                    fmt::tokens(d.write_5m),
                    fmt::tokens(d.write_1h),
                    fmt::tokens(d.write_unsplit),
                    d.share_1h * 100.0
                );
            };
            for d in &s.days {
                line(&d.day, d);
            }
            line("total", &s.total);
        }
        "session" => {
            let d = usage_core::session::analyze(c, &sid).map_err(e)?;
            if json {
                out(serde_json::to_value(d).unwrap());
                return Ok(());
            }
            println!(
                "{} · {} prompts, {} requests, avg context {}, peak {}, {} compactions",
                d.title.as_deref().unwrap_or(&d.id),
                d.prompts,
                d.requests,
                fmt::tokens(d.avg_context as u64),
                fmt::tokens(d.peak_context),
                d.compactions
            );
            if let Some(c) = &d.cost {
                println!("Claude Code estimate ${:.2}", c.usd);
            }
            println!(
                "{} attribution: {} of {} requests from request snapshots",
                d.mode, d.exact_requests, d.requests
            );
            println!("\nwhat filled the context");
            for s in d.composition.iter().take(15) {
                println!(
                    "  {:<44} {:<13} {:>9} {:>6.1}%",
                    s.label,
                    s.group,
                    fmt::tokens(s.tokens as u64),
                    s.share * 100.0
                );
            }
            println!("\nwarnings");
            for w in &d.warnings {
                println!("  [{}] {}\n        {}", w.level, w.title, w.detail);
            }
            if turns {
                print_turns(&d);
            }
            if pieces {
                print_pieces(&d);
            }
            println!(
                "\n{} subagents, {} events, {} timeline points",
                d.subagents.len(),
                d.events.len(),
                d.timeline.len()
            );
        }
        "insights" => {
            let r = usage_core::insights::report(c, &f, &jiff::Zoned::now(), &Default::default())
                .map_err(e)?;
            if json {
                out(serde_json::to_value(r).unwrap());
                return Ok(());
            }
            print_insights(&r);
        }
        "models" => {
            for m in query::models(c).map_err(e)? {
                println!("{m}");
            }
        }
        other => return Err(format!("unknown command {other:?}\n\n{HELP}")),
    }
    Ok(())
}

fn hhmm(ms: i64) -> String {
    jiff::Timestamp::from_millisecond(ms)
        .map(|t| {
            t.to_zoned(jiff::tz::TimeZone::system())
                .strftime("%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

fn print_turns(d: &usage_core::session::SessionDetail) {
    println!("\nturns");
    println!(
        "  {:>4} {:<11} {:<17} {:>4} {:>9} {:>9} {:>7}  tools / flags",
        "#", "time", "origin", "req", "context", "added", "output"
    );
    for t in &d.turns {
        let mut tools: Vec<_> = t.tools.iter().collect();
        tools.sort_by_key(|x| std::cmp::Reverse(x.calls));
        let tools: Vec<String> = tools
            .iter()
            .take(4)
            .map(|x| format!("{}×{}", x.name, x.calls))
            .collect();
        println!(
            "  {:>4} {:<11} {:<17} {:>4} {:>9} {:>9} {:>7}  {}{}{}",
            t.index,
            hhmm(t.ts),
            t.origin.chars().take(17).collect::<String>(),
            t.requests,
            fmt::tokens(t.context_tokens),
            fmt::tokens(t.context_added as u64),
            fmt::tokens(t.output),
            tools.join(" "),
            if t.flags.is_empty() { "" } else { "  " },
            t.flags.join(",")
        );
    }
}

fn print_pieces(d: &usage_core::session::SessionDetail) {
    println!("\nlargest pieces by context carried");
    println!(
        "  {:<28} {:<12} {:<26} {:>8} {:>6} {:>9}  time",
        "piece", "group", "tool", "size", "reqs", "carried"
    );
    for p in &d.top_pieces {
        println!(
            "  {:<28} {:<12} {:<26} {:>8} {:>6} {:>9}  {}",
            p.label.chars().take(28).collect::<String>(),
            p.group,
            p.tool
                .as_deref()
                .unwrap_or("")
                .chars()
                .take(26)
                .collect::<String>(),
            fmt::tokens(p.tokens as u64),
            p.requests,
            fmt::tokens(p.carried_tokens as u64),
            hhmm(p.ts)
        );
    }
}

/// `usage cost-check`: share of sessions where the price table agrees with
/// Claude Code's `cost-state` within 10%, and the worst outliers.
fn cost_check(json: bool, limit: usize) -> Result<(), String> {
    let t = cost::PriceTable::load().map_err(|e| e.to_string())?;
    let r = cost::check(&t, &paths::projects_dir()).map_err(|e| e.to_string())?;
    if json {
        println!("{}", serde_json::to_string_pretty(&r).unwrap());
        return Ok(());
    }
    let pct = |s: &cost::Share| format!("{} of {} ({:.1}%)", s.within, s.compared, s.share * 100.0);
    println!("prices from {} ({})", t.source, t.retrieved);
    println!(
        "sessions within {:.0}% of Claude Code's estimate (cost-state):",
        r.tolerance * 100.0
    );
    println!("  all sessions                      {}", pct(&r.all));
    println!(
        "  current records (Claude Code 2.1.317+, have 5m/1h split)  {}",
        pct(&r.current_format)
    );
    println!(
        "  older records (no 5m/1h split)    {}",
        pct(&r.legacy_format)
    );
    println!(
        "  explained (older records also checked at the 0.1x cache-read rate older Claude Code used)  {}",
        pct(&r.explained)
    );
    if r.unpriced > 0 {
        println!(
            "{} sessions not compared, no price for: {}",
            r.unpriced,
            r.unknown_models.join(", ")
        );
    }
    let worst: Vec<_> = r
        .results
        .iter()
        .filter(|s| s.diff.is_some_and(|d| d.abs() > r.tolerance))
        .filter(|s| !s.explained(r.tolerance))
        .take(limit)
        .collect();
    if !worst.is_empty() {
        println!("\nworst unexplained outliers (computed vs Claude Code)");
        for s in worst {
            let models: Vec<String> = s
                .models
                .iter()
                .map(|m| match (m.computed_usd, m.reported_usd > 0.0) {
                    (Some(c), true) => {
                        format!(
                            "{} {:+.1}%",
                            m.model,
                            (c - m.reported_usd) / m.reported_usd * 100.0
                        )
                    }
                    _ => m.model.clone(),
                })
                .collect();
            println!(
                "  {}  {:+7.1}%  {}",
                s.session_id,
                s.diff.unwrap() * 100.0,
                models.join(", ")
            );
        }
    }
    Ok(())
}

fn print_insights(r: &usage_core::insights::Report) {
    println!(
        "Insights {} to {} · {} tokens · {} sessions ({} analyzed, {} just now)",
        r.from.as_deref().unwrap_or("start"),
        r.to.as_deref().unwrap_or("today"),
        fmt::tokens(r.totals.total),
        r.sessions,
        r.analyzed,
        r.analyzed_now
    );
    if let Some(c) = &r.change {
        println!("\n{}: {}", c.periods.label, c.explanation);
    }
    if r.insights.is_empty() {
        println!("\nNothing stands out.");
    }
    for (n, i) in r.insights.iter().enumerate() {
        println!(
            "\n{:>2}. [{}] {}{} · {} ({:.1}%)\n    {}\n    → {}",
            n + 1,
            serde_json::to_value(i.severity)
                .unwrap()
                .as_str()
                .unwrap_or(""),
            i.title,
            i.subject_label
                .as_deref()
                .map(|s| format!(": {s}"))
                .unwrap_or_default(),
            fmt::tokens(i.amount.max(0.0) as u64),
            i.share * 100.0,
            i.finding,
            i.action
        );
    }
    if !r.skipped.is_empty() {
        println!("\nNot checked yet:");
        for s in &r.skipped {
            println!("  {} ({})", s.title, s.reason);
        }
    }
}

mod share_cli {
    use std::io::IsTerminal;
    use usage_core::share::{self, CompareOptions, ExportOptions, Projects};
    use usage_core::{fmt, paths, query, Filter, Index};

    const EXPORT_HELP: &str = "usage export: write what you can share, or a table

  usage export --preview [options]       print exactly what --summary would write
  usage export --summary FILE [options]  write a shareable summary (JSON)
  usage export --csv TABLE FILE          write a table as CSV
  usage export --json TABLE FILE         write a table as JSON

A summary has per-day totals by model family, per-request metrics, weekday and
hour shares and group shares. It never contains paths, project names, session
ids or titles, branches, prompts or tool arguments. Tables are for your own use
and do contain project paths and session titles.

TABLE is daily or by-<dim>: by-model, by-project, by-session, by-effort,
by-agent, by-hour, by-weekday, by-entrypoint. FILE - means standard output.

options
  --days N             last N days, ending today (summary default: this week
                       and the four before it; tables: 14 days)
  --from D --to D      explicit range, YYYY-MM-DD
  --name X             display name shown to whoever compares (summary)
  --hashed-projects    add per-project rows, identified by a salted hash
  --include-projects   add per-project rows with folder names (opt in)
  --model S --project PATH --weekdays --limit N   table filters
  --no-refresh         do not read new transcript lines first";

    const COMPARE_HELP: &str = "usage compare: people side by side, week by week

  usage compare a.json b.json [...] [--weeks N] [--family F] [--json]

  --weeks N    weeks to show, ending with the latest week any file covers (4)
  --family F   only one model family: opus, sonnet, haiku, fable, other

Changes are against the previous week. A week a summary only partly covers is
marked * and compared with the same weekdays a week earlier.";

    fn open(refresh: bool) -> Result<Index, String> {
        let mut idx = Index::open(&paths::index_path()).map_err(|e| e.to_string())?;
        if refresh {
            let tty = std::io::stderr().is_terminal();
            let s = idx
                .refresh(&paths::projects_dir(), &|d, t| {
                    if tty && t > 50 && (d % 25 == 0 || d == t) {
                        eprint!("\rreading transcripts {d}/{t}");
                    }
                })
                .map_err(|e| e.to_string())?;
            if tty && s.files_read > 50 {
                eprintln!();
            }
        }
        Ok(idx)
    }

    fn write(file: &str, text: &str) -> Result<(), String> {
        if file == "-" {
            print!("{text}");
            Ok(())
        } else {
            std::fs::write(file, text).map_err(|e| format!("{file}: {e}"))?;
            eprintln!("wrote {file}");
            Ok(())
        }
    }

    pub fn export(args: Vec<String>) -> Result<(), String> {
        let mut it = args.into_iter();
        let (mut summary, mut preview, mut table) = (None, false, None);
        let (mut days, mut name, mut projects) = (None::<i64>, None, 0u8);
        let (mut refresh, mut limit) = (true, 1000usize);
        let mut f = Filter::default();
        while let Some(a) = it.next() {
            let mut val = |n: &str| it.next().ok_or(format!("{n} needs a value"));
            match a.as_str() {
                "-h" | "--help" => {
                    println!("{EXPORT_HELP}");
                    return Ok(());
                }
                "--summary" => summary = Some(val("--summary")?),
                "--preview" => preview = true,
                "--csv" | "--json" => {
                    let t = val(&a)?;
                    table = Some((a.clone(), t, val(&a)?));
                }
                "--days" => {
                    days = Some(
                        val("--days")?
                            .parse()
                            .map_err(|_| "--days needs a number")?,
                    )
                }
                "--from" => f.from = Some(val("--from")?),
                "--to" => f.to = Some(val("--to")?),
                "--name" => name = Some(val("--name")?),
                "--hashed-projects" => projects = projects.max(1),
                "--include-projects" => projects = 2,
                "--model" => f.model = Some(val("--model")?),
                "--project" => f.project = Some(val("--project")?),
                "--weekdays" => f.weekdays_only = true,
                "--limit" => {
                    limit = val("--limit")?
                        .parse()
                        .map_err(|_| "--limit needs a number")?
                }
                "--no-refresh" => refresh = false,
                other => return Err(format!("unknown argument {other:?}\n\n{EXPORT_HELP}")),
            }
        }
        if summary.is_none() && !preview && table.is_none() {
            return Err(EXPORT_HELP.to_string());
        }
        let today = query::today();
        let to: jiff::civil::Date = match &f.to {
            Some(s) => s.parse().map_err(|e| format!("--to: {e}"))?,
            None => today,
        };
        let from_days = |n: i64| -> Result<String, String> {
            Ok(to
                .checked_sub(jiff::Span::new().days(n.max(1) - 1))
                .map_err(|e| e.to_string())?
                .to_string())
        };
        let idx = open(refresh)?;
        let c = &idx.conn;
        let e = |e: usage_core::Error| e.to_string();

        if let Some((kind, t, file)) = table {
            let mut tf = f.clone();
            tf.to = Some(to.to_string());
            if tf.from.is_none() {
                tf.from = Some(from_days(days.unwrap_or(14))?);
            }
            let tbl = share::table(c, &tf, &t, limit).map_err(e)?;
            let text = if kind == "--csv" {
                tbl.to_csv()
            } else {
                tbl.to_json() + "\n"
            };
            write(&file, &text)?;
        }
        if summary.is_some() || preview {
            if f.model.is_some() || f.project.is_some() || f.weekdays_only {
                return Err("a summary covers all usage; --model, --project and --weekdays apply to tables only".into());
            }
            let from = match (&f.from, days) {
                (Some(s), _) => s.clone(),
                (None, Some(n)) => from_days(n)?,
                (None, None) => share::default_range(to, 4).0,
            };
            let projects = match projects {
                0 => Projects::None,
                n => {
                    let salt = share::load_or_create_salt(&paths::data_dir().join("share-salt"))
                        .map_err(e)?;
                    if n == 1 {
                        Projects::Hashed { salt }
                    } else {
                        Projects::Named { salt }
                    }
                }
            };
            let s = share::export_summary(
                c,
                &ExportOptions {
                    from,
                    to: to.to_string(),
                    name,
                    projects,
                },
            )
            .map_err(e)?;
            let text = share::to_json(&s) + "\n";
            if preview {
                print!("{text}");
            }
            if let Some(file) = summary {
                write(&file, &text)?;
            }
        }
        Ok(())
    }

    pub fn compare(args: Vec<String>) -> Result<(), String> {
        let mut it = args.into_iter();
        let (mut files, mut weeks, mut json) = (Vec::new(), 4usize, false);
        let mut opts = CompareOptions::default();
        while let Some(a) = it.next() {
            let mut val = |n: &str| it.next().ok_or(format!("{n} needs a value"));
            match a.as_str() {
                "-h" | "--help" => {
                    println!("{COMPARE_HELP}");
                    return Ok(());
                }
                "--weeks" => {
                    weeks = val("--weeks")?
                        .parse()
                        .map_err(|_| "--weeks needs a number")?
                }
                "--family" => opts.family = Some(val("--family")?),
                "--json" => json = true,
                s if !s.starts_with('-') => files.push(s.to_string()),
                other => return Err(format!("unknown argument {other:?}\n\n{COMPARE_HELP}")),
            }
        }
        if files.is_empty() {
            return Err(COMPARE_HELP.to_string());
        }
        let people = share::load_summaries(&files).map_err(|e| e.to_string())?;
        let w = share::default_weeks(&people, weeks.max(1));
        let cmp = share::compare(&people, &w, &opts).map_err(|e| e.to_string())?;
        if json {
            println!("{}", serde_json::to_string_pretty(&cmp).unwrap());
            return Ok(());
        }
        let pc = |p: Option<f64>| p.map(|x| format!("{x:+.0}%")).unwrap_or_else(|| "-".into());
        let width = people
            .iter()
            .map(|p| p.label.chars().count())
            .max()
            .unwrap_or(4)
            .clamp(6, 24);
        if let Some(fam) = &cmp.family {
            println!("model family: {fam}\n");
        }
        let w = width + 1;
        for (i, wk) in cmp.weeks.iter().enumerate() {
            println!("week of {}", wk.start);
            println!(
                "  {:<w$} {:>8} {:>6} {:>9} {:>6} {:>8} {:>6} {:>8} {:>6}",
                "", "requests", "", "total", "", "ctx/req", "", "out/req", ""
            );
            for p in &cmp.people {
                let c = &p.cells[i];
                let mut label: String = p.label.chars().take(24).collect();
                if c.days_covered == 0 {
                    println!("  {label:<w$} (not covered)");
                    continue;
                }
                if c.partial {
                    label.push('*');
                }
                let t = &c.totals;
                println!(
                    "  {:<w$} {:>8} {:>6} {:>9} {:>6} {:>8} {:>6} {:>8.0} {:>6}",
                    label,
                    t.requests,
                    pc(c.change.requests),
                    fmt::tokens(t.total),
                    pc(c.change.total),
                    fmt::tokens(t.context_per_request as u64),
                    pc(c.change.context_per_request),
                    t.output_per_request,
                    pc(c.change.output_per_request),
                );
            }
            println!();
        }
        if cmp.people.iter().any(|p| p.cells.iter().any(|c| c.partial)) {
            println!(
                "* week only partly covered; change is against the same weekdays a week earlier"
            );
        }
        Ok(())
    }
}
