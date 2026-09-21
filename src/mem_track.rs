//! Optional heap accounting for debugging (enabled with `--features mem-debug`)
//!
//! Counts live bytes on every alloc/dealloc so we can attribute memory growth to
//! specific initialization steps. Never enabled in release builds by default.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

pub static LIVE: AtomicUsize = AtomicUsize::new(0);
pub static PEAK: AtomicUsize = AtomicUsize::new(0);

pub struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = System.alloc(l);
        if !p.is_null() {
            let live = LIVE.fetch_add(l.size(), Ordering::Relaxed) + l.size();
            PEAK.fetch_max(live, Ordering::Relaxed);
        }
        p
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Ordering::Relaxed);
        System.dealloc(p, l)
    }
}

pub fn live_mb() -> f64 {
    LIVE.load(Ordering::Relaxed) as f64 / 1_048_576.0
}

pub fn peak_mb() -> f64 {
    PEAK.load(Ordering::Relaxed) as f64 / 1_048_576.0
}

/// Current resident set size in MB (from /proc/self/statm).
pub fn rss_mb() -> f64 {
    let statm = std::fs::read_to_string("/proc/self/statm").unwrap_or_default();
    let pages: f64 = statm
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0);
    pages * 4096.0 / 1_048_576.0
}

/// Peak resident set size in MB (VmHWM from /proc/self/status).
pub fn peak_rss_mb() -> f64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            if let Some(kb) = rest.split_whitespace().next().and_then(|s| s.parse::<f64>().ok()) {
                return kb / 1024.0;
            }
        }
    }
    0.0
}

/// Print live/peak heap usage with a label.
pub fn snapshot(label: &str) {
    nprint!(
        "[mem] {:<44} heap_live={:9.1} MB  heap_peak={:9.1} MB  rss={:8.1} MB  rss_peak={:8.1} MB",
        label,
        live_mb(),
        peak_mb(),
        rss_mb(),
        peak_rss_mb()
    );
}
