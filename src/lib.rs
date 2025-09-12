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
mod flags;

use crate::flags::RenderFrameInfoFlags;
use anyhow::Result;
use bitbag::BitBag;
use crossbeam::channel::{Receiver, Sender};
use egui::Rect;
use glow::HasContext as _;
use libmpv::{FileState, Mpv};
use log::trace;
use parking_lot::Mutex;
use std::{ffi, path::Path, ptr, sync::Arc};

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

struct GpaContext<'a> {
    pub get_proc_addr: &'a dyn Fn(&ffi::CStr) -> *const ffi::c_void,
}

// LibMPV Doc String:
//     This retrieves OpenGL function pointers, and will use them in subsequent
//     operation.
//     Usually, you can simply call the GL context APIs from this callback (e.g.
//     glXGetProcAddressARB or wglGetProcAddress), but some APIs do not always
//     return pointers for all standard functions (even if present); in this
//     case you have to compensate by looking up these functions yourself when
//     libmpv wants to resolve them through this callback.
//     libmpv will not normally attempt to resolve GL functions on its own, nor
//     does it link to GL libraries directly.
extern "C" fn art_get_proc_address_stub(
    ctx: *mut ::std::os::raw::c_void,
    name: *const ::std::os::raw::c_char,
) -> *mut ::std::os::raw::c_void {
    // SAFETY: We handed it a pointer to a stack ref and we know via inspection of mpv source
    //         that the get_proc_address function pointer we pass it will not outlive the call
    //         to mpv_render_context_create.
    let gpa_ctx = unsafe { &*ctx.cast::<GpaContext<'_>>() };

    // SAFETY: We trust that MPV is handing us sane, gl-relevant (i.e. ascii) character strings.
    let c_name = unsafe { ffi::CStr::from_ptr(name) };

    trace!(
        "art_get_proc_address_stub: {}",
        c_name.to_str().expect("invalid utf-8 in get_proc_address")
    );
    (gpa_ctx.get_proc_addr)(c_name).cast_mut()
}

#[derive(Clone, Copy, Debug)]
enum MpvEvent {
    CoreUpdate,
    RenderUpdate,
}

#[derive(Clone, Debug)]
struct CallbackContext {
    ctx: egui::Context,
    events: Sender<MpvEvent>,
}

extern "C" fn mpv_update_callback(cb_ctx: *mut ffi::c_void) {
    // SAFETY: we boxed the callback context so it wouldn't move after passing the address to MPV.
    let ctx = unsafe { &*cb_ctx.cast::<CallbackContext>() };
    ctx.events
        .send(MpvEvent::CoreUpdate)
        .expect("mpv events channel was closed");
    ctx.ctx.request_repaint();
}

// typedef void (*mpv_render_update_fn)(void *cb_ctx);
extern "C" fn mpv_render_update_callback(cb_ctx: *mut ffi::c_void) {
    // SAFETY: we boxed the callback context so it wouldn't move after passing the address to MPV.
    let ctx = unsafe { &*cb_ctx.cast::<CallbackContext>() };
    ctx.events
        .send(MpvEvent::RenderUpdate)
        .expect("mpv events channel was closed");
    ctx.ctx.request_repaint();
}

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
}

#[derive(Default)]
pub struct MpvPlayer {
    // Mpv client and state
    state: MpvPlayerState,
    mpv: Option<Mpv>,

    // Mpv render API context
    ctx: *mut libmpv_sys::mpv_render_context,

    // The glue between OpenGL, egui, and mpv
    tex: Arc<Mutex<Option<PlayerTexture>>>,
    events: Option<Receiver<MpvEvent>>,
}

impl Drop for MpvPlayer {
    fn drop(&mut self) {
        if !self.ctx.is_null() {
            // SAFETY: we just checked for null
            unsafe { libmpv_sys::mpv_render_context_free(self.ctx) };
            self.ctx = ptr::null_mut();
        }
        // FIXME: free the callback context boxes?
    }
}

