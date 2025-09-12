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
     * If the return value of `mpv_render_context_render()` had the
     * `MPV_RENDER_UPDATE_FRAME` flag set, this flag will usually be set as well,
     * unless the frame is rendered, or discarded by other asynchronous events.
     */
    Present = 1 << 0,
    /**
     * If set, the frame is not an actual new video frame, but a redraw request.
     * For example if the video is paused, and an option that affects video
     * rendering was changed (or any other reason), an update request can be
     * issued and this flag will be set.
     *
     * Typically, redraw frames will not be subject to video timing.
     *
     * Implies `MPV_RENDER_FRAME_INFO_PRESENT`.
     */
    Redraw = 1 << 1,
    /**
     * If set, this is supposed to reproduce the previous frame perfectly. This
     * is usually used for certain "video-sync" options ("display-..." modes).
     * Typically the renderer will blit the video from a FBO. Unset otherwise.
     *
     * Implies `MPV_RENDER_FRAME_INFO_PRESENT`.
     */
    Repeat = 1 << 2,
    /**
     * If set, the player timing code expects that the user thread blocks on
     * vsync (by either delaying the render call, or by making a call to
     * `mpv_render_context_report_swap()` at vsync time).
     *
     * Implies `MPV_RENDER_FRAME_INFO_PRESENT`.
     */
    BlockVsync = 1 << 3,
}
