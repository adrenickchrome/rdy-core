//! The deep gate's memory soak check. The soak lands one change at a time on one daemon serving a
//! project of realistic size, with views kept open across landings, and takes a [`Sample`] before
//! the first landing (landing 0) and after each cycle. "Net" is the daemon's live memory less the
//! bytes its capped caches say they hold. The check fails when:
//!
//! - (a) the daemon's anonymous resident memory (`RssAnon`) goes over [`RSS_ANON_LIMIT`] at any
//!   sample;
//! - (b) from landing [`LEVEL_FROM`] on, net goes over [`LEVEL_TENTHS`]/10 times the net at
//!   landing 0 (canon built and read, nothing landed). The views kept open and the count-capped
//!   caches of old landings and proposal versions are full by then; each one that held its own
//!   copy of the project, as the engine before scope sharing did, adds about canon's size;
//! - (c) the median net of the last [`TAIL_SAMPLES`] samples is more than half the net at landing 0
//!   over the net at landing [`LEVEL_FROM`]: a leak that grows with each landing.
//!
//! The check also fails when the net at landing 0 is under [`MIN_ZERO_NET`]: the project is then
//! too small for (b) and (c) to tell a copy of it from noise.

/// The most anonymous resident memory the daemon may hold at any sample (800 MiB).
pub const RSS_ANON_LIMIT: u64 = 800 * 1024 * 1024;
/// The landing from which rules (b) and (c) apply: the kept views and capped caches are full.
pub const LEVEL_FROM: u64 = 10;
/// Rule (b)'s bound on net, in tenths of the net at landing 0 (4.5 times).
pub const LEVEL_TENTHS: u64 = 45;
/// The least net at landing 0 for which the rules can tell a copy of the project from noise
/// (16 MiB).
pub const MIN_ZERO_NET: u64 = 16 * 1024 * 1024;
/// The samples at the end whose median rule (c) compares.
pub const TAIL_SAMPLES: usize = 5;

/// One reading of the daemon, taken after landing `landing`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    pub landing: u64,
    /// `RssAnon` of the daemon, in bytes.
    pub rss_anon: u64,
    /// The bytes the daemon's allocator has handed out and not had back.
    pub live: u64,
    /// The bytes the daemon's capped caches say they hold.
    pub cache: u64,
}

impl Sample {
    /// Live memory less what the capped caches hold.
    pub fn net(&self) -> u64 {
        self.live.saturating_sub(self.cache)
    }
}

fn mib(b: u64) -> String {
    format!("{:.1} MiB", b as f64 / (1024.0 * 1024.0))
}

