use crate::{
    flags::RenderFrameInfoFlags,
    shared::{CallbackContext, MpvEvent},
};
use bitbag::BitBag;
use crossbeam::channel;
use log::trace;
use std::{ffi, ptr};

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

// typedef void (*mpv_render_update_fn)(void *cb_ctx);
extern "C" fn mpv_render_update_callback(cb_ctx: *mut ffi::c_void) {
    // SAFETY: we boxed the callback context so it wouldn't move after passing the address to MPV.
    let ctx = unsafe { &*cb_ctx.cast::<CallbackContext>() };
    ctx.send(MpvEvent::RenderUpdate);
    ctx.request_repaint();
}

pub struct MpvRender {
    // Mpv render API context
    ctx: *mut libmpv_sys::mpv_render_context,

    // Event notification channel
    events: Option<channel::Receiver<MpvEvent>>,
}

impl Drop for MpvRender {
    fn drop(&mut self) {
        if !self.ctx.is_null() {
            // SAFETY: we just checked for null
            unsafe { libmpv_sys::mpv_render_context_free(self.ctx) };
            self.ctx = ptr::null_mut();
        }
        // FIXME: free the callback context boxes?
    }
}

impl MpvRender {
    pub fn new(mpv: *mut libmpv_sys::mpv_handle, cc: &eframe::CreationContext<'_>) -> Self {
        // Create the machinery needed to receive events from MPV so that we can drive the
        // render loop successfully.
        let (send, recv) = channel::unbounded();
        let cb_ctx = CallbackContext::new(cc.egui_ctx.clone(), send);
        let render_cb_ctx = Box::new(cb_ctx);

        // Create the MPV renderer in "advanced" mode. This means we need to be very careful
        // with how we access the context, but means we won't deadlock accidentally, barring
        // legacy bugs that we shouldn't be shipping.
        let mut ctx = ptr::null_mut();
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
            libmpv_sys::mpv_render_context_create(&mut ctx, mpv, &mut params_pack[0]);

            libmpv_sys::mpv_render_context_set_update_callback(
                ctx,
                Some(mpv_render_update_callback),
                Box::into_raw(render_cb_ctx).cast(),
            );
        }

        Self {
            ctx,
            events: Some(recv),
        }
    }

    pub fn render_ptr(&self) -> *mut libmpv_sys::mpv_render_context {
        self.ctx
    }

    pub fn drain_events(&self) -> bool {
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
                MpvEvent::CoreUpdate => {}
            }
        }
        redraw
    }
}
