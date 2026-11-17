//! Host memory admission for new ART sessions (owner directive, 2026-11-15).
//!
//! ACOS runs real ART guests. An outside agent that calls `acos` or `acos-mcp`
//! directly can start several sessions at once, and each one costs a real guest
//! plus its cgroup. There is **no** session queue, no slot file and no lock: a
//! launch either has host room right now or it fails fast with a structured,
//! retryable `HOST_MEMORY_LOW`.
//!
//! The rule is one comparison:
//!
//! ```text
//! MemAvailable - session budget  <  10% of MemTotal   ->  HOST_MEMORY_LOW
//! ```
//!
//! `MemAvailable` and `MemTotal` come from `/proc/meminfo`; the session budget
//! is the per-session cgroup cap the guest will get (`ACOS_ART_MEM_MAX`,
//! default 2 GiB — the same value `acos-art` applies to the run's cgroup). The
//! check never waits and never retries; the agent decides whether to free
//! memory and call again.
//!
//! `ACOS_MEMORY_GUARD=off` (also `0`, `false`, `disabled`) disables the check;
//! it exists so a lab host with a known-static configuration can opt out. It
//! does not disable the per-session cgroup cap, which `acos-art` always applies.

use std::path::Path;

/// Env var that turns the admission check off (`off`, `0`, `false`, `disabled`).
pub const GUARD_ENV: &str = "ACOS_MEMORY_GUARD";

/// Per-session guest memory cap, mirroring `acos_art::cgroup`.
pub const MEM_MAX_ENV: &str = "ACOS_ART_MEM_MAX";

/// Default per-session guest memory cap: 2 GiB (`acos-art`'s `DEFAULT_MEM_MAX`).
pub const DEFAULT_MEM_MAX: u64 = 2 * 1024 * 1024 * 1024;

/// Host filesystem path of the kernel memory summary.
pub const MEMINFO_PATH: &str = "/proc/meminfo";

/// The fraction of `MemTotal` that must remain available after admission, in
/// percent. The owner's ceiling is "below 10% of MemTotal is too little".
pub const MIN_FREE_PERCENT: u64 = 10;

/// One `/proc/meminfo` reading, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemorySnapshot {
    pub available_bytes: u64,
    pub total_bytes: u64,
}

/// Why a launch was refused. Every field is a host fact the agent can act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionDenied {
    pub available_bytes: u64,
    pub total_bytes: u64,
    pub requested_bytes: u64,
    /// `MIN_FREE_PERCENT` of `total_bytes`: what must stay free after the new
    /// session's budget is accounted for.
    pub min_free_bytes: u64,
}

impl AdmissionDenied {
    /// Short human line for the CLI/MCP envelope detail.
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "host memory would drop below {}% of total: MemAvailable={} MiB, \
             MemTotal={} MiB, this session's budget={} MiB, required free after \
             admission={} MiB. Free memory (stop a session) or run on a host with \
             more headroom, then retry. The check is per launch; nothing is queued. \
             Set {GUARD_ENV}=off only on a lab host whose memory is known-static.",
            MIN_FREE_PERCENT,
            mib(self.available_bytes),
            mib(self.total_bytes),
            mib(self.requested_bytes),
            mib(self.min_free_bytes),
        )
    }

    /// The `details` object carried in the protocol error envelope.
    #[must_use]
    pub fn details(&self) -> serde_json::Value {
        serde_json::json!({
            "mem_available_mib": mib(self.available_bytes),
            "mem_total_mib": mib(self.total_bytes),
            "requested_mib": mib(self.requested_bytes),
            "min_free_mib": mib(self.min_free_bytes),
            "min_free_percent": MIN_FREE_PERCENT,
            "guard": GUARD_ENV,
            "budget_env": MEM_MAX_ENV,
        })
    }
}

fn mib(bytes: u64) -> u64 {
    bytes / (1024 * 1024)
}

/// Parse `MemAvailable:` and `MemTotal:` out of `/proc/meminfo` text.
///
/// `None` when either line is missing or unparsable — the caller treats an
/// unreadable summary as "cannot be checked", never as "there is room".
#[must_use]
pub fn parse_meminfo(text: &str) -> Option<MemorySnapshot> {
    let mut available = None;
    let mut total = None;
    for line in text.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let value = rest.split_whitespace().next()?.parse::<u64>().ok()?;
        match key.trim() {
            // Both fields are kB in procfs; convert once here.
            "MemAvailable" => available = Some(value.saturating_mul(1024)),
            "MemTotal" => total = Some(value.saturating_mul(1024)),
            _ => {}
        }
    }
    Some(MemorySnapshot {
        available_bytes: available?,
        total_bytes: total?,
    })
}

/// `ACOS_MEMORY_GUARD` semantics: the check runs unless it names an off value.
#[must_use]
pub fn guard_enabled(raw: Option<&str>) -> bool {
    match raw.map(|v| v.trim().to_ascii_lowercase()) {
        Some(v) if matches!(v.as_str(), "off" | "0" | "false" | "disabled" | "no") => false,
        _ => true,
    }
}

