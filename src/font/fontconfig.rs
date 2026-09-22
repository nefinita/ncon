//! fontconfig integration
//!
//! Search and select system fonts

use anyhow::{anyhow, Result};
use fontconfig::Fontconfig;
use log::{info, warn};
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
    pub fn find_cjk(&self) -> Option<FontMatch> {
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
