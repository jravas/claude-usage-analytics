//! "How does today compare with a usual day?" for the menu bar.
//!
//! On a subscription plan dollars mean nothing and plan-limit usage is not in
//! local data, so the menu bar compares the user with their own normal
//! instead. Amounts are price-weighted tokens (input = 1), so cheap cache
//! reads do not drown out the work that actually costs.
//!
//! A usual day is the median of past days of the same kind (weekday or
//! weekend) in the last 4 weeks that had any use, each cut at the current
//! time of day. Same kind rather than same weekday: four samples per weekday
//! are too few, and one holiday would move the median. Days with no use are
//! left out, so a week off does not make every day look heavy. A usual week
//! is the median of the previous 4 weeks with any use, cut at the same
//! weekday and time.
//!
//! When there are too few such days, or when a usual day has barely started
//! by this time (a ratio against a tiny baseline is noise), the result says
//! so instead of giving a multiple.

use jiff::civil::{Date, Weekday};
use jiff::{Span, Zoned};
use rusqlite::{params, Connection};

/// Past days of the same kind, with any use, needed for a usual day. The
/// last 4 weeks have 20 weekdays and 8 weekend days.
pub const MIN_WEEKDAYS: usize = 5;
pub const MIN_WEEKEND_DAYS: usize = 3;
/// Past weeks with any use needed for a usual week (of 4).
pub const MIN_WEEKS: usize = 3;
/// Below this share of a usual full day, the usual amount up to now is too
/// small to compare against. For a week it is the same share of a seventh of
/// a usual week, so Monday afternoon already compares.
pub const EARLY_SHARE: f64 = 0.1;

/// One past day or week: its use up to the current time of day (or weekday
/// and time) and its whole use.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub so_far: f64,
    pub full: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Usual {
    /// Fewer than `need` past periods with any use.
    TooLittleHistory { have: usize, need: usize },
    /// Medians over `periods` past periods with any use.
    Typical {
        so_far: f64,
        full: f64,
        periods: usize,
    },
}

/// The usual period from past samples. Periods with no use are ignored.
pub fn typical(samples: &[Sample], need: usize) -> Usual {
    let active: Vec<Sample> = samples.iter().copied().filter(|s| s.full > 0.0).collect();
    if active.len() < need.max(1) {
        return Usual::TooLittleHistory {
            have: active.len(),
            need,
        };
    }
    let so_far = median(active.iter().map(|s| s.so_far).collect());
    let full = median(active.iter().map(|s| s.full).collect());
    Usual::Typical {
        so_far,
        full,
        periods: active.len(),
    }
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    let n = v.len();
    if n == 0 {
        0.0
    } else if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// The current period against the usual one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Versus {
    TooLittleHistory {
        have: usize,
        need: usize,
    },
    /// By this time a usual period has used less than `EARLY_SHARE` of a
    /// usual day, so a multiple would be noise.
    Early {
        usual_full: f64,
    },
    /// `now` is `ratio` × the usual amount up to the same time.
    Ratio {
        ratio: f64,
        usual_so_far: f64,
        usual_full: f64,
    },
}

/// `days` is how many days the period has: 1 for a day, 7 for a week.
pub fn versus(now: f64, u: Usual, days: f64) -> Versus {
    match u {
        Usual::TooLittleHistory { have, need } => Versus::TooLittleHistory { have, need },
        Usual::Typical { so_far, full, .. } => {
            if so_far <= 0.0 || so_far < EARLY_SHARE * full / days {
                Versus::Early { usual_full: full }
            } else {
                Versus::Ratio {
                    ratio: now / so_far,
                    usual_so_far: so_far,
                    usual_full: full,
                }
            }
        }
    }
}

/// "1.4×", "0.6×", "12×".
pub fn multiple(r: f64) -> String {
    if r >= 9.95 {
        format!("{r:.0}×")
    } else {
        format!("{r:.1}×")
    }
}

