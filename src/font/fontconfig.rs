//! fontconfig integration
//!
//! Search and select system fonts

use anyhow::{anyhow, Result};
use fontconfig::{Fontconfig, Pattern};
use log::{info, warn};
use std::ffi::CString;
use std::path::{Path, PathBuf};

/// Font search result
#[derive(Debug, Clone)]
pub struct FontMatch {
    /// Font file path
    pub path: PathBuf,
    /// Font name
    pub family: String,
    /// Face index inside the file (font collections hold several faces)
    pub index: i32,
}

/// Search fonts using fontconfig
pub struct FontFinder {
    fc: Fontconfig,
}

impl FontFinder {
    /// Initialize FontFinder
    pub fn new() -> Result<Self> {
        let fc = Fontconfig::new().ok_or_else(|| anyhow!("fontconfig initialization failed"))?;
        info!("fontconfig initialized");
        Ok(Self { fc })
    }

    /// Search by font name
    /// Verifies that the returned font actually matches the requested family name
    /// (fontconfig always returns the "closest" match, even if completely unrelated)
    pub fn find_font(&self, family: &str) -> Option<FontMatch> {
        // Use fontconfig's find method
        if let Some(font) = self.fc.find(family, None) {
            // Verify the returned font name matches the request
            // fontconfig returns "best match" which may be completely unrelated
            let req = family.to_ascii_lowercase();
            let got = font.name.to_ascii_lowercase();
            if got.contains(&req) || req.contains(&got) {
                return Some(FontMatch {
                    path: font.path,
                    family: font.name,
                    index: font.index.unwrap_or(0),
                });
            }
            warn!(
                "fontconfig: rejected false match for \"{}\": got \"{}\"",
                family, font.name
            );
            return None;
        }
        None
    }

    /// Search for monospace font
    pub fn find_monospace(&self) -> Option<FontMatch> {
        // Fallback candidates
        let fallbacks = [
            "DejaVu Sans Mono",
            "Liberation Mono",
            "Noto Sans Mono",
            "Source Code Pro",
            "Inconsolata",
            "Courier New",
            "monospace",
        ];

        for name in fallbacks {
            if let Some(m) = self.find_font(name) {
                return Some(m);
            }
        }

        warn!("Monospace font not found");
        None
    }

    /// Search for CJK font
    ///
    /// A zh/ja/ko locale decides which regional variant is right
    /// (SC/TC/HK/JP/KR); without one we keep the historical fallbacks.
    pub fn find_cjk(&self) -> Option<FontMatch> {
        if let Some(lang) = locale_lang_tag() {
            for family in cjk_families_for_lang(lang) {
                if let Some(m) = self.find_font(family) {
                    info!(
                        "CJK font (locale {}): {} ({}, face {})",
                        lang,
                        m.family,
                        m.path.display(),
                        m.index
                    );
                    return Some(m);
                }
            }

            // None of the curated families is installed: let fontconfig pick
            // any font that supports the language.
            if let Some(m) = self.find_font_for_lang(lang) {
                return Some(m);
            }
            warn!("CJK font for locale {} not found via fontconfig", lang);
        }

        // Non-CJK locale (or nothing found above): historical fallbacks.
        let candidates = [
            "Noto Sans CJK JP",
            "Noto Sans CJK",
            "Source Han Sans",
            "IPA Gothic",
            "IPAGothic",
            "VL Gothic",
            "Takao Gothic",
        ];

        for name in candidates {
            if let Some(m) = self.find_font(name) {
                return Some(m);
            }
        }

        warn!("CJK font not found");
        None
    }

    /// Let fontconfig choose a font supporting `lang`, with no family preference.
    ///
    /// Only used when no curated family for the locale is installed:
    /// fontconfig's ranking may prefer fonts that are not great for terminals
    /// (e.g. bitmap-hinted WenQuanYi Zen Hei), but it stays language-correct.
    fn find_font_for_lang(&self, lang: &str) -> Option<FontMatch> {
        let mut pattern = Pattern::new(&self.fc);
        pattern.add_string(fontconfig::FC_LANG.as_cstr(), &CString::new(lang).ok()?);
        let matched = pattern.font_match();

        let family = matched.name()?.to_string();
        let path = PathBuf::from(matched.filename()?);
        let index = matched.face_index().unwrap_or(0);
        info!(
            "CJK font (fontconfig lang={}): {} ({}, face {})",
            lang,
            family,
            path.display(),
            index
        );
        Some(FontMatch { path, family, index })
    }