/// The admission rule itself: pure, so it can be tested against fabricated
/// meminfo and budgets instead of the live host.
#[must_use]
pub fn check_snapshot(
    snapshot: MemorySnapshot,
    requested_bytes: u64,
    min_free_bytes: u64,
) -> Result<(), AdmissionDenied> {
    let after = snapshot.available_bytes.saturating_sub(requested_bytes);
    if after < min_free_bytes {
        return Err(AdmissionDenied {
            available_bytes: snapshot.available_bytes,
            total_bytes: snapshot.total_bytes,
            requested_bytes,
            min_free_bytes,
        });
    }
    Ok(())
}

/// The free-memory floor for a host of `total_bytes`.
#[must_use]
pub fn min_free_bytes(total_bytes: u64) -> u64 {
    total_bytes / 100 * MIN_FREE_PERCENT
}

/// Parse `2097152`, `2048K`, `512M`, `2G` (binary multiples), the spelling
/// `acos-art` accepts for `ACOS_ART_MEM_MAX`.
#[must_use]
pub fn parse_size(raw: &str) -> Option<u64> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    let (num, mul) = match s.chars().last()?.to_ascii_uppercase() {
        'K' => (&s[..s.len() - 1], 1024u64),
        'M' => (&s[..s.len() - 1], 1024 * 1024),
        'G' => (&s[..s.len() - 1], 1024 * 1024 * 1024),
        _ => (s, 1),
    };
    let n: u64 = num.trim().parse().ok()?;
    n.checked_mul(mul)
}

/// The memory budget a new session will claim: `ACOS_ART_MEM_MAX`, else
/// `DEFAULT_MEM_MAX`. A malformed or zero value falls back to the default
/// (the launcher refuses it separately, before the guest starts).
#[must_use]
pub fn session_budget_bytes(raw: Option<&str>) -> u64 {
    raw.and_then(parse_size)
        .filter(|bytes| *bytes > 0)
        .unwrap_or(DEFAULT_MEM_MAX)
}

/// Check the live host before an ART guest is spawned.
///
/// Returns `Err` when the launch must be refused. An unreadable
/// `/proc/meminfo` is not treated as "no room": on a host where the summary
/// cannot be read the launcher fails later with its own host error, and a
/// fabricated refusal would be worse than an honest one. That case is logged
/// to stderr and allowed.
pub fn check_host() -> Result<(), AdmissionDenied> {
    check_host_with(Path::new(MEMINFO_PATH), |key| std::env::var(key).ok())
}

