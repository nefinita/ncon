//! FreeType wrapper
//!
//! High-quality text rendering with LCD subpixel rendering

#![allow(dead_code)]

use anyhow::{anyhow, Result};
use freetype::face::LoadFlag;
use freetype::ffi::FT_Library;
use freetype::render_mode::RenderMode;
use freetype::{LcdFilter, Library};
use log::info;
use std::rc::Rc;
use std::sync::Arc;

// Directly declare functions not exported by freetype-sys
extern "C" {
    fn FT_Library_SetLcdFilterWeights(
        library: FT_Library,
        weights: *const u8,
    ) -> freetype::ffi::FT_Error;

    fn FT_Set_Transform(
        face: freetype::ffi::FT_Face,
        matrix: *const freetype::ffi::FT_Matrix,
        delta: *const freetype::ffi::FT_Vector,
    );

    fn FT_GlyphSlot_Embolden(slot: freetype::ffi::FT_GlyphSlot);
}

/// Subpixel phase (0, 1/3, 2/3 pixel offset)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SubpixelPhase {
    /// 0/3 pixel (integer position)
    Phase0 = 0,
    /// 1/3 pixel
    Phase1 = 1,
    /// 2/3 pixel
    Phase2 = 2,
}

impl SubpixelPhase {
    /// Calculate phase from fractional part
    /// frac: range 0.0..1.0
    pub fn from_frac(frac: f32) -> Self {
        // 0.0-0.166.. -> Phase0, 0.166..-0.5 -> Phase1, 0.5-0.833.. -> Phase2, 0.833..-1.0 -> Phase0
        // Using 1/6 as boundary selects the nearest phase
        let phase = ((frac + 1.0 / 6.0) * 3.0) as u32 % 3;
        match phase {
            0 => Self::Phase0,
            1 => Self::Phase1,
            _ => Self::Phase2,
        }
    }

    /// Phase offset (in pixels)
    pub fn offset(self) -> f32 {
        match self {
            Self::Phase0 => 0.0,
            Self::Phase1 => 1.0 / 3.0,
            Self::Phase2 => 2.0 / 3.0,
        }
    }

    /// 16.16 fixed-point offset for FreeType
    fn fixed_offset(self) -> i64 {
        match self {
            Self::Phase0 => 0,
            Self::Phase1 => 0x5555, // 1/3 * 65536 ≈ 21845
            Self::Phase2 => 0xAAAA, // 2/3 * 65536 ≈ 43690
        }
    }
}

/// LCD rendering mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum LcdMode {
    /// Normal grayscale (R8)
    Grayscale,
    /// Horizontal RGB subpixel (LCD_V)
    LcdHorizontal,
    /// Vertical RGB subpixel
    LcdVertical,
}

impl Default for LcdMode {
    fn default() -> Self {
        Self::LcdHorizontal
    }
}

/// LCD filter setting
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum LcdFilterMode {
    None,
    Default,
    Light,
    Legacy,
    /// Custom weights (specified via lcd_weights)
    Custom,
}

impl Default for LcdFilterMode {
    fn default() -> Self {
        Self::Light
    }
}

impl LcdFilterMode {
    /// Convert from config string
    pub fn from_str(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "none" => Self::None,
            "default" => Self::Default,
            "light" => Self::Light,
            "legacy" => Self::Legacy,
            "custom" => Self::Custom,
            _ => Self::Light,
        }
    }

    fn to_freetype(self) -> Option<LcdFilter> {
        match self {
            Self::None => Some(LcdFilter::LcdFilterNone),
            Self::Default => Some(LcdFilter::LcdFilterDefault),
            Self::Light => Some(LcdFilter::LcdFilterLight),
            Self::Legacy => Some(LcdFilter::LcdFilterLegacy),
            Self::Custom => None, // Custom is configured via weights
        }
    }
}

/// Hinting mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(dead_code)]
pub enum HintingMode {
    /// Normal hinting (sharp, slightly thicker)
    #[default]
    Normal,
    /// Light hinting (natural curves, slightly thinner)
    Light,
    /// No hinting (macOS style, most natural)
    None,
}

