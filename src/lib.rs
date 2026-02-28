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
///         app.player.playlist_replace_async(&std::path::PathBuf::from("video.mp4"), None).ok();
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
pub use libmpv::FileState;

use crate::{render::MpvRender, texture::PlayerTexture};
use anyhow::Result;
use std::{path::Path, sync::Arc};

// Notes:
//   The glow opengl context is created and bound to the main (rendering) thread. Thus, all
//   of our render API calls have to be on the main thread. This means that all of our other
//   MPV usage has to be off the main thread, using async commands, and/or on the threading
//   exception list in render.h.

#[derive(Default)]
pub struct MpvPlayer {
    // Mpv client and state
    client: Option<MpvAdvancedClient>,
    render: Option<MpvRender>,

    // The glue between OpenGL, egui, and mpv
    tex: Arc<parking_lot::Mutex<Option<PlayerTexture>>>,
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
    /// While `MpvPlayer` is Default so that it can be used with `EFrame` easily, it won't
    /// actually be able to do anything until it is initialized with the GL context.
    pub fn init_with_eframe(&mut self, cc: &eframe::CreationContext<'_>) -> Result<()> {
        // Connect to the MPV client to control playback
        self.client = Some(MpvAdvancedClient::new(cc.egui_ctx.clone())?);

        let client_handle = self.client.as_ref().expect("not init").handle_ptr().addr();
        self.render = Some(MpvRender::new(
            client_handle as *mut libmpv_sys::mpv_handle,
            cc,
        ));

        Ok(())
    }

    /// This method will pump the event loop, redraw the texture with a new frame (if available)
    /// using the painter's GL context, then return the (potentially new from a resize) texture id.
    pub fn texture(
        &mut self,
        // TODO: figure out how to draw at the full video size
        rect: &egui::Rect,
        painter: &egui::Painter,
    ) -> Option<glow::Texture> {
        // Read from our events stream on the main thread and respond to MPV
        self.monitor_events();

        // Ask for a repaint to the texture
        self.queue_redraw(rect, painter);

        // The painter may or may not have updated our texture, but return it if we have it.
        let tex = self.tex.lock();
        tex.as_ref().map(|v| v.tex())
    }

    /// This method will pump the event loop, redraw the texture with a new frame (if available)
    /// using the painter's GL context, then wrap the texture id in an Image for us to use in
    /// whatever UX we're building.
    pub fn image(
        &mut self,
        rect: &egui::Rect,
        painter: &egui::Painter,
        frame: &mut eframe::Frame,
    ) -> Option<egui::Image<'_>> {
        // Read from our events stream on the main thread and respond to MPV
        self.monitor_events();

        // Ask for a repaint to the texture
        self.queue_redraw(rect, painter);

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

    // Keep mpv's connection alive and monitored, without syncing on the graphics engine. For use
    // if the video is temporarily not being shown.
    pub fn monitor_events(&mut self) {
        // Read from our events stream on the main thread and respond to MPV
        self.client
            .as_mut()
            .expect("not initialized")
            .drain_events();
    }

    pub fn queue_redraw(&mut self, rect: &egui::Rect, painter: &egui::Painter) {
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

        // Redraw if requested by MPV
        if self
            .render
            .as_mut()
            .expect("not initialized")
            .drain_events()
        {
            painter.add(egui::PaintCallback { rect, callback });
        }
    }

    /// Panics if initialize has not yet been called.
    fn mpv(&self) -> &MpvAdvancedClient {
        self.client.as_ref().expect("mpv not initialized")
    }

    /// Panics if initialize has not yet been called.
    fn mpv_mut(&mut self) -> &mut MpvAdvancedClient {
        self.client.as_mut().expect("mpv not initialized")
    }

    // --- Seek functions ---
    //

    /// Seek forward relatively from current position in seconds.
    /// This is less exact than `seek_absolute`, see [mpv manual]
    /// (<https://mpv.io/manual/master/#command-interface-[relative|absolute|absolute-percent|relative-percent|exact|keyframes]>).
    pub fn seek_forward_async(&mut self, secs: f64) -> Result<()> {
        self.mpv_mut().seek_forward_async(secs)
    }

    /// See `seek_forward`.
    pub fn seek_backward_async(&mut self, secs: f64) -> Result<()> {
        self.mpv_mut().seek_backward_async(secs)
    }

    /// Seek to a given absolute secs.
    pub fn seek_absolute_async(&mut self, secs: f64) -> Result<()> {
        self.mpv_mut().seek_absolute_async(secs)
    }

