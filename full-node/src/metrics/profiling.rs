use super::*;

// Sample the process for FLAMEGRAPH_SECONDS and stream back the flamegraph SVG. Runs on its own spawned task
// (bounded to one concurrent via `profiling`) so /metrics keeps answering during the profile.

// The whole profile — start guard, sample, symbolize, render — runs inside one `spawn_blocking` closure so the
// `pprof::ProfilerGuard` is created and dropped on a single blocking thread (it never crosses an `.await`, so
// the spawned future stays `Send`) and the CPU work never steals a runtime worker.
pub(crate) async fn sample_flamegraph(seconds: u64) -> Result<Vec<u8>, String> {
    tokio::task::spawn_blocking(move || {
        let guard = pprof::ProfilerGuardBuilder::default()
            .frequency(FLAMEGRAPH_HZ)
            .blocklist(&["libc", "libgcc", "pthread", "vdso"])
            .build()
            .map_err(|e| format!("profiler start: {e}"))?;
        std::thread::sleep(Duration::from_secs(seconds));
        let report = guard
            .report()
            .build()
            .map_err(|e| format!("report build: {e}"))?;
        let mut svg = Vec::new();
        report
            .flamegraph(&mut svg)
            .map_err(|e| format!("flamegraph render: {e}"))?;
        Ok(svg)
    })
    .await
    .map_err(|e| format!("profiler task join: {e}"))?
}

// The decisive leak instrument: dump jemalloc's sampled heap profile (allocation-site stacks for the
// LIVE bytes the process holds) and stream it back. Symbolize offline against the container binary:
// `jeprof --show_bytes dg heap.prof --pdf` or `pprof -http=: dg heap.prof`.
//
// Requires the embedding binary's jemalloc to be built with its `profiling` feature.
// AND activated at PROCESS START via jemalloc's env var. tikv-jemalloc-sys builds with
// --with-jemalloc-prefix=_rjem_, so the variable the linked jemalloc reads is `_RJEM_MALLOC_CONF`
// (NOT plain `MALLOC_CONF`):
//   _RJEM_MALLOC_CONF=prof:true,prof_active:true,lg_prof_sample:19
// `opt.prof` cannot be flipped at runtime — a deploy without the env var (or a prof-less build, e.g.
// macOS dev) must fail LOUD here, never stream back an empty profile.

// The mallctl work runs on the blocking pool: `prof.dump` writes the profile file synchronously
// (file I/O inside jemalloc) and must not stall a runtime worker.
pub(crate) async fn dump_heap_profile() -> Result<Vec<u8>, String> {
    tokio::task::spawn_blocking(jemalloc_prof_dump)
        .await
        .map_err(|e| format!("heap dump task join: {e}"))?
}

// The activation contract, checked loud-first (see `handle_heap`): `opt.prof` reads Err on a build
// without jemalloc prof compiled in, `false` when compiled in but not enabled at start, and
// `prof.active` is the runtime sampling switch. Only then is `prof.dump` worth issuing.
const HEAP_PROF_HOWTO: &str =
    "start the process with _RJEM_MALLOC_CONF=prof:true,prof_active:true,lg_prof_sample:19";

pub(super) fn jemalloc_prof_dump() -> Result<Vec<u8>, String> {
    // SAFETY (all three mallctl calls): `opt.prof` and `prof.active` are bool-typed mallctl keys read
    // as Rust `bool` (1 byte, matching jemalloc's C bool — the same shape tikv_jemalloc_ctl's own
    // `profiling::prof` wrapper uses); `prof.dump`'s new-value is a `*const c_char` pointing at a
    // NUL-terminated path that outlives the call (jemalloc uses it synchronously during the mallctl).
    let compiled = unsafe { tikv_jemalloc_ctl::raw::read::<bool>(b"opt.prof\0") }.map_err(|e| {
        format!(
            "jemalloc heap profiling is not compiled into this build ({e}); \
                     build `dg` with its `profiling` feature and {HEAP_PROF_HOWTO}"
        )
    })?;
    if !compiled {
        return Err(format!(
            "jemalloc heap profiling is compiled in but was not enabled at process start; {HEAP_PROF_HOWTO}"
        ));
    }
    let active = unsafe { tikv_jemalloc_ctl::raw::read::<bool>(b"prof.active\0") }
        .map_err(|e| format!("failed to read prof.active ({e}); {HEAP_PROF_HOWTO}"))?;
    if !active {
        return Err(format!(
            "jemalloc heap profiling is enabled but sampling is inactive (prof_active:false); {HEAP_PROF_HOWTO}"
        ));
    }
    let path = std::env::temp_dir().join(format!(
        "full-node-heap-{}-{}.prof",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|e| format!("dump path contains NUL: {e}"))?;
    let dump = unsafe { tikv_jemalloc_ctl::raw::write(b"prof.dump\0", c_path.as_ptr()) };
    dump.map_err(|e| format!("prof.dump failed: {e}"))?;
    let prof = std::fs::read(&path).map_err(|e| format!("read dumped profile: {e}"));
    let _ = std::fs::remove_file(&path); // best-effort temp hygiene, success or not
    prof
}