impl HintingMode {
    pub fn from_str(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "light" => Self::Light,
            "none" | "off" => Self::None,
            _ => Self::Normal,
        }
    }

    fn to_load_flag(self) -> LoadFlag {
        match self {
            Self::Normal => LoadFlag::TARGET_LCD,
            Self::Light => LoadFlag::TARGET_LIGHT,
            Self::None => LoadFlag::NO_HINTING,
        }
    }
}

/// LCD subpixel order
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum LcdSubpixel {
    /// Horizontal RGB (common)
    Rgb,
    /// Horizontal BGR
    Bgr,
    /// Vertical RGB
    VRgb,
    /// Vertical BGR
    VBgr,
    /// Auto-detect (from DRM rotation)
    Auto,
}

impl Default for LcdSubpixel {
    fn default() -> Self {
        Self::Rgb
    }
}

impl LcdSubpixel {
    /// Convert from config string
    pub fn from_str(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "rgb" => Self::Rgb,
            "bgr" => Self::Bgr,
            "vrgb" => Self::VRgb,
            "vbgr" => Self::VBgr,
            "auto" => Self::Auto,
            _ => Self::Rgb,
        }
    }

    /// Convert to FreeType LcdMode
    pub fn to_lcd_mode(self) -> LcdMode {
        match self {
            Self::Rgb | Self::Bgr => LcdMode::LcdHorizontal,
            Self::VRgb | Self::VBgr => LcdMode::LcdVertical,
            Self::Auto => LcdMode::LcdHorizontal, // default
        }
    }

    /// Check if BGR type (needs R/B swap in shader)
    pub fn is_bgr(self) -> bool {
        matches!(self, Self::Bgr | Self::VBgr)
    }

    /// Auto-detect from rotation angle
    pub fn from_rotation(rotation: u32, base_bgr: bool) -> Self {
        // rotation: 0, 90, 180, 270
        match (rotation, base_bgr) {
            (0, false) => Self::Rgb,
            (0, true) => Self::Bgr,
            (180, false) => Self::Bgr,
            (180, true) => Self::Rgb,
            (90, false) => Self::VRgb,
            (90, true) => Self::VBgr,
            (270, false) => Self::VBgr,
            (270, true) => Self::VRgb,
            _ => Self::Rgb,
        }
    }
}

/// FreeType rasterization result
#[allow(dead_code)]
pub struct FtGlyph {
    /// Bitmap data (grayscale: 1byte/pixel, LCD: 3bytes/pixel)
    pub bitmap: Vec<u8>,
    /// Bitmap width (pixels)
    pub width: u32,
    /// Bitmap height (pixels)
    pub height: u32,
    /// Horizontal offset from baseline
    pub bearing_x: i32,
    /// Vertical offset from baseline
    pub bearing_y: i32,
    /// Horizontal advance to next character (26.6 fixed point -> pixels)
    pub advance: f32,
}

/// Options for styled (bold / italic) rasterization.
#[derive(Debug, Clone, Copy, Default)]
pub struct StyleOptions {
    /// Apply synthetic bold (FT_GlyphSlot_Embolden)
    pub bold: bool,
    /// Apply synthetic italic (shear transform)
    pub italic: bool,
    /// Shear amount for synthetic italics (tan of the slant angle)
    pub shear: f32,
    /// Render with grayscale AA instead of LCD subpixel: sheared stems land on
    /// fractional pixel positions and LCD filtering turns them into coloured
    /// "ghost" strokes.
    pub grayscale: bool,
}

/// FreeType font
#[allow(dead_code)]
pub struct FtFont {
    library: Arc<Library>,
    face: freetype::Face,
    /// Current font size (pixels)
    size_px: u32,
    /// LCD rendering mode
    lcd_mode: LcdMode,
    /// Hinting mode
    hinting_mode: HintingMode,
}