/// `check_host` with the meminfo path and the environment injected (tests).
pub fn check_host_with(
    meminfo: &Path,
    env: impl Fn(&str) -> Option<String>,
) -> Result<(), AdmissionDenied> {
    if !guard_enabled(env(GUARD_ENV).as_deref()) {
        return Ok(());
    }
    let snapshot = match std::fs::read_to_string(meminfo) {
        Ok(text) => match parse_meminfo(&text) {
            Some(snapshot) => snapshot,
            None => {
                eprintln!(
                    "acos: {GUARD_ENV}: {} has no readable MemAvailable/MemTotal; \
                     memory admission is skipped for this launch",
                    meminfo.display()
                );
                return Ok(());
            }
        },
        Err(error) => {
            eprintln!(
                "acos: {GUARD_ENV}: cannot read {} ({error}); memory admission is \
                 skipped for this launch",
                meminfo.display()
            );
            return Ok(());
        }
    };
    let budget = session_budget_bytes(env(MEM_MAX_ENV).as_deref());
    check_snapshot(snapshot, budget, min_free_bytes(snapshot.total_bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gib(n: u64) -> u64 {
        n * 1024 * 1024 * 1024
    }

    /// 60 GiB host with 34 GiB available: a 2 GiB guest leaves far more than
    /// the 6 GiB floor, so the launch is admitted.
    #[test]
    fn roomy_host_is_admitted() {
        let text = "MemTotal:       62914560 kB\nMemFree:  1024 kB\nMemAvailable:   35651584 kB\n";
        let snapshot = parse_meminfo(text).expect("parsed");
        assert_eq!(snapshot.total_bytes, gib(60));
        assert_eq!(snapshot.available_bytes, gib(34));
        assert_eq!(
            check_snapshot(snapshot, gib(2), min_free_bytes(gib(60))),
            Ok(())
        );
    }

    /// 8 GiB host with 7.5 GiB available: a 2 GiB guest would leave 5.5 GiB,
    /// below the 0.8 GiB floor? No — 5.5 GiB > 0.8 GiB, so this is admitted;
    /// the floor is 10% of total, not 10% of a fixed number.
    #[test]
    fn floor_is_ten_percent_of_total() {
        let snapshot = MemorySnapshot {
            available_bytes: gib(8) - gib(1) / 2,
            total_bytes: gib(8),
        };
        // 10% of total, computed in bytes (integer maths, so compare against
        // the same expression the guard uses rather than 8 GiB / 10).
        assert_eq!(
            min_free_bytes(snapshot.total_bytes),
            snapshot.total_bytes / 100 * 10
        );
        assert!(check_snapshot(snapshot, gib(2), min_free_bytes(snapshot.total_bytes)).is_ok());
    }

    /// The refusal case: 8 GiB total (floor 0.8 GiB), 2.5 GiB available, 2 GiB
    /// budget -> 0.5 GiB left, below the floor. The details carry every number
    /// the agent needs.
    #[test]
    fn low_memory_is_refused_with_details() {
        let text = "MemTotal:        8388608 kB\nMemAvailable:    2621440 kB\n";
        let snapshot = parse_meminfo(text).expect("parsed");
        assert_eq!(snapshot.total_bytes, gib(8));
        assert_eq!(snapshot.available_bytes, gib(2) + gib(1) / 2);
        let denied = check_snapshot(snapshot, gib(2), min_free_bytes(snapshot.total_bytes))
            .expect_err("must refuse");
        assert_eq!(denied.available_bytes, gib(2) + gib(1) / 2);
        assert_eq!(denied.total_bytes, gib(8));
        assert_eq!(denied.requested_bytes, gib(2));
        assert_eq!(denied.min_free_bytes, snapshot.total_bytes / 100 * 10);
        let details = denied.details();
        assert_eq!(details["mem_available_mib"], 2560);
        assert_eq!(details["mem_total_mib"], 8192);
        assert_eq!(details["requested_mib"], 2048);
        assert_eq!(details["min_free_mib"], 819);
        assert!(denied.describe().contains("MemAvailable=2560 MiB"));
    }

    /// Exactly-at-the-floor is admitted; one byte below is refused. The
    /// comparison is `<`, not `<=`.
    #[test]
    fn boundary_is_inclusive_of_the_floor() {
        let snapshot = MemorySnapshot {
            available_bytes: gib(4),
            total_bytes: gib(10),
        };
        let floor = min_free_bytes(snapshot.total_bytes);
        assert_eq!(floor, snapshot.total_bytes / 100 * 10);
        // Spending exactly down to the floor is admitted; one more byte is not.
        let spend_to_the_floor = snapshot.available_bytes - floor;
        assert!(check_snapshot(snapshot, spend_to_the_floor, floor).is_ok());
        assert!(check_snapshot(snapshot, spend_to_the_floor + 1, floor).is_err());
    }

    /// `ACOS_MEMORY_GUARD=off` (and its spellings) disables the check; an
    /// empty or unrelated value leaves it on.
    #[test]
    fn guard_off_spellings() {
        for off in ["off", "OFF", "0", "false", "disabled", "no", " off "] {
            assert!(!guard_enabled(Some(off)), "{off:?} must disable the guard");
        }
        for on in [None, Some(""), Some("on"), Some("1"), Some("yes")] {
            assert!(guard_enabled(on), "{on:?} must keep the guard");
        }
    }

    /// The budget comes from the same env `acos-art` reads for its cgroup cap.
    #[test]
    fn session_budget_from_env() {
        assert_eq!(session_budget_bytes(None), gib(2));
        assert_eq!(session_budget_bytes(Some("3G")), gib(3));
        assert_eq!(session_budget_bytes(Some("1024M")), gib(1));
        assert_eq!(session_budget_bytes(Some("2147483648")), gib(2));
        // A bogus value must not silently become "unlimited" or "zero".
        assert_eq!(session_budget_bytes(Some("nonsense")), gib(2));
        assert_eq!(session_budget_bytes(Some("0")), gib(2));
    }

    /// A missing `MemAvailable`/`MemTotal` is "cannot check", not "no room".
    #[test]
    fn unparsable_meminfo_is_none() {
        assert!(parse_meminfo("MemTotal: 1024 kB\n").is_none());
        assert!(parse_meminfo("MemAvailable: 1024 kB\n").is_none());
        assert!(parse_meminfo("").is_none());
        let parsed = parse_meminfo("MemTotal: 1024 kB\nMemAvailable: 512 kB\n").expect("parsed");
        assert_eq!(parsed.total_bytes, 1024 * 1024);
        assert_eq!(parsed.available_bytes, 512 * 1024);
    }

    /// End to end against a fabricated `/proc/meminfo`: the env-injected entry
    /// point refuses, and `ACOS_MEMORY_GUARD=off` admits the same host.
    #[test]
    fn check_host_with_fake_meminfo() {
        let dir = std::env::temp_dir().join(format!("acos-host-mem-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let path = dir.join("meminfo");
        std::fs::write(&path, "MemTotal: 4194304 kB\nMemAvailable: 1572864 kB\n")
            .expect("write fake meminfo");
        let denied = check_host_with(&path, |_| None).expect_err("1.5 GiB free on a 4 GiB host");
        assert_eq!(denied.requested_bytes, gib(2));
        assert_eq!(
            check_host_with(&path, |key| {
                (key == GUARD_ENV).then(|| "off".to_string())
            }),
            Ok(())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