    /// Seek to a given relative percent position (may be negative).
    /// If `percent` of the playtime is bigger than the remaining playtime, the next file is played.
    /// out of bounds values are clamped to either 0 or 100.
    pub fn seek_percent_async(&mut self, percent: isize) -> Result<()> {
        self.mpv_mut().seek_percent_async(percent)
    }

    /// Seek to the given percentage of the playtime.
    pub fn seek_percent_absolute_async(&mut self, percent: usize) -> Result<()> {
        self.mpv_mut().seek_percent_absolute_async(percent)
    }

    /// Revert the previous `seek_` call, can also revert itself.
    pub fn seek_revert_async(&mut self) -> Result<()> {
        self.mpv_mut().seek_revert_async()
    }

    /// Mark the current position as the position that will be sought to by `seek_revert`.
    pub fn seek_revert_mark_async(&mut self) -> Result<()> {
        self.mpv_mut().seek_revert_mark_async()
    }

    /// Seek exactly one frame, and pause.
    /// Noop on audio only streams.
    pub fn seek_frame_async(&mut self) -> Result<()> {
        self.mpv_mut().seek_frame_async()
    }

    /// See `seek_frame`.
    /// [Note performance considerations.](https://mpv.io/manual/master/#command-interface-frame-back-step)
    pub fn seek_frame_backward_async(&mut self) -> Result<()> {
        self.mpv_mut().seek_frame_backward_async()
    }

    // --- Playlist functions ---
    //

    /// Play the next item of the current playlist.
    /// Does nothing if the current item is the last item.
    pub fn playlist_next_weak_async(&mut self) -> Result<()> {
        self.mpv_mut().playlist_next_weak_async()
    }

    /// Play the next item of the current playlist.
    /// Terminates playback if the current item is the last item.
    pub fn playlist_next_force_async(&mut self) -> Result<()> {
        self.mpv_mut().playlist_next_force_async()
    }

    /// See `playlist_next_weak`.
    pub fn playlist_previous_weak_async(&mut self) -> Result<()> {
        self.mpv_mut().playlist_previous_weak_async()
    }

    /// See `playlist_next_force`.
    pub fn playlist_previous_force_async(&mut self) -> Result<()> {
        self.mpv_mut().playlist_previous_force_async()
    }

    pub fn playlist_replace_async(&mut self, filename: &Path, extra: Option<&str>) -> Result<()> {
        self.mpv_mut()
            .playlist_load_files_async(&[(filename, FileState::Replace, extra)])
    }

    pub fn playlist_load_files_async(
        &mut self,
        files: &[(&Path, FileState, Option<&str>)],
    ) -> Result<()> {
        self.mpv_mut().playlist_load_files_async(files)
    }

    /// Remove every, except the current, item from the playlist.
    pub fn playlist_clear_async(&mut self) -> Result<()> {
        self.mpv_mut().playlist_clear_async()
    }

    /// Remove the currently selected item from the playlist.
    pub fn playlist_remove_current_async(&mut self) -> Result<()> {
        self.mpv_mut().playlist_remove_current_async()
    }

    /// Remove item at `position` from the playlist.
    pub fn playlist_remove_index_async(&mut self, position: usize) -> Result<()> {
        self.mpv_mut().playlist_remove_index_async(position)
    }

    /// Move item `old` to the position of item `new`.
    pub fn playlist_move_async(&mut self, old: usize, new: usize) -> Result<()> {
        self.mpv_mut().playlist_move_async(old, new)
    }

    /// Shuffle the playlist.
    pub fn playlist_shuffle_async(&mut self) -> Result<()> {
        self.mpv_mut().playlist_shuffle_async()
    }

    pub fn pause_async(&mut self) -> Result<()> {
        self.mpv_mut().pause_async()
    }

    pub fn unpause_async(&mut self) -> Result<()> {
        self.mpv_mut().unpause_async()
    }

    pub fn is_paused(&self) -> bool {
        self.mpv().is_paused()
    }

    pub fn percent_pos(&self) -> f64 {
        self.mpv().percent_pos()
    }

    pub fn time_pos(&self) -> f64 {
        self.mpv().time_pos()
    }

    pub fn time_remaining(&self) -> f64 {
        self.mpv().time_remaining()
    }

    pub fn duration(&self) -> f64 {
        self.mpv().duration()
    }

    pub fn width(&self) -> f64 {
        self.mpv().width()
    }

    pub fn height(&self) -> f64 {
        self.mpv().height()
    }

    pub fn rect(&self) -> egui::Rect {
        let w = self.width().max(1.0) as f32;
        let h = self.height().max(1.0) as f32;
        egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(w, h))
    }

    pub fn aspect_ratio(&self) -> f64 {
        let h = self.height();
        if h > 0.0 {
            self.width() / h
        } else {
            1.0
        }
    }
}
