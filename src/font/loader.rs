//! Font file loading: memory-mapped, de-duplicated, face-index aware.
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
//!
//! Font *collections* (`.ttc`/`.otc`) additionally need a face index: Sarasa
//! Gothic packs 48 faces into one file (J/K/SC/TC/CL/HC × Fixed/Term/Gothic/…),
//! where face 0 is "Sarasa Fixed CL" but the family a config asks for
//! ("Sarasa Term SC Nerd Font") is face 34. Loading face 0 silently swaps the
//! glyph shapes, so the index travels with the bytes in [`FontFace`].

use anyhow::{anyhow, Context};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

static FONT_CACHE: LazyLock<Mutex<HashMap<PathBuf, &'static [u8]>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// A loaded font: memory-mapped bytes plus the face index inside the file.
#[derive(Clone, Copy, Debug)]
pub struct FontFace {
    /// Font file bytes (mmap'd and leaked for the process lifetime).
    pub data: &'static [u8],
    /// Face index inside the file (0 for single-face fonts).
    pub index: i32,
}

impl FontFace {
    pub fn new(data: &'static [u8], index: i32) -> Self {
        Self { data, index }
    }
}

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

/// Resolve a font specifier to a file path plus face index (no mapping).
///
/// Accepted forms:
/// - `/abs/path.ttf`, `relative/path.ttf`
/// - `/abs/path.ttc#34`                        — explicit face index
/// - `/abs/path.ttc#Sarasa Term SC Nerd Font`  — face picked by family name
/// - `Sarasa Term SC Nerd Font`                — fontconfig lookup (index included)
pub fn resolve_font_spec(specifier: &str) -> anyhow::Result<(PathBuf, i32)> {
    if let Some((path, suffix)) = split_face_suffix(specifier) {
        let index = match suffix {
            FaceSuffix::Index(index) => index,
            FaceSuffix::Family(family) => {
                let data = load_font_static(&path)?;
                face_index_by_family(data, &family).ok_or_else(|| {
                    anyhow!(
                        "Font \"{}\": no face named \"{}\" in {}",
                        specifier,
                        family,
                        path.display()
                    )
                })?
            }
        };
        log::info!(
            "Font loaded from path: {} (face {})",
            path.display(),
            index
        );
        return Ok((path, index));
    }

    super::fontconfig::resolve_font_face(specifier)
}

/// Resolve a font specifier and map its bytes.
///
/// Prefer this over [`load_font_static`] when the specifier comes from the
/// config: it carries the face index for `.ttc` collections.
pub fn load_font(specifier: &str) -> anyhow::Result<FontFace> {
    let (path, index) = resolve_font_spec(specifier)?;
    let data = load_font_static(&path)
        .with_context(|| format!("Failed to map font file {}", path.display()))?;
    Ok(FontFace::new(data, index))
}

/// Load the system monospace font (mmap'd and shared).
///
/// Honours the `NCON_FONT` environment variable, then falls back to
/// fontconfig's best monospace match.
pub fn load_system_font() -> anyhow::Result<FontFace> {
    if let Ok(path) = std::env::var("NCON_FONT") {
        let data = load_font_static(Path::new(&path))?;
        log::info!("Font loaded: {} (NCON_FONT)", path);
        return Ok(FontFace::new(data, 0));
    }

    let (path, index) = super::fontconfig::system_font_face()?;
    let data = load_font_static(&path)
        .with_context(|| format!("Failed to map font file {}", path.display()))?;
    Ok(FontFace::new(data, index))
}

/// Load a system CJK font (mmap'd and shared).
pub fn load_cjk_font() -> Option<FontFace> {
    let (path, index) = super::fontconfig::cjk_font_face()?;
    match load_font_static(&path) {
        Ok(data) => Some(FontFace::new(data, index)),
        Err(e) => {
            log::warn!("Failed to map CJK font {}: {}", path.display(), e);
            None
        }
    }
}

/// Suffix of a `path#…` font specifier.
enum FaceSuffix {
    /// `path#34`
    Index(i32),
    /// `path#Sarasa Term SC Nerd Font`
    Family(String),
}

/// Split `path#index` / `path#Family Name`.
///
/// Only treats `#` as a face suffix when the part before it exists as a file,
/// so paths that legitimately contain `#` still resolve as plain paths.
fn split_face_suffix(specifier: &str) -> Option<(PathBuf, FaceSuffix)> {
    let (path, suffix) = specifier.split_once('#')?;
    let path = Path::new(path);
    if path.as_os_str().is_empty() || !path.exists() {
        return None;
    }

    if let Ok(index) = suffix.trim().parse::<i32>() {
        return Some((path.to_path_buf(), FaceSuffix::Index(index)));
    }

    let family = suffix.trim();
    if family.is_empty() {
        return None;
    }
    Some((
        path.to_path_buf(),
        FaceSuffix::Family(family.to_string()),
    ))
}

