/// egui-mpv-glow
///
/// Uses libmpv (via ffmpeg) to do hardware decoding of media into OpenGL textures without
/// requiring the frames to go through the CPU or main memory. Those textures are then injected
/// into Egui via the `egui_glow` driver and provided to the ui each frame as a normal `egui::Image`
/// that can be drawn either via `ui.add` or `img.paint_at(ui, rect)`.
///
/// Usage with eframe:
/// ```rust
/// #[derive(Default)]
/// struct App {
///     player: egui_mpv_glow::MpvPlayer,
/// }
///
/// impl App {
///     pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
///         // Standard eframe setup code
///         let mut app: Self = Default::default();
///
///         // An init_* method variant _must_ be called at startup with the GL context
///         // to set up the MPV Render subsystem.
///         app.player.init_with_eframe(cc).unwrap();
///         app.player.play(&std::path::PathBuf::from("video.mp4"));
///
///         app
///     }
/// }
///
/// impl eframe::App for App {
///     fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
///         egui::CentralPanel::default().show(ctx, |ui| {
///             let window_rect = ui.available_rect_before_wrap();
///             let img = self.player.image(&window_rect, ui.painter(), frame).unwrap();
///             img.paint_at(ui, window_rect);
///
///             // paint_at doesn't allocate space, so we can now draw video controls on top of the video...
///         });
///     }
/// }
/// ```
mod advanced_client;
mod flags;
mod omt_client;
mod render;
mod shared;
mod texture;

pub use crate::advanced_client::MpvAdvancedClient;

use crate::{render::MpvRender, texture::PlayerTexture};
use anyhow::Result;
use egui::Rect;
use log::trace;
use parking_lot::Mutex;
use std::{path::Path, sync::Arc};

// Notes:
//   The glow opengl context is created and bound to the main (rendering) thread. Thus, all
//   of our render API calls have to be on the main thread. This means that all of our other
//   MPV usage has to be off the main thread, using async commands, and/or on the threading
//   exception list in render.h.

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MpvPlayerState {
    #[default]
    Uninitialized,
    Stopped,
    Playing,
    Paused,
}

#[derive(Default)]
pub struct MpvPlayer {
    // Mpv client and state
    state: MpvPlayerState,
    client: Option<MpvAdvancedClient>,
    render: Option<MpvRender>,

    // The glue between OpenGL, egui, and mpv
    tex: Arc<Mutex<Option<PlayerTexture>>>,
}

impl Drop for MpvPlayer {
    fn drop(&mut self) {
        // Note: we don't have a painter, so we must leak the texture and fbo ids.
        let mut tex = self.tex.lock();
        *tex = None;

        // Make sure that the renderer gets destroyed first
        self.render.take();
        self.client.take();
    }
}

impl MpvPlayer {
    pub fn init_with_eframe(&mut self, cc: &eframe::CreationContext<'_>) -> Result<()> {
        // Connect to the MPV client to control playback and set initial state.
        self.client = Some(MpvAdvancedClient::new(cc.egui_ctx.clone())?);
        self.state = MpvPlayerState::Stopped;

        let client_handle = self.client.as_ref().expect("not init").handle_ptr().addr();
        self.render = Some(MpvRender::new(
            client_handle as *mut libmpv_sys::mpv_handle,
            cc,
        ));

        Ok(())
    }

    pub fn image(
        &mut self,
        rect: &Rect,
        painter: &egui::Painter,
        frame: &mut eframe::Frame,
    ) -> Option<egui::Image<'_>> {
        // Clone locals so we can move them into the paint callback:
        let tex_ref = self.tex.clone();
        let rect = *rect;
        let render_ctx = self.render.as_ref().expect("not init").render_ptr().addr();
        // Note: this is called on the main thread later during the rendering bits when
        //       the right gl context has been made current and had its state prepared.
        let callback = Arc::new(egui_glow::CallbackFn::new(move |_info, painter| {
            // Note: we need the lock to make the data Send + Sync.
            let mut tex = tex_ref.lock();
            let mut rebuild = tex.is_none();
            if let Some(tex) = tex.as_ref()
                && tex.size() != rect
            {
                tex.destroy(painter);
                rebuild = true;
            }
            if rebuild {
                *tex = Some(PlayerTexture::new(rect, painter));
            }
            let tex = tex.as_ref().expect("opengl texture not initialized");

            // SAFETY: C doesn't have a means to move this pointer around once it is created, but
            //         the actual constraint on its usage with threads is purely documentation.
            unsafe {
                let ctx = render_ctx as *mut libmpv_sys::mpv_render_context;
                tex.mpv_render(ctx);
            }
        }));

        // Read from our events stream on the main thread and respond to MPV
        if self
            .client
            .as_mut()
            .expect("not initialized")
            .drain_events()
        {
            self.state = MpvPlayerState::Stopped;
        }

        // Redraw if requested by MPV
        if self
            .render
            .as_mut()
            .expect("not initialized")
            .drain_events()
        {
            painter.add(egui::PaintCallback { rect, callback });
        }

        // We may or may not have an image this frame, but get it and return it if we have it.
        let mut tex = self.tex.lock();
        tex.as_mut().map(|tex| {
            let tex_id = if let Some(tex_id) = tex.tex_id() {
                tex_id
            } else {
                let tex_id = frame.register_native_glow_texture(tex.tex());
                tex.set_tex_id(tex_id);
                tex_id
            };
            egui::Image::from_texture(egui::load::SizedTexture {
                id: tex_id,
                size: tex.size().max.to_vec2(),
            })
        })
    }

    // Panics if initialize has not yet been called.
    fn mpv_mut(&mut self) -> &mut MpvAdvancedClient {
        self.client.as_mut().expect("mpv not initialized")
    }

    /// Play a media file.
    /// Panics if initialize has not yet been called.
    pub fn play(&mut self, filename: &Path) {
        trace!("mpv playing file {filename:?}");
        self.mpv_mut()
            .playlist_clear_async()
            .expect("mpv disconnect");
        self.mpv_mut()
            .playlist_remove_current_async()
            .expect("mpv disconnect");
        self.mpv_mut()
            .playlist_load_files_async(&[(filename, libmpv::FileState::Replace, None)])
            .expect("mpv disconnect");
    }

    pub fn pause(&mut self) {
        self.mpv_mut().pause_async().expect("mpv disconnect");
    }

    pub fn unpause(&mut self) {
        self.mpv_mut().unpause_async().expect("mpv disconnect");
    }

    pub fn is_paused(&self) -> bool {
        self.client.as_ref().expect("mpv uninit").is_paused()
    }

    pub fn stop(&mut self) {
        self.mpv_mut()
            .playlist_clear_async()
            .expect("mpv disconnect");
        self.mpv_mut()
            .playlist_remove_current_async()
            .expect("mpv disconnect");
    }

    pub fn seek_forward(&mut self, delta_secs: f64) {
        self.mpv_mut()
            .seek_forward_async(delta_secs)
            .expect("mpv disconnect");
    }

    pub fn seek_backward(&mut self, delta_secs: f64) {
        self.mpv_mut()
            .seek_backward_async(delta_secs)
            .expect("mpv disconnect");
    }
}
