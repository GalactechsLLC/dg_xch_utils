use super::*;

// Resident set size in bytes from /proc/self/statm (Linux): field 2 is resident pages. 0 on any platform or
// read that does not expose it (the deployed node is Linux; local dev may see 0).
// jemalloc's cached stats refresh on epoch advance; reads report the linked jemalloc's view
// (near-zero when a test binary runs on the system allocator instead).
pub(super) fn jemalloc_stat_allocated() -> u64 {
    let _ = tikv_jemalloc_ctl::epoch::advance();
    tikv_jemalloc_ctl::stats::allocated::read().map_or(0, |v| v as u64)
}

pub(super) fn jemalloc_stat_resident() -> u64 {
    tikv_jemalloc_ctl::stats::resident::read().map_or(0, |v| v as u64)
}

pub(super) fn jemalloc_stat_active() -> u64 {
    tikv_jemalloc_ctl::stats::active::read().map_or(0, |v| v as u64)
}

pub(super) fn jemalloc_stat_retained() -> u64 {
    tikv_jemalloc_ctl::stats::retained::read().map_or(0, |v| v as u64)
}

/// One structured startup memory self-report, logged by the server right after the engine walk cache
/// is warmed (the last big startup allocation). The mm-node OOM died 8 seconds after start with zero
/// allocation evidence — a pod that dies before its first Prometheus scrape still leaves this line
/// in the pod log: RSS, jemalloc's four views, and the walk-cache record count that drove them.
pub fn log_startup_memory(context: &'static str, walk_cache_records: usize) {
    info!(
        "startup memory self-report after engine walk-cache warm event={} context={} walk_cache_records={} rss_bytes={} alloc_allocated_bytes={} alloc_active_bytes={} alloc_resident_bytes={} alloc_retained_bytes={}",
        "fullnode.startup.memory",
        context,
        walk_cache_records,
        process_rss_bytes(),
        jemalloc_stat_allocated(),
        jemalloc_stat_active(),
        jemalloc_stat_resident(),
        jemalloc_stat_retained()
    );
}

pub(super) fn process_rss_bytes() -> u64 {
    let Ok(statm) = std::fs::read_to_string("/proc/self/statm") else {
        return 0;
    };
    let Some(resident_pages) = statm
        .split_whitespace()
        .nth(1)
        .and_then(|p| p.parse::<u64>().ok())
    else {
        return 0;
    };
    // Linux page size is 4 KiB on the deployment target; assume it rather than link libc for one syscall.
    resident_pages.saturating_mul(4096)
}
