
#[derive(bitbag::Flags)]
#[repr(u64)]
pub enum RenderFrameInfoFlags {
    /**
     * Set if there is actually a next frame. If unset, there is no next frame
     * yet, and other flags and fields that require a frame to be queued will
     * be unset.
     *
     * This is set for _any_ kind of frame, even for redraw requests.
     *
     * Note that when this is unset, it simply means no new frame was
     * decoded/queued yet, not necessarily that the end of the video was
     * reached. A new frame can be queued after some time.
     *
     * If the return value of mpv_render_context_render() had the
     * MPV_RENDER_UPDATE_FRAME flag set, this flag will usually be set as well,
     * unless the frame is rendered, or discarded by other asynchronous events.
     */
    Present         = 1 << 0,
    /**
     * If set, the frame is not an actual new video frame, but a redraw request.
     * For example if the video is paused, and an option that affects video
     * rendering was changed (or any other reason), an update request can be
     * issued and this flag will be set.
     *
     * Typically, redraw frames will not be subject to video timing.
     *
     * Implies MPV_RENDER_FRAME_INFO_PRESENT.
     */
    Redraw          = 1 << 1,
    /**
     * If set, this is supposed to reproduce the previous frame perfectly. This
     * is usually used for certain "video-sync" options ("display-..." modes).
     * Typically the renderer will blit the video from a FBO. Unset otherwise.
     *
     * Implies MPV_RENDER_FRAME_INFO_PRESENT.
     */
    Repeat          = 1 << 2,
    /**
     * If set, the player timing code expects that the user thread blocks on
     * vsync (by either delaying the render call, or by making a call to
     * mpv_render_context_report_swap() at vsync time).
     *
     * Implies MPV_RENDER_FRAME_INFO_PRESENT.
     */
    BlockVsync     = 1 << 3,
}

#[repr(u64)]
pub enum EventId {
    /**
     * Nothing happened. Happens on timeouts or sporadic wakeups.
     */
    None              = 0,
    /**
     * Happens when the player quits. The player enters a state where it tries
     * to disconnect all clients. Most requests to the player will fail, and
     * the client should react to this and quit with mpv_destroy() as soon as
     * possible.
     */
    Shutdown          = 1,
    /**
     * See mpv_request_log_messages().
     */
    LogMessage       = 2,
    /**
     * Reply to a mpv_get_property_async() request.
     * See also mpv_event and mpv_event_property.
     */
    GetPropertyReply = 3,
    /**
     * Reply to a mpv_set_property_async() request.
     * (Unlike GET_PROPERTY, mpv_event_property is not used.)
     */
    SetPropertyReply = 4,
    /**
     * Reply to a mpv_command_async() or mpv_command_node_async() request.
     * See also mpv_event and mpv_event_command.
     */
    CommandReply     = 5,
    /**
     * Notification before playback start of a file (before the file is loaded).
     * See also mpv_event and mpv_event_start_file.
     */
    StartFile        = 6,
    /**
     * Notification after playback end (after the file was unloaded).
     * See also mpv_event and mpv_event_end_file.
     */
    EndFile          = 7,
    /**
     * Notification when the file has been loaded (headers were read etc.), and
     * decoding starts.
     */
    FileLoaded       = 8,
    /**
     * Idle mode was entered. In this mode, no file is played, and the playback
     * core waits for new commands. (The command line player normally quits
     * instead of entering idle mode, unless --idle was specified. If mpv
     * was started with mpv_create(), idle mode is enabled by default.)
     *
     * @deprecated This is equivalent to using mpv_observe_property() on the
     *             "idle-active" property. The event is redundant, and might be
     *             removed in the far future. As a further warning, this event
     *             is not necessarily sent at the right point anymore (at the
     *             start of the program), while the property behaves correctly.
     */
    Idle              = 11,
    /**
     * Sent every time after a video frame is displayed. Note that currently,
     * this will be sent in lower frequency if there is no video, or playback
     * is paused - but that will be removed in the future, and it will be
     * restricted to video frames only.
     *
     * @deprecated Use mpv_observe_property() with relevant properties instead
     *             (such as "playback-time").
     */
    Tick              = 14,
    /**
     * Triggered by the script-message input command. The command uses the
     * first argument of the command as client name (see mpv_client_name()) to
     * dispatch the message, and passes along all arguments starting from the
     * second argument as strings.
     * See also mpv_event and mpv_event_client_message.
     */
    ClientMessage    = 16,
    /**
     * Happens after video changed in some way. This can happen on resolution
     * changes, pixel format changes, or video filter changes. The event is
     * sent after the video filters and the VO are reconfigured. Applications
     * embedding a mpv window should listen to this event in order to resize
     * the window if needed.
     * Note that this event can happen sporadically, and you should check
     * yourself whether the video parameters really changed before doing
     * something expensive.
     */
    VideoReconfig    = 17,
    /**
     * Similar to VIDEO_RECONFIG. This is relatively uninteresting,
     * because there is no such thing as audio output embedding.
     */
    AudioReconfig    = 18,
    /**
     * Happens when a seek was initiated. Playback stops. Usually it will
     * resume with PLAYBACK_RESTART as soon as the seek is finished.
     */
    Seek              = 20,
    /**
     * There was a discontinuity of some sort (like a seek), and playback
     * was reinitialized. Usually happens on start of playback and after
     * seeking. The main purpose is allowing the client to detect when a seek
     * request is finished.
     */
    PlaybackRestart  = 21,
    /**
     * Event sent due to mpv_observe_property().
     * See also mpv_event and mpv_event_property.
     */
    PropertyChange   = 22,
    /**
     * Happens if the internal per-mpv_handle ringbuffer overflows, and at
     * least 1 event had to be dropped. This can happen if the client doesn't
     * read the event queue quickly enough with mpv_wait_event(), or if the
     * client makes a very large number of asynchronous calls at once.
     *
     * Event delivery will continue normally once this event was returned
     * (this forces the client to empty the queue completely).
     */
    QueueOverflow    = 24,
    /**
     * Triggered if a hook handler was registered with mpv_hook_add(), and the
     * hook is invoked. If you receive this, you must handle it, and continue
     * the hook with mpv_hook_continue().
     * See also mpv_event and mpv_event_hook.
     */
    Hook              = 25,
    // Internal note: adjust INTERNAL_EVENT_BASE when adding new events.
}