impl MpvPlayer {
    pub fn init_with_eframe(&mut self, cc: &eframe::CreationContext<'_>) -> Result<()> {
        // Connect to the MPV client to control playback and set initial state.
        self.mpv = Some(
            Mpv::with_initializer(|init| {
                init.set_property("idle", true)?;
                init.set_property("vo", "libmpv")?;
                init.set_property("vd-lavc-dr", true)?;
                // builder.set_option("opengl-hwdec-interop", "auto")?;
                // builder.set_option("hwdec-preload", "auto")?;
                Ok(())
            })
            .map_err(|e| anyhow::anyhow!("mpv init error: {}", e))?,
        );
        self.state = MpvPlayerState::Stopped;

        // Create the machinery needed to receive events from MPV so that we can drive the
        // render loop successfully.
        let (send, recv) = crossbeam::channel::unbounded();
        let cb_ctx = CallbackContext {
            ctx: cc.egui_ctx.clone(),
            events: send,
        };
        let core_cb_ctx = Box::new(cb_ctx.clone());
        let render_cb_ctx = Box::new(cb_ctx);
        self.events = Some(recv);

        // Create the MPV renderer in "advanced" mode. This means we need to be very careful
        // with how we access the context, but means we won't deadlock accidentally, barring
        // legacy bugs that we shouldn't be shipping.
        let mut gpa_ctx = GpaContext {
            get_proc_addr: cc.get_proc_address.expect("opengl not available"),
        };
        let gpa_ctx_p: *mut GpaContext<'_> = &mut gpa_ctx;
        let api_type: *const ffi::c_char = libmpv_sys::MPV_RENDER_API_TYPE_OPENGL.as_ptr().cast();
        let api_type_p: *mut ffi::c_char = api_type.cast_mut();
        let mut ogl_params_pack = libmpv_sys::mpv_opengl_init_params {
            get_proc_address: Some(art_get_proc_address_stub),
            get_proc_address_ctx: gpa_ctx_p.cast(),
        };
        let ogl_params_p: *mut libmpv_sys::mpv_opengl_init_params = &mut ogl_params_pack;
        let mut advanced_ctrl = 1i32;
        let advanced_ctrl_p: *mut ffi::c_int = &mut advanced_ctrl;
        let mut params_pack = [
            libmpv_sys::mpv_render_param {
                type_: libmpv_sys::mpv_render_param_type_MPV_RENDER_PARAM_API_TYPE,
                data: api_type_p.cast(),
            },
            libmpv_sys::mpv_render_param {
                type_: libmpv_sys::mpv_render_param_type_MPV_RENDER_PARAM_OPENGL_INIT_PARAMS,
                data: ogl_params_p.cast(),
            },
            libmpv_sys::mpv_render_param {
                type_: libmpv_sys::mpv_render_param_type_MPV_RENDER_PARAM_ADVANCED_CONTROL,
                data: advanced_ctrl_p.cast(),
            },
            // Terminator
            libmpv_sys::mpv_render_param {
                type_: libmpv_sys::mpv_render_param_type_MPV_RENDER_PARAM_INVALID,
                data: ptr::null_mut(),
            },
        ];
        // SAFETY: we're dependent on the correctness of the layouts in libmpv_sys.
        unsafe {
            libmpv_sys::mpv_render_context_create(
                &mut self.ctx,
                self.mpv_mut().ctx.as_ptr(),
                &mut params_pack[0],
            );

            libmpv_sys::mpv_set_wakeup_callback(
                self.mpv.as_ref().expect("mpv not inited").ctx.as_ptr(),
                Some(mpv_update_callback),
                Box::into_raw(core_cb_ctx).cast(),
            );

            libmpv_sys::mpv_render_context_set_update_callback(
                self.ctx,
                Some(mpv_render_update_callback),
                Box::into_raw(render_cb_ctx).cast(),
            );
        }

        Ok(())
    }