/// Index of the face whose family name matches `family`.
///
/// Comparison ignores case and whitespace: FreeType reports the legacy
/// `name` table entry (`SarasaTermSC Nerd Font`) while fontconfig/configs use
/// the typographic one (`Sarasa Term SC Nerd Font`). Used for `path#<family>`
/// specifiers, where fontconfig cannot tell us the index.
fn face_index_by_family(data: &'static [u8], family: &str) -> Option<i32> {
    let library = freetype::Library::init().ok()?;
    let num_faces = library
        .new_memory_face2(data, 0)
        .ok()
        .map(|face| face.raw().num_faces)?;

    let want = normalize_family(family);
    for index in 0..num_faces.max(1) {
        let Ok(face) = library.new_memory_face2(data, index as isize) else {
            continue;
        };
        let matches = face
            .family_name()
            .is_some_and(|name| normalize_family(&name) == want);
        if matches {
            return Some(index as i32);
        }
    }
    None
}

/// Lowercase and drop whitespace so `Sarasa Term SC` == `SarasaTermSC`.
fn normalize_family(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write a dummy file so `split_face_suffix` sees an existing path.
    fn dummy_file(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("ncon-test-{}-{}", std::process::id(), name));
        std::fs::write(&path, b"not a real font").unwrap();
        path
    }

    #[test]
    fn face_suffix_parses_index() {
        let file = dummy_file("index.ttc");
        let (path, suffix) = split_face_suffix(&format!("{}#34", file.display())).unwrap();
        assert_eq!(path, file);
        assert!(matches!(suffix, FaceSuffix::Index(34)));
    }

    #[test]
    fn face_suffix_parses_family() {
        let file = dummy_file("family.ttc");
        let spec = format!("{}#Sarasa Term SC Nerd Font", file.display());
        let (path, suffix) = split_face_suffix(&spec).unwrap();
        assert_eq!(path, file);
        match suffix {
            FaceSuffix::Family(name) => assert_eq!(name, "Sarasa Term SC Nerd Font"),
            FaceSuffix::Index(_) => panic!("expected a family suffix"),
        }
    }

    #[test]
    fn plain_path_and_family_names_are_untouched() {
        assert!(split_face_suffix("/usr/share/fonts/TTF/DejaVuSans.ttf").is_none());
        // Family names may contain '#'-like characters; only existing paths split.
        assert!(split_face_suffix("Sarasa Term SC#Nerd Font").is_none());
        // Trailing '#' with no suffix is not a face request.
        let file = dummy_file("empty.ttc");
        assert!(split_face_suffix(&format!("{}#", file.display())).is_none());
    }

    #[test]
    fn resolve_spec_keeps_fontconfig_index_for_collections() {
        // Environment-dependent: only runs where the font is installed.
        let Ok((path, index)) = resolve_font_spec("Sarasa Term SC Nerd Font") else {
            eprintln!("skipping: Sarasa Term SC Nerd Font not installed");
            return;
        };
        assert!(
            path.extension().is_some_and(|e| e == "ttc"),
            "expected a collection, got {}",
            path.display()
        );
        assert_ne!(index, 0, "fontconfig should point at the SC face, not face 0");
        // And that face really is the requested family (FreeType reports the
        // legacy name-table entry, so compare with whitespace removed).
        let data = load_font_static(&path).unwrap();
        let library = freetype::Library::init().unwrap();
        let face = library.new_memory_face2(data, index as isize).unwrap();
        let family = face.family_name().unwrap_or_default();
        assert_eq!(
            normalize_family(&family),
            normalize_family("Sarasa Term SC Nerd Font"),
            "face {index} is {family:?}"
        );
    }

    #[test]
    fn family_normalization_ignores_case_and_spaces() {
        assert_eq!(
            normalize_family("Sarasa Term SC Nerd Font"),
            normalize_family("sarasaTermSC nerd  font")
        );
    }

    #[test]
    fn face_index_by_family_matches_fontconfig() {
        let Ok((path, index)) = resolve_font_spec("Sarasa Term SC Nerd Font") else {
            eprintln!("skipping: Sarasa Term SC Nerd Font not installed");
            return;
        };
        let data = load_font_static(&path).unwrap();
        assert_eq!(face_index_by_family(data, "sarasa term sc nerd font"), Some(index));
        assert_eq!(face_index_by_family(data, "No Such Family"), None);
    }
}
