use crate::{
    omt_client::{ClientRequest, MpvOmtClient},
    shared::{CallbackContext, MpvEvent},
};
use anyhow::Result;
use crossbeam::channel;
use log::{debug, error, info, trace};
use std::{collections::HashMap, ffi, mem, path::Path, ptr, thread};

#[derive(Default)]
struct AsyncClientState {
    is_paused: bool,
    percent_pos: f64,
    time_pos: f64,
    time_remaining: f64,
    duration: f64,
}

// When using advanced mode, we're required to listen for events and respond appropriately.
extern "C" fn mpv_update_callback(cb_ctx: *mut ffi::c_void) {
    // SAFETY: we boxed the callback context so it wouldn't move after passing the address to MPV.
    let ctx = unsafe { &*cb_ctx.cast::<CallbackContext>() };
    ctx.send(MpvEvent::CoreUpdate);
    ctx.request_repaint();
}

/// Safe wrapper for libmpv when using an "advanced" mode renderer.
///
/// The "advanced" client wraps the normal client and only exposes the methods that are safe to
/// call when rendering in advanced mode. Additionally, it connects to and listens for events from
/// MPV to help drive the rendering loop safely.
pub struct MpvAdvancedClient {
    client: libmpv::Mpv,
    events: channel::Receiver<MpvEvent>,
    cb_ctx_p: *mut CallbackContext,
    omt_send: channel::Sender<ClientRequest>,
    omt: thread::JoinHandle<()>,
    next_id: u64,
    outstanding: HashMap<u64, ClientRequest>,
    async_state: AsyncClientState,
}

impl Drop for MpvAdvancedClient {
    fn drop(&mut self) {
        // Shut down the background thread
        self.omt_send
            .send(ClientRequest::Exit)
            .expect("omt disconnected");
        let mut tmp = thread::spawn(move || {});
        mem::swap(&mut tmp, &mut self.omt);
        tmp.join().expect("omt exited");

        // SAFETY: unset the wakeup callback before dropping the heap context pointer
        unsafe {
            // Disconnect the wakeup callback
            libmpv_sys::mpv_set_wakeup_callback(self.client.ctx.as_ptr(), None, ptr::null_mut());

            // Free the callback context we leaked to send to C
            let mut cb_ctx_p = ptr::null_mut();
            mem::swap(&mut self.cb_ctx_p, &mut cb_ctx_p);
            let cb_ctx = Box::from_raw(cb_ctx_p);
            drop(cb_ctx);
        }
    }
}

impl MpvAdvancedClient {
    pub fn new(ctx: egui::Context) -> Result<Self> {
        let client = libmpv::Mpv::with_initializer(|init| {
            init.set_property("idle", true)?;
            init.set_property("vo", "libmpv")?;
            init.set_property("vd-lavc-dr", true)?;
            Ok(())
        })
        .map_err(|e| anyhow::anyhow!("mpv init error: {}", e))?;

        // Create the machinery needed to receive events from MPV so that we can drive the
        // render loop successfully.
        let (wakeup_send, wakeup_recv) = channel::unbounded();
        let cb_ctx = Box::new(CallbackContext::new(ctx, wakeup_send));
        let cb_ctx_p = Box::into_raw(cb_ctx);

        // SAFETY: we take ownership of the client and cb_ctx; drop unregisters the callback before
        //         freeing the cb_ctx pointer.
        unsafe {
            libmpv_sys::mpv_set_wakeup_callback(
                client.ctx.as_ptr(),
                Some(mpv_update_callback),
                cb_ctx_p.cast(),
            );
        }

        let handle_addr = client.ctx.as_ptr().addr();
        let (omt_send, omt_recv) = channel::unbounded();
        let omt = thread::spawn(move || {
            let handle = handle_addr as *mut libmpv_sys::mpv_handle;
            let mut omt_client = MpvOmtClient::new(handle, omt_recv);
            omt_client.run_until_exit();
        });

        Ok(Self {
            client,
            events: wakeup_recv,
            cb_ctx_p,
            omt_send,
            omt,
            next_id: 1,
            outstanding: HashMap::new(),
            async_state: AsyncClientState::default(),
        })
    }