pub fn is_weekend(d: Date) -> bool {
    matches!(d.weekday(), Weekday::Saturday | Weekday::Sunday)
}

/// Today against a usual day of the same kind, and against a usual week.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Report {
    pub today: f64,
    pub day: Versus,
    pub week: f64,
    pub week_vs: Versus,
    pub weekend: bool,
}

fn err(e: jiff::Error) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(e))
}

/// Weighted tokens on days `[from, to]` (local dates), all of it and the part
/// before `cut_ms`.
fn sums(conn: &Connection, from: Date, to: Date, cut_ms: i64) -> rusqlite::Result<Sample> {
    conn.query_row(
        "SELECT COALESCE(SUM(CASE WHEN ts < ?3 THEN weighted END), 0),
                COALESCE(SUM(weighted), 0)
         FROM requests WHERE day >= ?1 AND day <= ?2",
        params![from.to_string(), to.to_string(), cut_ms],
        |r| {
            Ok(Sample {
                so_far: r.get(0)?,
                full: r.get(1)?,
            })
        },
    )
}

/// The same local time of day on `d`. On a DST gap the later time is used.
fn at_time(d: Date, now: &Zoned) -> rusqlite::Result<i64> {
    let z = d
        .to_datetime(now.time())
        .to_zoned(now.time_zone().clone())
        .map_err(err)?;
    Ok(z.timestamp().as_millisecond())
}