    /// Search for color emoji font
    pub fn find_emoji(&self) -> Option<FontMatch> {
        let candidates = [
            "Noto Color Emoji",
            "Apple Color Emoji",
            "Twemoji",
            "EmojiOne",
        ];

        for name in candidates {
            if let Some(m) = self.find_font(name) {
                return Some(m);
            }
        }

        warn!("Color emoji font not found");
        None
    }

    /// Search for Nerd Font (symbol/icon font)
    pub fn find_nerd_font(&self) -> Option<FontMatch> {
        let candidates = [
            // Nerd Font variants (most common)
            "Hack Nerd Font Mono",
            "Hack Nerd Font",
            "HackNerdFontMono",
            "HackNerdFont",
            "FiraCode Nerd Font Mono",
            "FiraCode Nerd Font",
            "FiraCodeNerdFontMono",
            "FiraCodeNerdFont",
            "JetBrainsMono Nerd Font Mono",
            "JetBrainsMono Nerd Font",
            "JetBrainsMonoNerdFontMono",
            "JetBrainsMonoNerdFont",
            "DejaVuSansMono Nerd Font Mono",
            "DejaVuSansMono Nerd Font",
            "DejaVuSansM Nerd Font Mono",
            "DejaVuSansM Nerd Font",
            "SauceCodePro Nerd Font Mono",
            "SauceCodePro Nerd Font",
            "Symbols Nerd Font Mono",
            "Symbols Nerd Font",
        ];

        for name in candidates {
            if let Some(m) = self.find_font(name) {
                return Some(m);
            }
        }

        None
    }
}

/// Preferred CJK families per fontconfig language tag, best first.
///
/// Kept explicit instead of trusting fontconfig's own `lang=` ranking, which
/// can prefer bitmap-hinted fonts (WenQuanYi Zen Hei) over Noto Sans CJK SC on
/// a zh-cn system.
fn cjk_families_for_lang(lang: &str) -> &'static [&'static str] {
    match lang {
        "zh-cn" => &[
            "Noto Sans CJK SC",
            "Source Han Sans SC",
            "Sarasa Term SC",
            "Sarasa Gothic SC",
        ],
        "zh-tw" => &[
            "Noto Sans CJK TC",
            "Source Han Sans TC",
            "Sarasa Term TC",
            "Sarasa Gothic TC",
        ],
        "zh-hk" => &[
            "Noto Sans CJK HK",
            "Source Han Sans HC",
            "Sarasa Term HC",
            "Sarasa Gothic HC",
        ],
        "ja" => &[
            "Noto Sans CJK JP",
            "Source Han Sans JP",
            "Sarasa Term J",
            "Sarasa Gothic J",
        ],
        "ko" => &[
            "Noto Sans CJK KR",
            "Source Han Sans KR",
            "Sarasa Term K",
            "Sarasa Gothic K",
        ],
        _ => &[],
    }
}

/// Map a POSIX locale to a fontconfig language tag for CJK, if any.
///
/// `zh_CN.UTF-8` → `zh-cn`, `zh_TW` → `zh-tw`, `zh_HK` → `zh-hk`,
/// `ja_JP@…` → `ja`, `ko_KR` → `ko`; non-CJK locales return `None`.
fn lang_tag_from_locale(locale: &str) -> Option<&'static str> {
    let base = locale.split(['.', '@']).next().unwrap_or(locale);
    let mut parts = base.split(['_', '-']);
    let language = parts.next().unwrap_or("").to_ascii_lowercase();
    let territory = parts.next().unwrap_or("").to_ascii_uppercase();

    match language.as_str() {
        "zh" => Some(match territory.as_str() {
            "TW" => "zh-tw",
            "HK" | "MO" => "zh-hk",
            _ => "zh-cn",
        }),
        "ja" => Some("ja"),
        "ko" => Some("ko"),
        _ => None,
    }
}