    pub fn image(
        &self,
        rect: &Rect,
        painter: &egui::Painter,
        frame: &mut eframe::Frame,
    ) -> Option<egui::Image<'_>> {
        // Clone locals so we can move them into the paint callback:
        let tex_ref = self.tex.clone();
        let rect = *rect;
        let render_ctx = self.ctx.addr();
        // Note: this is called on the main thread later during the rendering bits when
        //       the right gl context has been made current and had its state prepared.
        let cb = egui_glow::CallbackFn::new(move |_info, painter| {
            // TODO: figure out how to avoid blocking here, if possible, especially since we're holding a lock.
            //       For that matter, do we even need this lock anymore, now that we know this callback
            //       runs on the main thread?
            let mut tex = tex_ref.lock();
            let mut rebuild = tex.is_none();
            if let Some(tex) = tex.as_ref()
                && tex.tex_size != rect
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
                Self::render_to_texture(ctx, tex);
            }
        });

        // Read from our events stream on the main thread and respond to MPV
        let mut redraw = false;
        while let Ok(event) = self.events.as_ref().expect("no events").try_recv() {
            match event {
                MpvEvent::RenderUpdate => {
                    // SAFETY: called from the main thread in response to a render update callback.
                    let flags = unsafe { libmpv_sys::mpv_render_context_update(self.ctx) };
                    let flags = BitBag::<RenderFrameInfoFlags>::new_checked(flags)
                        .expect("invalid Frame Info flags");
                    if flags.is_set(RenderFrameInfoFlags::Present) {
                        redraw = true;
                    }
                }
                MpvEvent::CoreUpdate => {
                    loop {
                        // SAFETY: we received a wakeup event from MPV, so we know there is an event available.
                        let event = unsafe {
                            libmpv_sys::mpv_wait_event(
                                self.mpv.as_ref().expect("mpv not init").ctx.as_ptr(),
                                0.0,
                            )
                        };
                        if event.is_null() {
                            break;
                        }
                        // SAFETY: just checked for null
                        let event = unsafe { *event };
                        match event.event_id {
                            libmpv_sys::mpv_event_id_MPV_EVENT_NONE => break,
                            libmpv_sys::mpv_event_id_MPV_EVENT_LOG_MESSAGE => {
                                let msg: *const libmpv_sys::mpv_event_log_message =
                                    event.data as *const _;
                                // SAFETY: the message is documented as being present
                                let msg = unsafe { &*msg };
                                let level = match msg.log_level {
                                    libmpv_sys::mpv_log_level_MPV_LOG_LEVEL_NONE
                                    | libmpv_sys::mpv_log_level_MPV_LOG_LEVEL_FATAL
                                    | libmpv_sys::mpv_log_level_MPV_LOG_LEVEL_ERROR => {
                                        log::Level::Error
                                    }
                                    libmpv_sys::mpv_log_level_MPV_LOG_LEVEL_WARN => {
                                        log::Level::Warn
                                    }
                                    libmpv_sys::mpv_log_level_MPV_LOG_LEVEL_INFO => {
                                        log::Level::Info
                                    }
                                    libmpv_sys::mpv_log_level_MPV_LOG_LEVEL_V
                                    | libmpv_sys::mpv_log_level_MPV_LOG_LEVEL_DEBUG => {
                                        log::Level::Debug
                                    }
                                    _ => log::Level::Trace,
                                };
                                // SAFETY: the message is documented as being present
                                let content =
                                    unsafe { ffi::CStr::from_ptr(msg.text) }.to_string_lossy();
                                for line in content.split('\n') {
                                    log::log!(level, "{line}");
                                }
                            }
                            libmpv_sys::mpv_event_id_MPV_EVENT_START_FILE => {
                                // SAFETY: per the docs, the only element in the struct passed here is the playlist offset.
                                let playlist = unsafe {
                                    *(event.data as *const libmpv_sys::mpv_event_start_file)
                                };
                                trace!(
                                    "mpv started playing file at playlist entry id: {}",
                                    playlist.playlist_entry_id
                                );
                            }
                            libmpv_sys::mpv_event_id_MPV_EVENT_IDLE => {}
                            libmpv_sys::mpv_event_id_MPV_EVENT_FILE_LOADED => {
                                trace!("mpv decoding started on current file");
                            }
                            libmpv_sys::mpv_event_id_MPV_EVENT_AUDIO_RECONFIG => {
                                trace!("mpv audio has been reconfigured");
                            }
                            libmpv_sys::mpv_event_id_MPV_EVENT_VIDEO_RECONFIG => {
                                // TODO: resize the texture here? Should we not be using the output size?
                                trace!("mpv video has been reconfigured");
                            }
                            libmpv_sys::mpv_event_id_MPV_EVENT_PLAYBACK_RESTART => {
                                // Playback position update or start is complete
                                trace!("mpv video seek finished");
                            }
                            _ => trace!("unrecognized mpv event: {event:#?}"),
                        }
                    }
                }
            }
        }

        // Rust correctly identifies that we might get multiple messages asking for redraw, even
        // if in practice that should never happen. Queue up the event out of line to prove that
        // we won't send it twice.
        if redraw {
            painter.add(egui::PaintCallback {
                rect,
                callback: Arc::new(cb),
            });
        }

        // We may or may not have an image this frame, but get it and return it if we have it.
        let mut tex = self.tex.lock();
        tex.as_mut().map(|tex| {
            let tex_id = if let Some(tex_id) = tex.tex_id {
                tex_id
            } else {
                let tex_id = frame.register_native_glow_texture(tex.tex);
                tex.tex_id = Some(tex_id);
                tex_id
            };
            egui::Image::from_texture(egui::load::SizedTexture {
                id: tex_id,
                size: tex.tex_size.max.to_vec2(),
            })
        })
    }

    unsafe fn render_to_texture(ctx: *mut libmpv_sys::mpv_render_context, tex: &PlayerTexture) {
        // SAFETY: We're following the documentation for how to call this API in render.h,
        //         as demonstrated by the SDL example program. We also depend on the correctness
        //         of the layouts in libmpv_sys.
        unsafe {
            let mut fbo_param = libmpv_sys::mpv_opengl_fbo {
                fbo: tex.fbo.0.get().cast_signed(),
                w: tex.tex_size.width() as i32,
                h: tex.tex_size.height() as i32,
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

    // Panics if initialize has not yet been called.
    fn mpv_mut(&mut self) -> &mut Mpv {
        self.mpv.as_mut().expect("mpv not initialized")
    }

    pub fn state(&self) -> MpvPlayerState {
        self.state
    }

    pub fn command_async(&self, name: &str, args: &[&str]) -> libmpv::Result<()> {
        let mut cmd = name.to_owned();

        for elem in args {
            cmd.push(' ');
            cmd.push_str(elem);
        }

        // let raw = CString::new(cmd)?;
        // unsafe {
        //     libmpv_sys::mpv_command_async(self.ctx.as_ptr(), raw.as_ptr())
        // }
        todo!()
    }

    /// Play a media file.
    /// Panics if initialize has not yet been called.
    pub fn play(&mut self, filename: &Path) {
        match self.state {
            MpvPlayerState::Uninitialized => panic!("mpv not initialized"),
            MpvPlayerState::Playing | MpvPlayerState::Paused => {
                // Note: our lifecycle wants the user to stop the existing playback before starting a new one.
                trace!("mpv skipping play because already playing or paused");
            }
            MpvPlayerState::Stopped => {
                trace!("mpv playing file {filename:?}");
                self.mpv_mut().playlist_clear().expect("mpv disconnect");
                self.mpv_mut()
                    .playlist_load_files(&[(&filename.to_string_lossy(), FileState::Replace, None)])
                    .expect("mpv disconnect");
                self.state = MpvPlayerState::Playing;
            }
        }
    }

    pub fn pause(&mut self) {
        match self.state {
            MpvPlayerState::Uninitialized => panic!("mpv not initialized"),
            MpvPlayerState::Paused | MpvPlayerState::Stopped => {}
            MpvPlayerState::Playing => {
                self.mpv_mut().pause().expect("mpv disconnect");
                self.state = MpvPlayerState::Paused;
            }
        }
    }

    pub fn resume(&mut self) {
        match self.state {
            MpvPlayerState::Uninitialized => panic!("mpv not initialized"),
            MpvPlayerState::Playing | MpvPlayerState::Stopped => {}
            MpvPlayerState::Paused => {
                self.mpv_mut().unpause().expect("mpv disconnect");
                self.state = MpvPlayerState::Playing;
            }
        }
    }

    pub fn stop(&mut self) {
        match self.state {
            MpvPlayerState::Uninitialized => panic!("mpv not initialized"),
            MpvPlayerState::Stopped => {}
            MpvPlayerState::Playing | MpvPlayerState::Paused => {
                self.mpv_mut().pause().expect("mpv disconnect");
                self.mpv_mut().playlist_clear().expect("mpv disconnect");
                self.state = MpvPlayerState::Stopped;
            }
        }
    }

    pub fn seek_forward(&mut self, delta_secs: f64) {
        match self.state {
            MpvPlayerState::Uninitialized => panic!("mpv not initialized"),
            MpvPlayerState::Stopped => {}
            MpvPlayerState::Playing | MpvPlayerState::Paused => {
                self.mpv_mut()
                    .seek_forward(delta_secs)
                    .expect("mpv disconnect");
            }
        }
    }

    pub fn seek_backward(&mut self, delta_secs: f64) {
        match self.state {
            MpvPlayerState::Uninitialized => panic!("mpv not initialized"),
            MpvPlayerState::Stopped => {}
            MpvPlayerState::Playing | MpvPlayerState::Paused => {
                self.mpv_mut()
                    .seek_backward(delta_secs)
                    .expect("mpv disconnect");
            }
        }
    }
}