#[allow(dead_code)]
impl FtFont {
    /// Load from font data
    pub fn from_bytes(
        data: &[u8],
        size_px: u32,
        lcd_mode: LcdMode,
        lcd_filter: LcdFilterMode,
        lcd_weights: Option<[u8; 5]>,
        hinting_mode: HintingMode,
    ) -> Result<Self> {
        let library =
            Library::init().map_err(|e| anyhow!("FreeType initialization failed: {:?}", e))?;

        // Enable filter for LCD mode
        if lcd_mode != LcdMode::Grayscale {
            if lcd_filter == LcdFilterMode::Custom {
                // Use custom weights
                if let Some(weights) = lcd_weights {
                    unsafe {
                        FT_Library_SetLcdFilterWeights(library.raw(), weights.as_ptr());
                    }
                    info!("FreeType LCD filter: Custom {:?}", weights);
                } else {
                    // Fallback to Light if no weights
                    let _ = library.set_lcd_filter(LcdFilter::LcdFilterLight);
                    info!("FreeType LCD filter: Light (no custom weights)");
                }
            } else if let Some(ft_filter) = lcd_filter.to_freetype() {
                let _ = library.set_lcd_filter(ft_filter);
                info!("FreeType LCD filter: {:?}", lcd_filter);
            }
        }

        // freetype-rs requires Rc<Vec<u8>>
        let font_data: Rc<Vec<u8>> = Rc::new(data.to_vec());

        let face = library
            .new_memory_face(font_data, 0)
            .map_err(|e| anyhow!("FreeType font loading failed: {:?}", e))?;

        // Set pixel size
        face.set_pixel_sizes(0, size_px)
            .map_err(|e| anyhow!("FreeType size setting failed: {:?}", e))?;

        let family = face.family_name().unwrap_or_else(|| "unknown".to_string());

        info!(
            "FreeType font loaded: {} ({}px, {:?}, hinting={:?})",
            family, size_px, lcd_mode, hinting_mode
        );

        Ok(Self {
            library: Arc::new(library),
            face,
            size_px,
            lcd_mode,
            hinting_mode,
        })
    }

    /// Change font size
    pub fn set_size(&mut self, size_px: u32) -> Result<()> {
        self.face
            .set_pixel_sizes(0, size_px)
            .map_err(|e| anyhow!("FreeType size setting failed: {:?}", e))?;
        self.size_px = size_px;
        Ok(())
    }

    /// Rasterize character
    pub fn rasterize(&self, ch: char) -> Option<FtGlyph> {
        // Check if glyph exists in font
        // get_char_index returns 0 if not found
        let glyph_index = self.face.get_char_index(ch as usize);
        if glyph_index.is_none() || glyph_index == Some(0) {
            return None;
        }

        // Load glyph (apply hinting mode)
        let load_flags = LoadFlag::DEFAULT | self.hinting_mode.to_load_flag();

        if self.face.load_char(ch as usize, load_flags).is_err() {
            return None;
        }

        let glyph = self.face.glyph();

        // Select rendering mode
        let render_mode = match self.lcd_mode {
            LcdMode::Grayscale => RenderMode::Normal,
            LcdMode::LcdHorizontal => RenderMode::Lcd,
            LcdMode::LcdVertical => RenderMode::LcdV,
        };

        if glyph.render_glyph(render_mode).is_err() {
            return None;
        }

        let bitmap = glyph.bitmap();
        let metrics = glyph.metrics();

        // Bitmap width (3x wider in LCD mode)
        let raw_width = bitmap.width() as u32;
        let raw_height = bitmap.rows() as u32;

        // Divide width by 3 in LCD mode
        let width = match self.lcd_mode {
            LcdMode::Grayscale => raw_width,
            LcdMode::LcdHorizontal => raw_width / 3,
            LcdMode::LcdVertical => raw_width,
        };

        let height = match self.lcd_mode {
            LcdMode::Grayscale => raw_height,
            LcdMode::LcdHorizontal => raw_height,
            LcdMode::LcdVertical => raw_height / 3,
        };

        if width == 0 || height == 0 {
            // Empty glyph (e.g., space)
            return Some(FtGlyph {
                bitmap: vec![],
                width: 0,
                height: 0,
                bearing_x: (metrics.horiBearingX >> 6) as i32,
                bearing_y: (metrics.horiBearingY >> 6) as i32,
                advance: (metrics.horiAdvance >> 6) as f32,
            });
        }

        // Sanity check: glyph dimensions should be reasonable (< 4096 pixels)
        // This prevents integer overflow and OOM from malformed fonts
        const MAX_GLYPH_DIMENSION: u32 = 4096;
        if width > MAX_GLYPH_DIMENSION || height > MAX_GLYPH_DIMENSION {
            log::warn!("FreeType: glyph too large ({}x{}), skipping", width, height);
            return None;
        }

        // Copy bitmap data
        let buffer = bitmap.buffer();
        let pitch = bitmap.pitch().unsigned_abs() as usize;

        let data = match self.lcd_mode {
            LcdMode::Grayscale => {
                // R8 format
                let mut data = Vec::with_capacity((width as usize) * (height as usize));
                for y in 0..height as usize {
                    for x in 0..width as usize {
                        data.push(buffer[y * pitch + x]);
                    }
                }
                data
            }
            LcdMode::LcdHorizontal => {
                // RGB format (1 pixel = 3 bytes)
                let mut data = Vec::with_capacity((width as usize) * (height as usize) * 3);
                for y in 0..height as usize {
                    for x in 0..width as usize {
                        let idx = y * pitch + x * 3;
                        data.push(buffer[idx]); // R
                        data.push(buffer[idx + 1]); // G
                        data.push(buffer[idx + 2]); // B
                    }
                }
                data
            }
            LcdMode::LcdVertical => {
                // Vertical subpixel (rarely used)
                let mut data = Vec::with_capacity((width as usize) * (height as usize) * 3);
                for y in 0..height as usize {
                    for x in 0..width as usize {
                        let y3 = y * 3;
                        data.push(buffer[y3 * pitch + x]); // R
                        data.push(buffer[(y3 + 1) * pitch + x]); // G
                        data.push(buffer[(y3 + 2) * pitch + x]); // B
                    }
                }
                data
            }
        };

        Some(FtGlyph {
            bitmap: data,
            width,
            height,
            bearing_x: (metrics.horiBearingX >> 6) as i32,
            bearing_y: (metrics.horiBearingY >> 6) as i32,
            advance: (metrics.horiAdvance >> 6) as f32,
        })
    }

