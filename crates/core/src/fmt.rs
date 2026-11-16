/// 1234 → "1.2k", 1_234_567 → "1.2M", 1_234_567_890 → "1.23B".
pub fn tokens(n: u64) -> String {
    let v = n as f64;
    if v >= 1e9 {
        format!("{:.2}B", v / 1e9)
    } else if v >= 1e6 {
        format!("{:.1}M", v / 1e6)
    } else if v >= 1e3 {
        format!("{:.1}k", v / 1e3)
    } else {
        n.to_string()
    }
}

/// Signed percent change from `before` to `after`, or None when `before` is 0.
pub fn change(before: f64, after: f64) -> Option<f64> {
    (before > 0.0).then(|| (after - before) / before * 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(tokens(999), "999");
        assert_eq!(tokens(1_250), "1.2k");
        assert_eq!(tokens(3_700_000), "3.7M");
        assert_eq!(tokens(1_207_900_000), "1.21B");
        assert_eq!(change(0.0, 5.0), None);
        assert_eq!(change(100.0, 50.0), Some(-50.0));
    }
}
