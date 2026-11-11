//! Memory-limit detection for sizing the concurrency budget at startup.
//!
//! The upstream concurrency defaults must be derived from the memory the
//! process is actually allowed to use, not from a magic constant. In
//! containers that is the cgroup limit — NOT the host RAM — so the host
//! value is only a fallback. Detection order:
//!
//! 1. cgroup v2: `/sys/fs/cgroup/memory.max` (`"max"` = no limit → try v1)
//! 2. cgroup v1: `/sys/fs/cgroup/memory/memory.limit_in_bytes`
//!    (values near `u64::MAX` mean "unlimited" → try meminfo)
//! 3. `/proc/meminfo` `MemTotal` (bare-metal host RAM)
//! 4. `None` (e.g. Windows dev machines): callers fall back to conservative
//!    hardcoded defaults.

use std::fs;

/// cgroup v1 writes a placeholder close to `u64::MAX` when no limit is set.
const CGROUP_V1_UNLIMITED: u64 = 1 << 60;

/// Memory-limit sources, most specific first. Used for the startup log so
/// operators can see which number the budget was derived from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryLimitSource {
    CgroupV2,
    CgroupV1,
    Meminfo,
}

impl MemoryLimitSource {
    pub fn as_str(self) -> &'static str {
        match self {
            MemoryLimitSource::CgroupV2 => "cgroup v2 (memory.max)",
            MemoryLimitSource::CgroupV1 => "cgroup v1 (memory.limit_in_bytes)",
            MemoryLimitSource::Meminfo => "/proc/meminfo (host RAM)",
        }
    }
}

/// Detects the memory limit this process may consume.
pub fn detect_memory_limit_bytes() -> Option<(u64, MemoryLimitSource)> {
    if let Some(bytes) = read_cgroup_v2() {
        return Some((bytes, MemoryLimitSource::CgroupV2));
    }
    if let Some(bytes) = read_cgroup_v1() {
        return Some((bytes, MemoryLimitSource::CgroupV1));
    }
    if let Some(bytes) = read_meminfo() {
        return Some((bytes, MemoryLimitSource::Meminfo));
    }
    None
}

fn read_cgroup_v2() -> Option<u64> {
    let content = fs::read_to_string("/sys/fs/cgroup/memory.max").ok()?;
    parse_cgroup_v2_limit(&content)
}

fn read_cgroup_v1() -> Option<u64> {
    let content = fs::read_to_string("/sys/fs/cgroup/memory/memory.limit_in_bytes").ok()?;
    parse_cgroup_v1_limit(&content)
}

fn read_meminfo() -> Option<u64> {
    let content = fs::read_to_string("/proc/meminfo").ok()?;
    parse_meminfo_total(&content)
}

/// cgroup v2: a decimal byte count, or the literal `max` for unlimited.
fn parse_cgroup_v2_limit(content: &str) -> Option<u64> {
    let value = content.trim();
    if value == "max" {
        return None;
    }
    value.parse::<u64>().ok()
}

/// cgroup v1: a decimal byte count; near-`u64::MAX` placeholders mean the
/// limit is not set (the machine's physical memory, not a container quota).
fn parse_cgroup_v1_limit(content: &str) -> Option<u64> {
    let bytes = content.trim().parse::<u64>().ok()?;
    if bytes >= CGROUP_V1_UNLIMITED {
        return None;
    }
    Some(bytes)
}

/// `/proc/meminfo`: `MemTotal:       32612304 kB` (kB is 1024 bytes here).
fn parse_meminfo_total(content: &str) -> Option<u64> {
    let line = content
        .lines()
        .find(|l| l.starts_with("MemTotal:"))?;
    let kb: u64 = line
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    Some(kb.saturating_mul(1024))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cgroup_v2_parses_byte_count() {
        assert_eq!(
            parse_cgroup_v2_limit("4294967296\n"),
            Some(4 * 1024 * 1024 * 1024)
        );
    }

    #[test]
    fn cgroup_v2_max_means_unlimited() {
        assert_eq!(parse_cgroup_v2_limit("max\n"), None);
    }

    #[test]
    fn cgroup_v2_rejects_garbage() {
        assert_eq!(parse_cgroup_v2_limit("banana"), None);
        assert_eq!(parse_cgroup_v2_limit(""), None);
    }

    #[test]
    fn cgroup_v1_placeholder_means_unlimited() {
        assert_eq!(parse_cgroup_v1_limit("9223372036854771712\n"), None);
    }

    #[test]
    fn cgroup_v1_parses_real_limit() {
        assert_eq!(parse_cgroup_v1_limit("2147483648\n"), Some(2 * 1024 * 1024 * 1024));
    }

    #[test]
    fn meminfo_parses_mem_total() {
        let content = "MemFree:        100 kB\nMemTotal:       32612304 kB\nSwapTotal:      0 kB\n";
        assert_eq!(parse_meminfo_total(content), Some(32612304 * 1024));
    }

    #[test]
    fn meminfo_missing_line_is_none() {
        assert_eq!(parse_meminfo_total("MemFree: 100 kB\n"), None);
    }
}