    /// Rasterize with subpixel phase
    /// phase: 0, 1/3, 2/3 pixel horizontal offset
    pub fn rasterize_with_phase(&mut self, ch: char, phase: SubpixelPhase) -> Option<FtGlyph> {
        let glyph_index = self.face.get_char_index(ch as usize);
        if glyph_index.is_none() || glyph_index == Some(0) {
            return None;
        }

        // Apply subpixel offset
        let delta = freetype::ffi::FT_Vector {
            x: phase.fixed_offset(),
            y: 0,
        };
        unsafe {
            FT_Set_Transform(self.face.raw_mut() as *mut _, std::ptr::null(), &delta);
        }

        // Load glyph (apply hinting mode)
        let load_flags = LoadFlag::DEFAULT | self.hinting_mode.to_load_flag();
        if self.face.load_char(ch as usize, load_flags).is_err() {
            // Reset transform
            unsafe {
                FT_Set_Transform(
                    self.face.raw_mut() as *mut _,
                    std::ptr::null(),
                    std::ptr::null(),
                );
            }
            return None;
        }

        let glyph = self.face.glyph();

        let render_mode = match self.lcd_mode {
            LcdMode::Grayscale => RenderMode::Normal,
            LcdMode::LcdHorizontal => RenderMode::Lcd,
            LcdMode::LcdVertical => RenderMode::LcdV,
        };

        if glyph.render_glyph(render_mode).is_err() {
            unsafe {
                FT_Set_Transform(
                    self.face.raw_mut() as *mut _,
                    std::ptr::null(),
                    std::ptr::null(),
                );
            }
            return None;
        }

        let bitmap = glyph.bitmap();
        let metrics = glyph.metrics();

        let raw_width = bitmap.width() as u32;
        let raw_height = bitmap.rows() as u32;

        let width = match self.lcd_mode {
            LcdMode::Grayscale => raw_width,
            LcdMode::LcdHorizontal => raw_width / 3,
            LcdMode::LcdVertical => raw_width,
        };

        let height = match self.lcd_mode {
            LcdMode::Grayscale => raw_height,
            LcdMode::LcdHorizontal => raw_height,
            LcdMode::LcdVertical => raw_height / 3,
        };

        // Reset transform
        unsafe {
            FT_Set_Transform(
                self.face.raw_mut() as *mut _,
                std::ptr::null(),
                std::ptr::null(),
            );
        }

        if width == 0 || height == 0 {
            return Some(FtGlyph {
                bitmap: vec![],
                width: 0,
                height: 0,
                bearing_x: (metrics.horiBearingX >> 6) as i32,
                bearing_y: (metrics.horiBearingY >> 6) as i32,
                advance: (metrics.horiAdvance >> 6) as f32,
            });
        }

        let buffer = bitmap.buffer();
        let pitch = bitmap.pitch().unsigned_abs() as usize;

        let data = match self.lcd_mode {
            LcdMode::Grayscale => {
                let mut data = Vec::with_capacity((width * height) as usize);
                for y in 0..height as usize {
                    for x in 0..width as usize {
                        data.push(buffer[y * pitch + x]);
                    }
                }
                data
            }
            LcdMode::LcdHorizontal => {
                let mut data = Vec::with_capacity((width * height * 3) as usize);
                for y in 0..height as usize {
                    for x in 0..width as usize {
                        let idx = y * pitch + x * 3;
                        data.push(buffer[idx]);
                        data.push(buffer[idx + 1]);
                        data.push(buffer[idx + 2]);
                    }
                }
                data
            }
            LcdMode::LcdVertical => {
                let mut data = Vec::with_capacity((width * height * 3) as usize);
                for y in 0..height as usize {
                    for x in 0..width as usize {
                        let y3 = y * 3;
                        data.push(buffer[y3 * pitch + x]);
                        data.push(buffer[(y3 + 1) * pitch + x]);
                        data.push(buffer[(y3 + 2) * pitch + x]);
                    }
                }
                data
            }
        };

        Some(FtGlyph {
            bitmap: data,
            width,
            height,
            bearing_x: (metrics.horiBearingX >> 6) as i32,
            bearing_y: (metrics.horiBearingY >> 6) as i32,
            advance: (metrics.horiAdvance >> 6) as f32,
        })
    }

