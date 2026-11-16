use usage_core::cost::PriceTable;
use usage_core::parse::Row;
use usage_core::{query, Filter, Index};

fn row(key: &str, model: &str, output: u64, read: u64) -> Row {
    Row {
        key: key.into(),
        session: "s".into(),
        day: "2026-11-16".into(),
        model: model.into(),
        project: "/p".into(),
        cwd: "/p".into(),
        output,
        cache_read: read,
        ..Default::default()
    }
}

#[test]
fn rows_get_a_cost_once_and_unknown_models_stay_unpriced() {
    let mut idx = Index::open_in_memory().unwrap();
    idx.insert_rows(
        "f",
        &[
            row("a", "claude-opus-5-5", 1_000, 100_000),
            row("b", "some-unknown-model", 1_000, 100_000),
        ],
    )
    .unwrap();
    let table = PriceTable::embedded();
    assert_eq!(idx.price_rows(&table).unwrap(), 2);
    // Same table: nothing to do.
    assert_eq!(idx.price_rows(&table).unwrap(), 0);

    let t = query::totals(&idx.conn, &Filter::default()).unwrap();
    assert_eq!(t.unpriced, 1);
    assert!(t.cost_usd > 0.0);
    assert!(t.weighted > 0.0);
    // One priced request: cost per request is its cost.
    assert!((t.cost_per_request - t.cost_usd).abs() < 1e-12);

    // The cost equals what the price table says for that request.
    let mut u = usage_core::cost::Usage {
        output: 1_000,
        cache_read: 100_000,
        ..Default::default()
    };
    u.set_cache_write(0, 0, 0);
    let expect = table
        .cost_of_on("claude-opus-5-5", &u, Some("2026-11-16"))
        .unwrap();
    assert!((t.cost_usd - expect).abs() < 1e-9);
}