    pub(crate) fn handle_ptr(&self) -> *mut libmpv_sys::mpv_handle {
        self.client.ctx.as_ptr()
    }

    fn send_reply_receipt(
        &mut self,
        event: &libmpv_sys::mpv_event,
        verbose: bool,
    ) -> Option<ClientRequest> {
        self.omt_send
            .send(ClientRequest::Completed(event.reply_userdata))
            .expect("omt disconnect");
        if let Some(msg) = self.outstanding.remove(&event.reply_userdata) {
            if event.error == 0 {
                if verbose {
                    debug!("mpv ok: {msg}");
                }
                return Some(msg);
            } else if verbose {
                error!("mpv error {}: {msg}", event.error);
            }
        } else if verbose {
            error!("mpv responded to missing task: {event:#?}");
        }
        None
    }

    pub(crate) fn drain_events(&mut self) -> bool {
        let mut stopped = false;
        while let Ok(event) = self.events.try_recv() {
            assert_eq!(event, MpvEvent::CoreUpdate, "non-CoreUpdate on client");

            // Note: the docs aren't clear on if one signal can indicate multiple events,
            //       so we loop to be safe here.
            loop {
                // SAFETY: we received a wakeup event from MPV, so we know there is an event available.
                let event = unsafe { libmpv_sys::mpv_wait_event(self.handle_ptr(), 0.0) };
                if event.is_null() {
                    break;
                }
                // SAFETY: just checked for null
                let event = unsafe { *event };
                match event.event_id {
                    libmpv_sys::mpv_event_id_MPV_EVENT_NONE => break,
                    libmpv_sys::mpv_event_id_MPV_EVENT_SET_PROPERTY_REPLY
                    | libmpv_sys::mpv_event_id_MPV_EVENT_COMMAND_REPLY => {
                        self.send_reply_receipt(&event, true);
                    }
                    libmpv_sys::mpv_event_id_MPV_EVENT_GET_PROPERTY_REPLY => {
                        if let Some(msg) = self.send_reply_receipt(&event, true) {
                            let prop: *const libmpv_sys::mpv_event_property = event.data.cast();
                            // SAFETY: as_ref checks for null; mpv_event_command is the documented data for this event.
                            if let Some(prop) = unsafe { prop.as_ref() } {
                                match prop.format {
                                    libmpv_sys::mpv_format_MPV_FORMAT_STRING => {
                                        panic!("get string property should be unused")
                                    }
                                    libmpv_sys::mpv_format_MPV_FORMAT_OSD_STRING => {
                                        panic!("get osd string property should be unused")
                                    }
                                    libmpv_sys::mpv_format_MPV_FORMAT_FLAG => {
                                        let ClientRequest::GetPropertyFlag(_id, name) = msg else {
                                            panic!("unexpected message type");
                                        };
                                        assert_eq!(name, "pause", "unexpected flag property");
                                        let flag_p: *const ffi::c_int = prop.data.cast();
                                        // SAFETY: see documentation on MPV_FORMAT_FLAG
                                        self.async_state.is_paused = unsafe { *flag_p } != 0;
                                    }
                                    libmpv_sys::mpv_format_MPV_FORMAT_INT64 => {
                                        panic!("get int64 property should be unused")
                                    }
                                    libmpv_sys::mpv_format_MPV_FORMAT_DOUBLE => {
                                        let ClientRequest::GetPropertyDouble(_id, name) = msg
                                        else {
                                            panic!("unexpected message type");
                                        };
                                        let dbl_p: *const ffi::c_double = prop.data.cast();
                                        // SAFETY: see documentation on MPV_FORMAT_DOUBLE
                                        let value = unsafe { *dbl_p };
                                        match name.as_str() {
                                            "percent-pos" => self.async_state.percent_pos = value,
                                            "time-pos" => self.async_state.time_pos = value,
                                            "time-remaining" => {
                                                self.async_state.time_remaining = value;
                                            }
                                            "duration" => {
                                                self.async_state.duration = value;
                                            }
                                            _ => panic!("unexpected double property name"),
                                        }
                                    }
                                    libmpv_sys::mpv_format_MPV_FORMAT_NODE => {
                                        panic!("get node property should be unused")
                                    }
                                    libmpv_sys::mpv_format_MPV_FORMAT_NODE_ARRAY => {
                                        panic!("get node array property should be unused")
                                    }
                                    libmpv_sys::mpv_format_MPV_FORMAT_NODE_MAP => {
                                        panic!("get node map property should be unused")
                                    }
                                    libmpv_sys::mpv_format_MPV_FORMAT_BYTE_ARRAY => {
                                        panic!("get byte array property should be unused")
                                    }
                                    /* libmpv_sys::mpv_format_MPV_FORMAT_NONE */ _ => {}
                                }
                            }
                        }
                    }
                    libmpv_sys::mpv_event_id_MPV_EVENT_LOG_MESSAGE => {
                        let msg: *const libmpv_sys::mpv_event_log_message = event.data as *const _;
                        // SAFETY: the message is documented as being present
                        let msg = unsafe { &*msg };
                        let level = match msg.log_level {
                            libmpv_sys::mpv_log_level_MPV_LOG_LEVEL_NONE
                            | libmpv_sys::mpv_log_level_MPV_LOG_LEVEL_FATAL
                            | libmpv_sys::mpv_log_level_MPV_LOG_LEVEL_ERROR => log::Level::Error,
                            libmpv_sys::mpv_log_level_MPV_LOG_LEVEL_WARN => log::Level::Warn,
                            libmpv_sys::mpv_log_level_MPV_LOG_LEVEL_INFO => log::Level::Info,
                            libmpv_sys::mpv_log_level_MPV_LOG_LEVEL_V
                            | libmpv_sys::mpv_log_level_MPV_LOG_LEVEL_DEBUG => log::Level::Debug,
                            _ => log::Level::Trace,
                        };
                        // SAFETY: the message is documented as being present
                        let content = unsafe { ffi::CStr::from_ptr(msg.text) }.to_string_lossy();
                        for line in content.split('\n') {
                            log::log!(level, "{line}");
                        }
                    }
                    libmpv_sys::mpv_event_id_MPV_EVENT_START_FILE => {
                        // SAFETY: per the docs, the only element in the struct passed here is the playlist offset.
                        let playlist =
                            unsafe { *(event.data as *const libmpv_sys::mpv_event_start_file) };
                        debug!(
                            "mpv started playing file at playlist entry id: {}",
                            playlist.playlist_entry_id
                        );
                    }
                    libmpv_sys::mpv_event_id_MPV_EVENT_END_FILE => {
                        debug!("mpv finished playing file, stopping automatically");
                        stopped = true;
                    }
                    libmpv_sys::mpv_event_id_MPV_EVENT_IDLE => {}
                    libmpv_sys::mpv_event_id_MPV_EVENT_FILE_LOADED => {
                        debug!("mpv decoding started on current file");
                    }
                    libmpv_sys::mpv_event_id_MPV_EVENT_AUDIO_RECONFIG => {
                        trace!("mpv audio has been reconfigured");
                    }
                    libmpv_sys::mpv_event_id_MPV_EVENT_VIDEO_RECONFIG => {
                        // Note: we don't resize the texture here because we resize the texture
                        //       to the _display_ size on output so that MPV/ffmpeg will do the
                        //       resizing for us, with proper letterboxing and whatnot.
                        trace!("mpv video has been reconfigured");
                    }
                    libmpv_sys::mpv_event_id_MPV_EVENT_SEEK => {
                        // Should receive a PLAYBACK_RESTART event after this one.
                        trace!("mpv video seek requested");
                    }
                    libmpv_sys::mpv_event_id_MPV_EVENT_PLAYBACK_RESTART => {
                        // Playback position update or start is complete
                        debug!("mpv video seek finished");
                    }
                    _ => info!("unrecognized mpv event: {event:#?}"),
                }
            }
        }

        // Send requests to update our async client state.
        self.get_property_flag_async("pause")
            .expect("client disconnect");
        self.get_property_double_async("percent-pos")
            .expect("client disconnect");
        self.get_property_double_async("time-pos")
            .expect("client disconnect");
        self.get_property_double_async("time-remaining")
            .expect("client disconnect");
        self.get_property_double_async("duration")
            .expect("client disconnect");

        stopped
    }

