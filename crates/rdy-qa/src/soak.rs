//! The deep gate's memory soak check. The soak lands one change at a time on one daemon and takes
//! a [`Sample`] after each landing. The check fails when:
//!
//! - (a) the daemon's anonymous resident memory (`RssAnon`) goes over [`RSS_ANON_LIMIT`] at any
//!   sample;
//! - (b) the daemon's live memory, less the bytes its capped caches say they hold, grows after
//!   landing [`SETTLE_LANDINGS`]. A capped cache may fill up to its cap: that is not growth.
//!
//! "Grows" means the median net bytes of the last [`TAIL_SAMPLES`] samples is more than
//! [`GROWTH_SLACK`] over the net bytes at landing [`SETTLE_LANDINGS`]. The slack is a fixed value
//! by judgement: a leak of one scope's build per landing passes it within a few landings, and
//! the project's own index growth over a soak stays well under it.

/// The most anonymous resident memory the daemon may hold at any sample (800 MiB).
pub const RSS_ANON_LIMIT: u64 = 800 * 1024 * 1024;
/// The landings before the growth check starts: start-up caches fill in these.
pub const SETTLE_LANDINGS: u64 = 5;
/// How far the net bytes may rise over the settled value before it counts as growth (32 MiB).
pub const GROWTH_SLACK: u64 = 32 * 1024 * 1024;
/// The samples at the end whose median is compared with the settled value.
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

/// Checks a soak's samples (in landing order) against rules (a) and (b). The error names each
/// rule that failed, with the numbers.
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
    let after: Vec<&Sample> = samples
        .iter()
        .filter(|s| s.landing >= SETTLE_LANDINGS)
        .collect();
    match after.first() {
        None => failed.push(format!(
            "no sample at or after landing {SETTLE_LANDINGS}: the soak is too short"
        )),
        Some(base) if after.len() <= TAIL_SAMPLES => failed.push(format!(
            "only {} samples from landing {}: the growth check needs more than {TAIL_SAMPLES}",
            after.len(),
            base.landing
        )),
        Some(base) => {
            let mut tail: Vec<u64> = after[after.len() - TAIL_SAMPLES..]
                .iter()
                .map(|s| s.net())
                .collect();
            tail.sort_unstable();
            let end = tail[TAIL_SAMPLES / 2];
            if end > base.net().saturating_add(GROWTH_SLACK) {
                let last = after[after.len() - 1];
                failed.push(format!(
                    "live memory less capped caches grew from {} at landing {} to {} at landing {} \
                     (median of the last {TAIL_SAMPLES}), over the {} slack",
                    mib(base.net()),
                    base.landing,
                    mib(end),
                    last.landing,
                    mib(GROWTH_SLACK)
                ));
            }
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

    fn series(n: u64, f: impl Fn(u64) -> (u64, u64, u64)) -> Vec<Sample> {
        (1..=n)
            .map(|landing| {
                let (rss_anon, live, cache) = f(landing);
                Sample {
                    landing,
                    rss_anon,
                    live,
                    cache,
                }
            })
            .collect()
    }

    #[test]
    fn soak1_rss_anon_over_800_mb_fails() {
        // flat live memory, but one sample is over the limit
        let s = series(40, |l| {
            let rss = if l == 23 { 801 * MB } else { 500 * MB };
            (rss, 300 * MB, 0)
        });
        let e = check(&s).unwrap_err();
        assert!(e.contains("RssAnon") && e.contains("landing 23"), "{e}");
        assert!(!e.contains("grew"), "{e}");
    }

    #[test]
    fn soak1_growth_after_landing_5_with_no_cache_growth_fails() {
        // 1 MiB a landing of live memory, the cache flat
        let s = series(60, |l| (500 * MB, 300 * MB + l * MB, 8 * MB));
        let e = check(&s).unwrap_err();
        assert!(e.contains("grew") && e.contains("landing 5"), "{e}");
        assert!(!e.contains("RssAnon"), "{e}");
    }

    #[test]
    fn soak1_only_the_capped_cache_growing_to_its_cap_passes() {
        // the cache fills ~0.7 MiB a landing to its cap near landing 90, then stays; live memory
        // grows by exactly what the cache holds, with some noise
        let s = series(120, |l| {
            let cache = (l * 7 * MB / 10).min(CAP);
            let noise = (l % 3) * 4 * MB;
            (450 * MB + 3 * cache, 320 * MB + cache + noise, cache)
        });
        assert_eq!(check(&s), Ok(()));
        assert_eq!(s.last().unwrap().cache, CAP, "the cache reached its cap");
    }

    #[test]
    fn soak1_a_soak_too_short_for_the_growth_check_fails() {
        let s = series(8, |_| (400 * MB, 300 * MB, 0));
        assert!(check(&s).unwrap_err().contains("needs more than"));
        assert!(check(&s[..3]).unwrap_err().contains("too short"));
    }

    #[test]
    fn soak1_rss_anon_line_parses_in_kb() {
        let text = "Name:\trdy\nVmRSS:\t  600000 kB\nRssAnon:\t  547000 kB\nRssFile:\t 1 kB\n";
        assert_eq!(parse_rss_anon(text), Some(547_000 * 1024));
        assert_eq!(parse_rss_anon("VmRSS:\t1 kB\n"), None);
        assert_eq!(parse_rss_anon("RssAnon:\t12 MB\n"), None);
    }
}
