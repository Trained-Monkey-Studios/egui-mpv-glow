use std::{env, path::PathBuf};

// A minimal `eframe` app.
#[derive(Default)]
struct App {
    // We need to retain state between frames, so store an MpvPlayer somewhere.
    player: egui_mpv_glow::MpvPlayer,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // The eframe template will also pull prior state from a RON object; for this demo,
        // we just use default initialization.
        let mut app: Self = Default::default();

        // We _must_ call one of the init_* methods before using the player. Eframe depends on
        // default-initialization, but in order to initialize and bind MPV to glow and egui, we
        // need the egui Context and glow's get_proc_address.
        app.player
            .init_with_eframe(cc)
            .expect("Failed to initialize mpv");

        // For the demo, we just immediately play the file passed as the first argument.
        if let Some(filename) = env::args().nth(1) {
            app.player.play(&PathBuf::from(filename));
        }

        app
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // The minimal demo just checks for the Escape key to exit.
        let mut exit = false;
        ctx.input(|input| {
            if input.key_pressed(egui::Key::Escape) {
                exit = true;
            }
        });
        if exit {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        // Create a full-window panel.
        egui::CentralPanel::default().show(ctx, |ui| {
            // Draw the video into the full panel area.
            let window_rect = ui.available_rect_before_wrap();

            // The image will only be unavailable in the brief time between when we start playing
            // the file and when the first frame of the file arrives. The decoded stream may have
            // a different resolution than the area we pass here: MPV will do its best to fit the
            // video to the given area and do the rescaling in hardware. Adjust the MPV properties
            // for letterboxing, etc to change the scaling behavior.
            let Some(img) = self.player.image(&window_rect, ui.painter(), frame) else {
                ui.spinner();
                return;
            };

            // paint_at doesn't allocate space, so we could proceed to draw video controls on top
            // of the video. If you want the video to consume space, use `ui.add(img)`.
            img.paint_at(ui, window_rect);
        });
    }
}

fn main() -> eframe::Result {
    env_logger::init();
    eframe::run_native(
        "EguiMpvGlow Demo",
        eframe::NativeOptions::default(),
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}