    pub fn command_async(&mut self, name: &str, args: &[&str]) -> Result<()> {
        let mut cmd = vec![name.to_owned()];
        cmd.extend(args.iter().map(|v| (*v).to_owned()));
        let id = self.next_id;
        self.next_id += 1;
        let msg = ClientRequest::Command(id, cmd);
        self.outstanding.insert(id, msg.clone());
        self.omt_send.send(msg)?;
        Ok(())
    }

    pub fn set_property_flag_async(&mut self, name: &str, value: bool) -> Result<()> {
        let id = self.next_id;
        self.next_id += 1;
        let msg = ClientRequest::SetPropertyFlag(id, name.to_owned(), value);
        self.outstanding.insert(id, msg.clone());
        self.omt_send.send(msg)?;
        Ok(())
    }

    pub fn get_property_flag_async(&mut self, name: &str) -> Result<()> {
        let id = self.next_id;
        self.next_id += 1;
        let msg = ClientRequest::GetPropertyFlag(id, name.to_owned());
        self.outstanding.insert(id, msg.clone());
        self.omt_send.send(msg)?;
        Ok(())
    }

    pub fn get_property_double_async(&mut self, name: &str) -> Result<()> {
        let id = self.next_id;
        self.next_id += 1;
        let msg = ClientRequest::GetPropertyDouble(id, name.to_owned());
        self.outstanding.insert(id, msg.clone());
        self.omt_send.send(msg)?;
        Ok(())
    }