/// fontconfig language tag for the current locale (`LC_ALL` → `LC_CTYPE` → `LANG`).
///
/// The first variable naming a CJK language wins: an `en_US` `LC_CTYPE` only
/// overrides character classes, so it should not shadow a `zh_CN` `LANG`.
fn locale_lang_tag() -> Option<&'static str> {
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .filter_map(|var| std::env::var(var).ok())
        .filter(|value| !value.is_empty() && value != "C" && value != "POSIX")
        .find_map(|value| lang_tag_from_locale(&value))
}

/// Load font file
#[allow(dead_code)]
pub fn load_font_file(path: &std::path::Path) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|e| anyhow!("Failed to read font file: {} ({})", path.display(), e))
}

/// Resolve a font specifier to a file path plus face index (no read, no cache).
///
/// Lookup order: absolute path, fontconfig family name, then relative path.
///
/// The face index matters for font collections: fontconfig reports which face
/// of a `.ttc` matches the requested family. The mmap loader
/// ([`crate::font::loader::load_font`]) also understands an explicit
/// `path#index` / `path#family` suffix, which this function does not handle.
pub fn resolve_font_face(specifier: &str) -> Result<(PathBuf, i32)> {
    let path = Path::new(specifier);
    if path.is_absolute() && path.exists() {
        info!("Font loaded from path: {}", specifier);
        return Ok((path.to_path_buf(), 0));
    }

    // Try as font family name via fontconfig
    let finder = FontFinder::new()?;
    if let Some(font_match) = finder.find_font(specifier) {
        info!(
            "Font resolved by name: \"{}\" → {} ({}, face {})",
            specifier,
            font_match.family,
            font_match.path.display(),
            font_match.index
        );
        return Ok((font_match.path, font_match.index));
    }

    // Last resort: try as relative path
    if path.exists() {
        info!("Font loaded from relative path: {}", specifier);
        return Ok((path.to_path_buf(), 0));
    }

    Err(anyhow!(
        "Font not found: \"{}\" (not a valid path or font name)",
        specifier
    ))
}

/// Search and load system font using fontconfig (kept for API completeness)
#[allow(dead_code)]
pub fn load_system_font_fc() -> Result<Vec<u8>> {
    let finder = FontFinder::new()?;

    if let Some(font_match) = finder.find_monospace() {
        info!(
            "System font (fontconfig): {} ({})",
            font_match.family,
            font_match.path.display()
        );
        return load_font_file(&font_match.path);
    }

    Err(anyhow!("Monospace font not found via fontconfig"))
}

/// Find the system monospace font path + face index (no file read, no copy).
pub fn system_font_face() -> Result<(PathBuf, i32)> {
    let finder = FontFinder::new()?;
    if let Some(font_match) = finder.find_monospace() {
        info!(
            "System font (fontconfig): {} ({}, face {})",
            font_match.family,
            font_match.path.display(),
            font_match.index
        );
        return Ok((font_match.path, font_match.index));
    }
    Err(anyhow!("Monospace font not found via fontconfig"))
}

/// Find a CJK font path + face index (no file read, no copy).
pub fn cjk_font_face() -> Option<(PathBuf, i32)> {
    let finder = FontFinder::new().ok()?;
    let m = finder.find_cjk()?;
    info!(
        "CJK font (fontconfig): {} ({}, face {})",
        m.family,
        m.path.display(),
        m.index
    );
    Some((m.path, m.index))
}

/// Search and load CJK font using fontconfig (kept for API completeness)
#[allow(dead_code)]
pub fn load_cjk_font_fc() -> Option<Vec<u8>> {
    let finder = match FontFinder::new() {
        Ok(f) => f,
        Err(e) => {
            warn!("fontconfig initialization failed: {:?}", e);
            return None;
        }
    };

    if let Some(font_match) = finder.find_cjk() {
        info!(
            "CJK font (fontconfig): {} ({})",
            font_match.family,
            font_match.path.display()
        );
        return load_font_file(&font_match.path).ok();
    }

    None
}