    /// Rasterize directly from glyph ID
    pub fn rasterize_glyph_id(&self, glyph_id: u32) -> Option<FtGlyph> {
        let load_flags = LoadFlag::DEFAULT | LoadFlag::TARGET_LCD;

        if self.face.load_glyph(glyph_id, load_flags).is_err() {
            return None;
        }

        let glyph = self.face.glyph();

        let render_mode = match self.lcd_mode {
            LcdMode::Grayscale => RenderMode::Normal,
            LcdMode::LcdHorizontal => RenderMode::Lcd,
            LcdMode::LcdVertical => RenderMode::LcdV,
        };

        if glyph.render_glyph(render_mode).is_err() {
            return None;
        }

        let bitmap = glyph.bitmap();
        let metrics = glyph.metrics();

        let raw_width = bitmap.width() as u32;
        let raw_height = bitmap.rows() as u32;

        let width = match self.lcd_mode {
            LcdMode::Grayscale => raw_width,
            LcdMode::LcdHorizontal => raw_width / 3,
            LcdMode::LcdVertical => raw_width,
        };

        let height = match self.lcd_mode {
            LcdMode::Grayscale => raw_height,
            LcdMode::LcdHorizontal => raw_height,
            LcdMode::LcdVertical => raw_height / 3,
        };

        if width == 0 || height == 0 {
            return Some(FtGlyph {
                bitmap: vec![],
                width: 0,
                height: 0,
                bearing_x: (metrics.horiBearingX >> 6) as i32,
                bearing_y: (metrics.horiBearingY >> 6) as i32,
                advance: (metrics.horiAdvance >> 6) as f32,
            });
        }

        let buffer = bitmap.buffer();
        let pitch = bitmap.pitch().unsigned_abs() as usize;

        let data = match self.lcd_mode {
            LcdMode::Grayscale => {
                let mut data = Vec::with_capacity((width * height) as usize);
                for y in 0..height as usize {
                    for x in 0..width as usize {
                        data.push(buffer[y * pitch + x]);
                    }
                }
                data
            }
            LcdMode::LcdHorizontal => {
                let mut data = Vec::with_capacity((width * height * 3) as usize);
                for y in 0..height as usize {
                    for x in 0..width as usize {
                        let idx = y * pitch + x * 3;
                        data.push(buffer[idx]);
                        data.push(buffer[idx + 1]);
                        data.push(buffer[idx + 2]);
                    }
                }
                data
            }
            LcdMode::LcdVertical => {
                let mut data = Vec::with_capacity((width * height * 3) as usize);
                for y in 0..height as usize {
                    for x in 0..width as usize {
                        let y3 = y * 3;
                        data.push(buffer[y3 * pitch + x]);
                        data.push(buffer[(y3 + 1) * pitch + x]);
                        data.push(buffer[(y3 + 2) * pitch + x]);
                    }
                }
                data
            }
        };

        Some(FtGlyph {
            bitmap: data,
            width,
            height,
            bearing_x: (metrics.horiBearingX >> 6) as i32,
            bearing_y: (metrics.horiBearingY >> 6) as i32,
            advance: (metrics.horiAdvance >> 6) as f32,
        })
    }