    // --- Seek functions ---
    //

    /// Seek forward relatively from current position in seconds.
    /// This is less exact than `seek_absolute`, see [mpv manual]
    /// (<https://mpv.io/manual/master/#command-interface-[relative|absolute|absolute-percent|relative-percent|exact|keyframes]>).
    pub fn seek_forward_async(&mut self, secs: f64) -> Result<()> {
        self.command_async("seek", &[&format!("{secs}"), "relative"])
    }

    /// See `seek_forward`.
    pub fn seek_backward_async(&mut self, secs: f64) -> Result<()> {
        self.command_async("seek", &[&format!("-{secs}"), "relative"])
    }

    /// Seek to a given absolute secs.
    pub fn seek_absolute_async(&mut self, secs: f64) -> Result<()> {
        self.command_async("seek", &[&format!("{secs}"), "absolute"])
    }

    /// Seek to a given relative percent position (may be negative).
    /// If `percent` of the playtime is bigger than the remaining playtime, the next file is played.
    /// out of bounds values are clamped to either 0 or 100.
    pub fn seek_percent_async(&mut self, percent: isize) -> Result<()> {
        self.command_async("seek", &[&format!("{percent}"), "relative-percent"])
    }

    /// Seek to the given percentage of the playtime.
    pub fn seek_percent_absolute_async(&mut self, percent: usize) -> Result<()> {
        self.command_async("seek", &[&format!("{percent}"), "absolute-percent"])
    }