pub fn report(conn: &Connection, now: &Zoned) -> rusqlite::Result<Report> {
    let today = now.date();
    let now_ms = now.timestamp().as_millisecond();
    let back = |d: Date, n: i64| d.checked_sub(Span::new().days(n)).map_err(err);

    let weekend = is_weekend(today);
    let mut days = vec![];
    for n in 1..=28 {
        let d = back(today, n)?;
        if is_weekend(d) == weekend {
            days.push(sums(conn, d, d, at_time(d, now)?)?);
        }
    }
    let need = if weekend {
        MIN_WEEKEND_DAYS
    } else {
        MIN_WEEKDAYS
    };
    let today_sum = sums(conn, today, today, now_ms + 1)?.so_far;

    let monday = back(today, today.weekday().to_monday_zero_offset() as i64)?;
    let mut weeks = vec![];
    for k in 1..=4 {
        let m = back(monday, 7 * k)?;
        let sunday = back(m, -6)?;
        let same_day = back(today, 7 * k)?;
        weeks.push(sums(conn, m, sunday, at_time(same_day, now)?)?);
    }
    let week_sum = sums(conn, monday, today, now_ms + 1)?.so_far;

    Ok(Report {
        today: today_sum,
        day: versus(today_sum, typical(&days, need), 1.0),
        week: week_sum,
        week_vs: versus(week_sum, typical(&weeks, MIN_WEEKS), 7.0),
        weekend,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(so_far: f64, full: f64) -> Sample {
        Sample { so_far, full }
    }

    #[test]
    fn median_of_active_days() {
        // Two empty days are ignored; five active ones remain.
        let v = [
            s(10.0, 100.0),
            s(0.0, 0.0),
            s(30.0, 300.0),
            s(20.0, 200.0),
            s(0.0, 0.0),
            s(50.0, 500.0),
            s(40.0, 400.0),
        ];
        assert_eq!(
            typical(&v, 5),
            Usual::Typical {
                so_far: 30.0,
                full: 300.0,
                periods: 5
            }
        );
        // Even count: mean of the middle two.
        let u = typical(&v[..4], 3);
        assert_eq!(
            u,
            Usual::Typical {
                so_far: 20.0,
                full: 200.0,
                periods: 3
            }
        );
        match versus(30.0, u, 1.0) {
            Versus::Ratio { ratio, .. } => assert!((ratio - 1.5).abs() < 1e-9),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn too_little_history() {
        let v = [s(10.0, 100.0), s(0.0, 0.0), s(5.0, 50.0)];
        let u = typical(&v, 5);
        assert_eq!(u, Usual::TooLittleHistory { have: 2, need: 5 });
        assert_eq!(
            versus(1_000.0, u, 1.0),
            Versus::TooLittleHistory { have: 2, need: 5 }
        );
        assert_eq!(
            typical(&[], 3),
            Usual::TooLittleHistory { have: 0, need: 3 }
        );
    }

    #[test]
    fn early_in_the_day_gives_no_multiple() {
        // By now a usual day has used 2 of 300: any multiple would be noise.
        let v = [s(2.0, 300.0), s(0.0, 280.0), s(4.0, 320.0)];
        assert_eq!(
            versus(50.0, typical(&v, 3), 1.0),
            Versus::Early { usual_full: 300.0 }
        );
        // 5 of 300 by now: too early for a day, but a week compares from a
        // tenth of a usual day (300 / 7 / 10).
        let v = [s(5.0, 300.0), s(5.0, 280.0), s(5.0, 320.0)];
        assert!(matches!(
            versus(10.0, typical(&v, 3), 7.0),
            Versus::Ratio { .. }
        ));
        assert!(matches!(
            versus(10.0, typical(&v, 3), 1.0),
            Versus::Early { .. }
        ));
        let v = [s(0.0, 300.0), s(0.0, 280.0), s(0.0, 320.0)];
        assert!(matches!(
            versus(0.0, typical(&v, 3), 1.0),
            Versus::Early { .. }
        ));
    }

    #[test]
    fn multiples() {
        assert_eq!(multiple(1.44), "1.4×");
        assert_eq!(multiple(0.0), "0.0×");
        assert_eq!(multiple(12.3), "12×");
    }

    #[test]
    fn report_from_rows() {
        use crate::live::tests::db;
        let c = db();
        c.execute_batch("ALTER TABLE requests ADD COLUMN day TEXT; ALTER TABLE requests ADD COLUMN weighted REAL;")
            .unwrap();
        let tz = jiff::tz::TimeZone::UTC;
        // Wednesday 2026-11-18 at 12:00 UTC.
        let now = jiff::civil::date(2026, 11, 18)
            .at(12, 0, 0, 0)
            .to_zoned(tz.clone())
            .unwrap();
        let add = |d: Date, h: i8, w: f64| {
            let ts = d
                .at(h, 0, 0, 0)
                .to_zoned(tz.clone())
                .unwrap()
                .timestamp()
                .as_millisecond();
            c.execute(
                "INSERT INTO requests(key, session, ts, project, subagent, input, output, cache_write, cw1h, cache_read, day, weighted)
                 VALUES (?1, 'a', ?2, '/p', 0, 0, 0, 0, 0, 0, ?3, ?4)",
                params![format!("{d}{h}"), ts, d.to_string(), w],
            )
            .unwrap();
        };
        // Each of the previous 10 weekdays: 100 in the morning, 100 at night.
        let mut d = now.date();
        let mut n = 0;
        while n < 10 {
            d = d.yesterday().unwrap();
            if !is_weekend(d) {
                add(d, 9, 100.0);
                add(d, 20, 100.0);
                n += 1;
            }
        }
        // A weekend day is not part of a usual weekday.
        add(jiff::civil::date(2026, 11, 15), 9, 10_000.0);
        add(now.date(), 10, 150.0);
        let r = report(&c, &now).unwrap();
        assert_eq!(r.today, 150.0);
        assert!(!r.weekend);
        assert_eq!(
            r.day,
            Versus::Ratio {
                ratio: 1.5,
                usual_so_far: 100.0,
                usual_full: 200.0
            }
        );
        // Only two past weeks have use: not enough for a usual week.
        assert_eq!(r.week_vs, Versus::TooLittleHistory { have: 2, need: 3 });
    }
}
