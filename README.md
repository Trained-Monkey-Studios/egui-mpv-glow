egui-mpv-glow
---
Uses libmpv to perform hardware decoding of videos into OpenGL texture-backed `egui::Image` without
the data ever touching the CPU or main memory. The `egui::Image` can be drawn either via `ui.add` or
`img.paint_at(ui, rect)` to integrate video fluidly into an egui UI. With even a modest GPU, this can easily
draw h.265 encoded 4k video at 60fps, with GUI elements surrounding or on top of the video.

## Usage

Minimal usage example leveraging eframe:
```rust
#[derive(Default)]
struct App {
    player: egui_mpv_glow::MpvPlayer,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Standard eframe setup code
        let mut app: Self = Default::default();

        // An init_* method variant _must_ be called at startup with the GL context
        // to set up the MPV Render subsystem to draw to our GL context.
        app.player.init_with_eframe(cc).unwrap();
        app.player.play(&std::path::PathBuf::from("video.mp4"));

        app
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            let window_rect = ui.available_rect_before_wrap();
            let img = self.player.image(&window_rect, ui.painter(), frame).unwrap();

            // paint_at doesn't allocate space, so we can draw controls on top of the video.
            img.paint_at(ui, window_rect);

            // ... draw controls ...
        });
    }
}
```

See the `player` example in the examples directory for more complex usage.

## Version

The version of this package will match the version of egui that is used. Using it with a different egui will result in
link errors, for obvious reasons. The mpv version is more flexible, but the shared libraries will need to be at least
0.39.0 to match the libmpv API used. See the mpv docs for whatever version you use for information about the transitive
ffmpeg dependency requirements.
