//! Font file loading: memory-mapped and de-duplicated.
//!
//! Font bytes must stay alive for the whole process (`'static`) because both
//! rustybuzz and FreeType keep borrowing them. The naive implementation
//! (`std::fs::read` + `Box::leak`) costs one *anonymous* copy per role
//! (main / cjk / symbols) — for a 200 MB CJK collection that is hundreds of
//! megabytes of unreclaimable heap.
//!
//! Instead we map the file once per canonical path and hand out a shared
//! `&'static [u8]`. The pages stay file-backed, so the kernel can evict them
//! under memory pressure, and the same file referenced by several roles is
//! only mapped once.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

static FONT_CACHE: LazyLock<Mutex<HashMap<PathBuf, &'static [u8]>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Map a font file as a leaked, de-duplicated byte slice.
pub fn load_font_static(path: &Path) -> std::io::Result<&'static [u8]> {
    let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());

    if let Some(data) = FONT_CACHE.lock().unwrap().get(&key) {
        return Ok(data);
    }

    let file = std::fs::File::open(&key)?;
    // SAFETY: the mapping is never mutated and is intentionally leaked for the
    // process lifetime, which satisfies rustybuzz/FreeType's `'static` borrow.
    let mmap = unsafe { memmap2::Mmap::map(&file)? };
    let leaked: &'static memmap2::Mmap = Box::leak(Box::new(mmap));
    let data: &'static [u8] = leaked;

    FONT_CACHE.lock().unwrap().insert(key, data);
    Ok(data)
}

/// Number of distinct font files currently mapped (for diagnostics).
pub fn mapped_font_count() -> usize {
    FONT_CACHE.lock().unwrap().len()
}

/// Load the system monospace font (mmap'd and shared).
///
/// Honours the `NCON_FONT` environment variable, then falls back to
/// fontconfig's best monospace match.
pub fn load_system_font() -> anyhow::Result<&'static [u8]> {
    if let Ok(path) = std::env::var("NCON_FONT") {
        let data = load_font_static(std::path::Path::new(&path))?;
        log::info!("Font loaded: {} (NCON_FONT)", path);
        return Ok(data);
    }

    let path = super::fontconfig::system_font_path()?;
    load_font_static(&path).map_err(|e| anyhow::anyhow!("Failed to map font {}: {}", path.display(), e))
}

/// Load a system CJK font (mmap'd and shared).
pub fn load_cjk_font() -> Option<&'static [u8]> {
    let path = super::fontconfig::cjk_font_path()?;
    match load_font_static(&path) {
        Ok(data) => Some(data),
        Err(e) => {
            log::warn!("Failed to map CJK font {}: {}", path.display(), e);
            None
        }
    }
}