    /// Rasterize with synthetic bold (FT_GlyphSlot_Embolden)
    pub fn rasterize_bold(&self, ch: char) -> Option<FtGlyph> {
        self.rasterize_styled(
            ch,
            StyleOptions {
                bold: true,
                ..Default::default()
            },
        )
    }

    /// Rasterize bold with subpixel phase
    pub fn rasterize_bold_with_phase(&mut self, ch: char, phase: SubpixelPhase) -> Option<FtGlyph> {
        self.rasterize_styled_with_phase(
            ch,
            phase,
            StyleOptions {
                bold: true,
                ..Default::default()
            },
        )
    }

    /// Internal: rasterize with optional bold/italic transforms
    /// Note: uses same &self as rasterize()/load_char() — FreeType's C API mutates face
    /// state through const-appearing pointers (same pattern as freetype-rs's load_char)
    pub fn rasterize_styled(&self, ch: char, opts: StyleOptions) -> Option<FtGlyph> {
        let glyph_index = self.face.get_char_index(ch as usize);
        if glyph_index.is_none() || glyph_index == Some(0) {
            return None;
        }

        // Get face pointer for FFI calls (const-to-mut cast matches freetype-rs pattern)
        let face_ptr =
            self.face.raw() as *const freetype::ffi::FT_FaceRec as *mut freetype::ffi::FT_FaceRec;

        // Apply italic shear matrix if needed (must be set before load_char).
        // The shear pivots at the baseline origin, so the top of the glyph
        // leans right; shift it left by half of that overhang so the slanted
        // glyph stays inside its cell.
        if opts.italic && opts.shear > 0.0 {
            let matrix = freetype::ffi::FT_Matrix {
                xx: 0x10000,
                xy: (opts.shear * 65536.0) as freetype::ffi::FT_Fixed,
                yx: 0,
                yy: 0x10000,
            };
            let ascent = self
                .face
                .size_metrics()
                .map(|m| (m.ascender >> 6) as f32)
                .unwrap_or(0.0);
            let shift = -(opts.shear * ascent * 0.5 * 64.0) as freetype::ffi::FT_Pos;
            let delta = freetype::ffi::FT_Vector { x: shift, y: 0 };
            unsafe {
                FT_Set_Transform(face_ptr, &matrix, &delta);
            }
        }

        let load_flags = LoadFlag::DEFAULT | self.hinting_mode.to_load_flag();
        if self.face.load_char(ch as usize, load_flags).is_err() {
            if opts.italic {
                unsafe {
                    FT_Set_Transform(face_ptr, std::ptr::null(), std::ptr::null());
                }
            }
            return None;
        }

        // Apply embolden after loading but before rendering
        if opts.bold {
            unsafe {
                let glyph_slot = (*face_ptr).glyph;
                FT_GlyphSlot_Embolden(glyph_slot);
            }
        }

        let glyph = self.face.glyph();
        // Synthetic italics deliberately use grayscale AA: sheared stems land
        // on fractional pixel positions and LCD filtering would turn them into
        // coloured ghost strokes.
        let render_mode = if opts.grayscale {
            RenderMode::Normal
        } else {
            match self.lcd_mode {
                LcdMode::Grayscale => RenderMode::Normal,
                LcdMode::LcdHorizontal => RenderMode::Lcd,
                LcdMode::LcdVertical => RenderMode::LcdV,
            }
        };

        if glyph.render_glyph(render_mode).is_err() {
            if opts.italic {
                unsafe {
                    FT_Set_Transform(face_ptr, std::ptr::null(), std::ptr::null());
                }
            }
            return None;
        }

        let result = if opts.grayscale && !matches!(self.lcd_mode, LcdMode::Grayscale) {
            self.extract_gray_as_rgb(&glyph)
        } else {
            self.extract_glyph_data(&glyph)
        };

        // Reset transform
        if opts.italic {
            unsafe {
                FT_Set_Transform(face_ptr, std::ptr::null(), std::ptr::null());
            }
        }

        result
    }