    /// Revert the previous `seek_` call, can also revert itself.
    pub fn seek_revert_async(&mut self) -> Result<()> {
        self.command_async("revert-seek", &[])
    }

    /// Mark the current position as the position that will be sought to by `seek_revert`.
    pub fn seek_revert_mark_async(&mut self) -> Result<()> {
        self.command_async("revert-seek", &["mark"])
    }

    /// Seek exactly one frame, and pause.
    /// Noop on audio only streams.
    pub fn seek_frame_async(&mut self) -> Result<()> {
        self.command_async("frame-step", &[])
    }

    /// See `seek_frame`.
    /// [Note performance considerations.](https://mpv.io/manual/master/#command-interface-frame-back-step)
    pub fn seek_frame_backward_async(&mut self) -> Result<()> {
        self.command_async("frame-back-step", &[])
    }

    // --- Playlist functions ---
    //

    /// Play the next item of the current playlist.
    /// Does nothing if the current item is the last item.
    pub fn playlist_next_weak_async(&mut self) -> Result<()> {
        self.command_async("playlist-next", &["weak"])
    }

    /// Play the next item of the current playlist.
    /// Terminates playback if the current item is the last item.
    pub fn playlist_next_force_async(&mut self) -> Result<()> {
        self.command_async("playlist-next", &["force"])
    }

    /// See `playlist_next_weak`.
    pub fn playlist_previous_weak_async(&mut self) -> Result<()> {
        self.command_async("playlist-prev", &["weak"])
    }

    /// See `playlist_next_force`.
    pub fn playlist_previous_force_async(&mut self) -> Result<()> {
        self.command_async("playlist-prev", &["force"])
    }

    pub fn playlist_load_files_async(
        &mut self,
        files: &[(&Path, libmpv::FileState, Option<&str>)],
    ) -> Result<()> {
        fn val(state: &libmpv::FileState) -> &str {
            match state {
                libmpv::FileState::Replace => "replace",
                libmpv::FileState::Append => "append",
                libmpv::FileState::AppendPlay => "append-play",
            }
        }

        for (filename, state, options) in files {
            let name = filename.to_string_lossy();
            let mut args = vec![&name, val(state)];
            if let Some(options) = options {
                args.push(options);
            }
            self.command_async("loadfile", &args)?;
        }
        Ok(())
    }

    /// Remove every, except the current, item from the playlist.
    pub fn playlist_clear_async(&mut self) -> Result<()> {
        self.command_async("playlist-clear", &[])
    }

    /// Remove the currently selected item from the playlist.
    pub fn playlist_remove_current_async(&mut self) -> Result<()> {
        self.command_async("playlist-remove", &["current"])
    }

    /// Remove item at `position` from the playlist.
    pub fn playlist_remove_index_async(&mut self, position: usize) -> Result<()> {
        self.command_async("playlist-remove", &[&format!("{position}")])
    }

    /// Move item `old` to the position of item `new`.
    pub fn playlist_move_async(&mut self, old: usize, new: usize) -> Result<()> {
        self.command_async("playlist-move", &[&format!("{new}"), &format!("{old}")])
    }

    /// Shuffle the playlist.
    pub fn playlist_shuffle_async(&mut self) -> Result<()> {
        self.command_async("playlist-shuffle", &[])
    }

    pub fn pause_async(&mut self) -> Result<()> {
        self.set_property_flag_async("pause", true)
    }

    pub fn unpause_async(&mut self) -> Result<()> {
        self.set_property_flag_async("pause", false)
    }

    pub fn is_paused(&self) -> bool {
        self.async_state.is_paused
    }

    pub fn percent_pos(&self) -> f64 {
        self.async_state.percent_pos
    }

    pub fn time_pos(&self) -> f64 {
        self.async_state.time_pos
    }

    pub fn time_remaining(&self) -> f64 {
        self.async_state.time_remaining
    }

    pub fn duration(&self) -> f64 {
        self.async_state.duration
    }
}