/// Checks a soak's samples (in landing order, the first at landing 0) against rules (a) to (c).
/// The error names each rule that failed, with the numbers.
pub fn check(samples: &[Sample]) -> Result<(), String> {
    let mut failed = vec![];
    if let Some(s) = samples.iter().find(|s| s.rss_anon > RSS_ANON_LIMIT) {
        failed.push(format!(
            "RssAnon {} at landing {} is over the {} limit",
            mib(s.rss_anon),
            s.landing,
            mib(RSS_ANON_LIMIT)
        ));
    }
    let zero = match samples.first() {
        Some(z) if z.landing == 0 => z.net(),
        _ => {
            failed.push("no sample at landing 0".to_string());
            return Err(failed.join("; "));
        }
    };
    let after: Vec<&Sample> = samples.iter().filter(|s| s.landing >= LEVEL_FROM).collect();
    if zero < MIN_ZERO_NET {
        failed.push(format!(
            "net {} at landing 0 is under {}: the project is too small for the growth rules",
            mib(zero),
            mib(MIN_ZERO_NET)
        ));
    } else if after.len() <= TAIL_SAMPLES {
        failed.push(format!(
            "only {} samples from landing {LEVEL_FROM}: the rules need more than {TAIL_SAMPLES}",
            after.len()
        ));
    } else {
        let level = zero / 10 * LEVEL_TENTHS;
        if let Some(s) = after.iter().find(|s| s.net() > level) {
            failed.push(format!(
                "net {} at landing {} is over {} ({}.{} times the {} at landing 0)",
                mib(s.net()),
                s.landing,
                mib(level),
                LEVEL_TENTHS / 10,
                LEVEL_TENTHS % 10,
                mib(zero)
            ));
        }
        let base = after[0];
        let mut tail: Vec<u64> = after[after.len() - TAIL_SAMPLES..]
            .iter()
            .map(|s| s.net())
            .collect();
        tail.sort_unstable();
        let end = tail[TAIL_SAMPLES / 2];
        if end > base.net().saturating_add(zero / 2) {
            failed.push(format!(
                "net grew from {} at landing {} to {} at landing {} (median of the last \
                 {TAIL_SAMPLES}), more than half the {} at landing 0",
                mib(base.net()),
                base.landing,
                mib(end),
                after[after.len() - 1].landing,
                mib(zero)
            ));
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(failed.join("; "))
    }
}

/// The bytes of the `RssAnon:` line of a `/proc/<pid>/status` text (the kernel writes it in kB).
pub fn parse_rss_anon(status: &str) -> Option<u64> {
    let line = status.lines().find(|l| l.starts_with("RssAnon:"))?;
    let mut words = line["RssAnon:".len()..].split_whitespace();
    let n: u64 = words.next()?.parse().ok()?;
    match words.next() {
        Some("kB") => n.checked_mul(1024),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB: u64 = 1024 * 1024;
    /// The capped store map cache's budget (MAP_CACHE_BYTES).
    const CAP: u64 = 64 * MB;

    /// Landing 0 with net `zero`, then landings 1..=n from `f`.
    fn series(zero: u64, n: u64, f: impl Fn(u64) -> (u64, u64, u64)) -> Vec<Sample> {
        let mut v = vec![Sample {
            landing: 0,
            rss_anon: zero + 20 * MB,
            live: zero,
            cache: 0,
        }];
        v.extend((1..=n).map(|landing| {
            let (rss_anon, live, cache) = f(landing);
            Sample {
                landing,
                rss_anon,
                live,
                cache,
            }
        }));
        v
    }

    /// The fixed engine on the bench (net in MiB): 132 at landing 0, 477 at 10, then about
    /// 1.2 a landing; the capped cache fills about 1.6 a landing.
    fn fixed(l: u64) -> (u64, u64, u64) {
        let cache = (l * 16 * MB / 10).min(CAP);
        let net = if l < 10 {
            132 * MB + l * 34 * MB
        } else {
            477 * MB + (l - 10) * 12 * MB / 10
        };
        (net + cache + 100 * MB, net + cache, cache)
    }

    #[test]
    fn soak1_the_fixed_engine_passes() {
        assert_eq!(check(&series(132 * MB, 40, fixed)), Ok(()));
    }

    #[test]
    fn soak1_the_old_engine_fails_on_its_level() {
        // the bench before scope sharing: 155 at landing 0, 871 at 10, 915 at 40
        let s = series(155 * MB, 40, |l| {
            let net = if l < 10 {
                155 * MB + l * 72 * MB
            } else {
                871 * MB + (l - 10) * 15 * MB / 10
            };
            (700 * MB, net, 0)
        });
        let e = check(&s).unwrap_err();
        assert!(e.contains("4.5 times"), "{e}");
        assert!(!e.contains("grew"), "{e}");
    }

    #[test]
    fn soak1_rss_anon_over_800_mb_fails() {
        let s = series(132 * MB, 40, |l| {
            let (rss, live, cache) = fixed(l);
            (if l == 23 { 801 * MB } else { rss }, live, cache)
        });
        let e = check(&s).unwrap_err();
        assert!(e.contains("RssAnon") && e.contains("landing 23"), "{e}");
    }

    #[test]
    fn soak1_growth_after_landing_10_with_no_cache_growth_fails() {
        // under the level bound, but 3 MiB more each landing: 90 MiB from landing 10 to 40
        let s = series(132 * MB, 40, |l| (300 * MB, 300 * MB + 3 * l * MB, 8 * MB));
        let e = check(&s).unwrap_err();
        assert!(e.contains("grew") && e.contains("landing 10"), "{e}");
        assert!(!e.contains("times"), "{e}");
    }

    #[test]
    fn soak1_only_the_capped_cache_growing_to_its_cap_passes() {
        let s = series(300 * MB, 120, |l| {
            let cache = (l * 7 * MB / 10).min(CAP);
            let noise = (l % 3) * 4 * MB;
            (450 * MB + 3 * cache, 320 * MB + cache + noise, cache)
        });
        assert_eq!(check(&s), Ok(()));
        assert_eq!(s.last().unwrap().cache, CAP, "the cache reached its cap");
    }

    #[test]
    fn soak1_a_project_too_small_to_show_a_copy_fails() {
        // the first bench run: about 1.3 MiB of net
        let s = series(MB, 40, |l| (5 * MB, MB + l * 45 * 1024, 0));
        assert!(check(&s).unwrap_err().contains("too small"));
    }

    #[test]
    fn soak1_a_soak_too_short_or_with_no_landing_0_fails() {
        let s = series(132 * MB, 12, fixed);
        assert!(check(&s).unwrap_err().contains("need more than"));
        assert!(check(&s[1..]).unwrap_err().contains("landing 0"));
    }

    #[test]
    fn soak1_rss_anon_line_parses_in_kb() {
        let text = "Name:\trdy\nVmRSS:\t  600000 kB\nRssAnon:\t  547000 kB\nRssFile:\t 1 kB\n";
        assert_eq!(parse_rss_anon(text), Some(547_000 * 1024));
        assert_eq!(parse_rss_anon("VmRSS:\t1 kB\n"), None);
        assert_eq!(parse_rss_anon("RssAnon:\t12 MB\n"), None);
    }
}