    /// Internal: rasterize with phase + optional bold/italic
    pub fn rasterize_styled_with_phase(
        &mut self,
        ch: char,
        phase: SubpixelPhase,
        opts: StyleOptions,
    ) -> Option<FtGlyph> {
        let glyph_index = self.face.get_char_index(ch as usize);
        if glyph_index.is_none() || glyph_index == Some(0) {
            return None;
        }

        // Build transform combining phase offset and optional italic shear
        let delta = freetype::ffi::FT_Vector {
            x: phase.fixed_offset(),
            y: 0,
        };

        if opts.italic && opts.shear > 0.0 {
            let matrix = freetype::ffi::FT_Matrix {
                xx: 0x10000,
                xy: (opts.shear * 65536.0) as freetype::ffi::FT_Fixed,
                yx: 0,
                yy: 0x10000,
            };
            let ascent = self
                .face
                .size_metrics()
                .map(|m| (m.ascender >> 6) as f32)
                .unwrap_or(0.0);
            let shear_shift = -(opts.shear * ascent * 0.5 * 64.0) as freetype::ffi::FT_Pos;
            let delta = freetype::ffi::FT_Vector {
                x: delta.x + shear_shift,
                y: 0,
            };
            unsafe {
                FT_Set_Transform(self.face.raw_mut() as *mut _, &matrix, &delta);
            }
        } else {
            unsafe {
                FT_Set_Transform(self.face.raw_mut() as *mut _, std::ptr::null(), &delta);
            }
        }

        let load_flags = LoadFlag::DEFAULT | self.hinting_mode.to_load_flag();
        if self.face.load_char(ch as usize, load_flags).is_err() {
            unsafe {
                FT_Set_Transform(
                    self.face.raw_mut() as *mut _,
                    std::ptr::null(),
                    std::ptr::null(),
                );
            }
            return None;
        }

        if opts.bold {
            unsafe {
                FT_GlyphSlot_Embolden((*self.face.raw()).glyph);
            }
        }

        let glyph = self.face.glyph();
        // See rasterize_styled: synthetic italics use grayscale AA.
        let render_mode = if opts.grayscale {
            RenderMode::Normal
        } else {
            match self.lcd_mode {
                LcdMode::Grayscale => RenderMode::Normal,
                LcdMode::LcdHorizontal => RenderMode::Lcd,
                LcdMode::LcdVertical => RenderMode::LcdV,
            }
        };

        if glyph.render_glyph(render_mode).is_err() {
            unsafe {
                FT_Set_Transform(
                    self.face.raw_mut() as *mut _,
                    std::ptr::null(),
                    std::ptr::null(),
                );
            }
            return None;
        }

        let result = if opts.grayscale && !matches!(self.lcd_mode, LcdMode::Grayscale) {
            self.extract_gray_as_rgb(&glyph)
        } else {
            self.extract_glyph_data(&glyph)
        };

        // Reset transform
        unsafe {
            FT_Set_Transform(
                self.face.raw_mut() as *mut _,
                std::ptr::null(),
                std::ptr::null(),
            );
        }

        result
    }

