use eframe::emath::Rect;
use glow::HasContext as _;
use std::ptr;

pub struct PlayerTexture {
    tex: glow::Texture,
    fbo: glow::Framebuffer,
    tex_id: Option<egui::TextureId>,
    tex_size: Rect,
}

impl PlayerTexture {
    pub fn new(size: Rect, painter: &egui_glow::Painter) -> Self {
        let gl = painter.gl().as_ref();
        // SAFETY: This is pretty bog-standard OpenGL
        unsafe {
            let tex = gl.create_texture().expect("failed to create fbo texture");
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGB as i32,
                size.width() as i32,
                size.height() as i32,
                0,
                glow::RGB,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(None),
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                glow::LINEAR as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAG_FILTER,
                glow::LINEAR as i32,
            );

            let fbo = gl.create_framebuffer().expect("failed to create fbo");
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(tex),
                0,
            );
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);

            Self {
                tex,
                fbo,
                tex_id: None,
                tex_size: size,
            }
        }
    }

    pub fn destroy(&self, painter: &egui_glow::Painter) {
        // SAFETY: manual cleanup of GL resources
        unsafe {
            let gl = painter.gl();
            gl.delete_framebuffer(self.fbo);
            gl.delete_texture(self.tex);
        }
    }

    pub unsafe fn mpv_render(&self, ctx: *mut libmpv_sys::mpv_render_context) {
        // SAFETY: We're following the documentation for how to call this API in render.h,
        //         as demonstrated by the SDL example program. We also depend on the correctness
        //         of the layouts in libmpv_sys.
        unsafe {
            let mut fbo_param = libmpv_sys::mpv_opengl_fbo {
                fbo: self.fbo.0.get().cast_signed(),
                w: self.tex_size.width() as i32,
                h: self.tex_size.height() as i32,
                internal_format: glow::RGB as i32,
            };
            let fbo_param_p: *mut libmpv_sys::mpv_opengl_fbo = &mut fbo_param;
            // Note: we need to flip for OpenGL, but this is done by egui_glow for us.
            let mut flip_y_param = 0i32;
            let flip_y_param_p: *mut i32 = &mut flip_y_param;
            let mut params_pack = [
                // Pass the FBO to draw to, already linked with our texture.
                libmpv_sys::mpv_render_param {
                    type_: libmpv_sys::mpv_render_param_type_MPV_RENDER_PARAM_OPENGL_FBO,
                    data: fbo_param_p.cast(),
                },
                libmpv_sys::mpv_render_param {
                    type_: libmpv_sys::mpv_render_param_type_MPV_RENDER_PARAM_FLIP_Y,
                    data: flip_y_param_p.cast(),
                },
                // Terminator
                libmpv_sys::mpv_render_param {
                    type_: libmpv_sys::mpv_render_param_type_MPV_RENDER_PARAM_INVALID,
                    data: ptr::null_mut(),
                },
            ];
            libmpv_sys::mpv_render_context_render(ctx, &mut params_pack[0]);
        }
    }

    pub fn set_tex_id(&mut self, tex_id: egui::TextureId) {
        self.tex_id = Some(tex_id);
    }

    pub fn tex_id(&self) -> Option<egui::TextureId> {
        self.tex_id
    }

    pub fn tex(&self) -> glow::Texture {
        self.tex
    }

    pub fn size(&self) -> Rect {
        self.tex_size
    }
}