/// Search and load emoji font using fontconfig
#[allow(dead_code)]
pub fn load_emoji_font_fc() -> Option<Vec<u8>> {
    let finder = match FontFinder::new() {
        Ok(f) => f,
        Err(e) => {
            warn!("fontconfig initialization failed: {:?}", e);
            return None;
        }
    };

    if let Some(font_match) = finder.find_emoji() {
        info!(
            "Emoji font (fontconfig): {} ({})",
            font_match.family,
            font_match.path.display()
        );
        return load_font_file(&font_match.path).ok();
    }

    None
}

/// Find a font that supports a specific Unicode codepoint using fontconfig.
/// Uses `fc-match` command with charset query.
/// Returns the font file path and face index if found.
pub fn find_font_for_char(ch: char) -> Option<(PathBuf, i32)> {
    use std::process::Command;

    let charset_query = format!(":charset={:04X}", ch as u32);
    let output = Command::new("fc-match")
        .args(["-f", "%{file}\n%{index}", &charset_query])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8(output.stdout).ok()?;
    let mut lines = stdout.lines();
    let path_str = lines.next()?.trim();
    if path_str.is_empty() {
        return None;
    }
    // `%{index}` is empty for some fonts — default to face 0.
    let index = lines
        .next()
        .and_then(|s| s.trim().parse::<i32>().ok())
        .unwrap_or(0);

    let path = PathBuf::from(path_str);
    if path.exists() {
        Some((path, index))
    } else {
        None
    }
}

/// Find Nerd Font path + face index (mmap'd by the caller).
pub fn nerd_font_face() -> Option<(PathBuf, i32)> {
    let finder = FontFinder::new().ok()?;
    let font_match = finder.find_nerd_font()?;
    info!(
        "Nerd Font (fontconfig): {} ({}, face {})",
        font_match.family,
        font_match.path.display(),
        font_match.index
    );
    Some((font_match.path, font_match.index))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_tags_map_to_cjk_variants() {
        assert_eq!(lang_tag_from_locale("zh_CN.UTF-8"), Some("zh-cn"));
        assert_eq!(lang_tag_from_locale("zh_SG.UTF-8"), Some("zh-cn"));
        assert_eq!(lang_tag_from_locale("zh_TW.UTF-8"), Some("zh-tw"));
        assert_eq!(lang_tag_from_locale("zh_HK.UTF-8"), Some("zh-hk"));
        assert_eq!(lang_tag_from_locale("zh_MO"), Some("zh-hk"));
        assert_eq!(lang_tag_from_locale("ja_JP.UTF-8"), Some("ja"));
        assert_eq!(lang_tag_from_locale("ko_KR.UTF-8@euckr"), Some("ko"));
        // Non-CJK locales must not invent a CJK preference.
        assert_eq!(lang_tag_from_locale("en_US.UTF-8"), None);
        assert_eq!(lang_tag_from_locale("C"), None);
        assert_eq!(lang_tag_from_locale("POSIX"), None);
    }

    #[test]
    fn cjk_families_follow_the_language() {
        let sc = cjk_families_for_lang("zh-cn");
        assert!(sc.iter().any(|f| f.contains("SC")), "zh-cn wants a Simplified Chinese family");
        assert!(!sc.iter().any(|f| f.contains("JP")), "zh-cn must not fall back to Japanese");

        assert!(cjk_families_for_lang("zh-tw").iter().any(|f| f.contains("TC")));
        assert!(cjk_families_for_lang("zh-hk").iter().any(|f| f.contains("HC")));
        assert!(cjk_families_for_lang("ja").iter().any(|f| f.contains("JP")));
        assert!(cjk_families_for_lang("ko").iter().any(|f| f.contains("KR")));
        assert!(cjk_families_for_lang("en").is_empty());
    }

    /// Integration: fontconfig must be able to name a zh-cn font on a machine
    /// that has CJK fonts at all (the curated list is only the first choice).
    #[test]
    fn language_query_returns_a_font() {
        let Ok(finder) = FontFinder::new() else {
            return;
        };
        match finder.find_font_for_lang("zh-cn") {
            Some(m) => {
                assert!(
                    m.path.exists(),
                    "fontconfig returned a missing file: {}",
                    m.path.display()
                );
                assert!(m.index >= 0);
            }
            None => eprintln!("skipping: fontconfig knows no zh-cn font"),
        }
    }
}