    /// Extract bitmap data from a rendered glyph slot
    fn extract_glyph_data(&self, glyph: &freetype::GlyphSlot) -> Option<FtGlyph> {
        let bitmap = glyph.bitmap();
        let metrics = glyph.metrics();

        let raw_width = bitmap.width() as u32;
        let raw_height = bitmap.rows() as u32;

        let width = match self.lcd_mode {
            LcdMode::Grayscale => raw_width,
            LcdMode::LcdHorizontal => raw_width / 3,
            LcdMode::LcdVertical => raw_width,
        };
        let height = match self.lcd_mode {
            LcdMode::Grayscale => raw_height,
            LcdMode::LcdHorizontal => raw_height,
            LcdMode::LcdVertical => raw_height / 3,
        };

        if width == 0 || height == 0 {
            return Some(FtGlyph {
                bitmap: vec![],
                width: 0,
                height: 0,
                bearing_x: (metrics.horiBearingX >> 6) as i32,
                bearing_y: (metrics.horiBearingY >> 6) as i32,
                advance: (metrics.horiAdvance >> 6) as f32,
            });
        }

        const MAX_GLYPH_DIMENSION: u32 = 4096;
        if width > MAX_GLYPH_DIMENSION || height > MAX_GLYPH_DIMENSION {
            return None;
        }

        let buffer = bitmap.buffer();
        let pitch = bitmap.pitch().unsigned_abs() as usize;

        let data = match self.lcd_mode {
            LcdMode::Grayscale => {
                let mut data = Vec::with_capacity((width * height) as usize);
                for y in 0..height as usize {
                    for x in 0..width as usize {
                        data.push(buffer[y * pitch + x]);
                    }
                }
                data
            }
            LcdMode::LcdHorizontal => {
                let mut data = Vec::with_capacity((width * height * 3) as usize);
                for y in 0..height as usize {
                    for x in 0..width as usize {
                        let idx = y * pitch + x * 3;
                        data.push(buffer[idx]);
                        data.push(buffer[idx + 1]);
                        data.push(buffer[idx + 2]);
                    }
                }
                data
            }
            LcdMode::LcdVertical => {
                let mut data = Vec::with_capacity((width * height * 3) as usize);
                for y in 0..height as usize {
                    for x in 0..width as usize {
                        let y3 = y * 3;
                        data.push(buffer[y3 * pitch + x]);
                        data.push(buffer[(y3 + 1) * pitch + x]);
                        data.push(buffer[(y3 + 2) * pitch + x]);
                    }
                }
                data
            }
        };

        Some(FtGlyph {
            bitmap: data,
            width,
            height,
            bearing_x: (metrics.horiBearingX >> 6) as i32,
            bearing_y: (metrics.horiBearingY >> 6) as i32,
            advance: (metrics.horiAdvance >> 6) as f32,
        })
    }

    /// Extract a grayscale (1 byte/pixel) bitmap into the 3-byte LCD atlas
    /// layout by replicating coverage into all channels. The shader then
    /// produces neutral (grayscale) antialiasing — used for synthetic italics.
    fn extract_gray_as_rgb(&self, glyph: &freetype::GlyphSlot) -> Option<FtGlyph> {
        let bitmap = glyph.bitmap();
        let metrics = glyph.metrics();
        let width = bitmap.width() as u32;
        let height = bitmap.rows() as u32;

        if width == 0 || height == 0 {
            return Some(FtGlyph {
                bitmap: vec![],
                width: 0,
                height: 0,
                bearing_x: (metrics.horiBearingX >> 6) as i32,
                bearing_y: (metrics.horiBearingY >> 6) as i32,
                advance: (metrics.horiAdvance >> 6) as f32,
            });
        }

        let buffer = bitmap.buffer();
        let pitch = bitmap.pitch().unsigned_abs() as usize;
        let mut data = Vec::with_capacity((width * height * 3) as usize);
        for y in 0..height as usize {
            for x in 0..width as usize {
                let v = buffer[y * pitch + x];
                data.push(v);
                data.push(v);
                data.push(v);
            }
        }

        Some(FtGlyph {
            bitmap: data,
            width,
            height,
            bearing_x: (metrics.horiBearingX >> 6) as i32,
            bearing_y: (metrics.horiBearingY >> 6) as i32,
            advance: (metrics.horiAdvance >> 6) as f32,
        })
    }

    /// Get line metrics
    pub fn line_metrics(&self) -> (f32, f32, f32) {
        let size_metrics = self
            .face
            .size_metrics()
            .expect("FreeType size not set - call set_size() first");
        let ascender = (size_metrics.ascender >> 6) as f32;
        let descender = (size_metrics.descender >> 6) as f32;
        let height = (size_metrics.height >> 6) as f32;
        (ascender, descender, height)
    }

    /// Get glyph ID for character
    pub fn get_glyph_index(&self, ch: char) -> Option<u32> {
        self.face.get_char_index(ch as usize)
    }

    /// Get current size
    pub fn size(&self) -> u32 {
        self.size_px
    }

    /// Get LCD mode
    pub fn lcd_mode(&self) -> LcdMode {
        self.lcd_mode
    }
}
