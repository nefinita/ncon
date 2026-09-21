//! Text drawing renderer
//!
//! Combine glyph atlas and shader
//! to render text on GPU

#![allow(dead_code)]

use anyhow::{anyhow, Result};
use glow::HasContext;
use log::info;

use super::bytemuck_cast_slice;

// === Curly underline renderer ===

use crate::gpu::shader::{self, CurlyShader};

/// Curly underline renderer (anti-aliasing with SDF + smoothstep)
///
/// Per-vertex: pos(2) + rect(4) + color(4) + params(4) = 14 floats
const CURLY_VERTEX_FLOATS: usize = 14;
const CURLY_MAX_RUNS: usize = 1024;

pub struct CurlyRenderer {
    shader: CurlyShader,
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    ebo: glow::Buffer,
    vertices: Vec<f32>,
    run_count: usize,
}

impl CurlyRenderer {
    pub fn new(gl: &glow::Context) -> Result<Self> {
        let shader = CurlyShader::new(gl)?;

        unsafe {
            let vao = gl
                .create_vertex_array()
                .map_err(|e| anyhow!("Failed to create VAO (Curly): {}", e))?;
            gl.bind_vertex_array(Some(vao));

            let vbo = gl
                .create_buffer()
                .map_err(|e| anyhow!("Failed to create VBO (Curly): {}", e))?;
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            let vbo_size = CURLY_MAX_RUNS * 4 * CURLY_VERTEX_FLOATS * 4;
            gl.buffer_data_size(glow::ARRAY_BUFFER, vbo_size as i32, glow::DYNAMIC_DRAW);

            let ebo = gl
                .create_buffer()
                .map_err(|e| anyhow!("Failed to create EBO (Curly): {}", e))?;
            gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(ebo));

            let mut indices: Vec<u16> = Vec::with_capacity(CURLY_MAX_RUNS * 6);
            for i in 0..CURLY_MAX_RUNS as u16 {
                let base = i * 4;
                indices.push(base);
                indices.push(base + 1);
                indices.push(base + 2);
                indices.push(base);
                indices.push(base + 2);
                indices.push(base + 3);
            }
            let index_bytes: &[u8] = bytemuck_cast_slice(&indices);
            gl.buffer_data_u8_slice(glow::ELEMENT_ARRAY_BUFFER, index_bytes, glow::STATIC_DRAW);

            let stride = (CURLY_VERTEX_FLOATS * 4) as i32;

            // a_pos: location=0, vec2
            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, stride, 0);

            // a_rect: location=1, vec4
            gl.enable_vertex_attrib_array(1);
            gl.vertex_attrib_pointer_f32(1, 4, glow::FLOAT, false, stride, 8);

            // a_color: location=2, vec4
            gl.enable_vertex_attrib_array(2);
            gl.vertex_attrib_pointer_f32(2, 4, glow::FLOAT, false, stride, 24);

            // a_params: location=3, vec4
            gl.enable_vertex_attrib_array(3);
            gl.vertex_attrib_pointer_f32(3, 4, glow::FLOAT, false, stride, 40);

            gl.bind_vertex_array(None);

            info!("Curly underline renderer initialized");

            Ok(Self {
                shader,
                vao,
                vbo,
                ebo,
                vertices: Vec::with_capacity(CURLY_MAX_RUNS * 4 * CURLY_VERTEX_FLOATS),
                run_count: 0,
            })
        }
    }

    pub fn begin(&mut self) {
        self.vertices.clear();
        self.run_count = 0;
    }

    /// Add curly underline
    ///
    /// # Arguments
    /// * `x` - Starting X coordinate
    /// * `y` - Starting Y coordinate (cell top)
    /// * `w` - Width
    /// * `h` - Height (cell height)
    /// * `color` - Color [r, g, b, a]
    /// * `amplitude` - Wave amplitude
    /// * `wavelength` - Wavelength
    /// * `thickness` - Line thickness
    /// * `base_y` - Base Y coordinate for wave
    pub fn push_curly(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        color: [f32; 4],
        amplitude: f32,
        wavelength: f32,
        thickness: f32,
        base_y: f32,
    ) {
        if self.run_count >= CURLY_MAX_RUNS {
            return;
        }

        let [r, g, b, a] = color;

        // 4 vertices in one extend (reduces function call overhead)
        // Each vertex: pos(2) + rect(4) + color(4) + params(4) = 14 floats
        #[rustfmt::skip]
        self.vertices.extend_from_slice(&[
            // top-left
            x, y,           x, y, w, h,  r, g, b, a,  amplitude, wavelength, thickness, base_y,
            // top-right
            x + w, y,       x, y, w, h,  r, g, b, a,  amplitude, wavelength, thickness, base_y,
            // bottom-right
            x + w, y + h,   x, y, w, h,  r, g, b, a,  amplitude, wavelength, thickness, base_y,
            // bottom-left
            x, y + h,       x, y, w, h,  r, g, b, a,  amplitude, wavelength, thickness, base_y,
        ]);

        self.run_count += 1;
    }

    pub fn flush(&self, gl: &glow::Context, width: u32, height: u32) {
        if self.run_count == 0 {
            return;
        }

        unsafe {
            gl.enable(glow::BLEND);
            gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);

            self.shader.bind(gl);

            let projection = shader::ortho_projection(width as f32, height as f32);
            self.shader.set_projection(gl, &projection);

            gl.bind_vertex_array(Some(self.vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));

            let vertex_bytes: &[u8] = bytemuck_cast_slice(&self.vertices);
            gl.buffer_sub_data_u8_slice(glow::ARRAY_BUFFER, 0, vertex_bytes);

            gl.draw_elements(
                glow::TRIANGLES,
                (self.run_count * 6) as i32,
                glow::UNSIGNED_SHORT,
                0,
            );

            gl.bind_vertex_array(None);
            gl.disable(glow::BLEND);
        }
    }

    pub fn destroy(&self, gl: &glow::Context) {
        unsafe {
            gl.delete_vertex_array(self.vao);
            gl.delete_buffer(self.vbo);
            gl.delete_buffer(self.ebo);
        }
        self.shader.destroy(gl);
    }
}
