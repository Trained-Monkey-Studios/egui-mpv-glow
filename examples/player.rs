use std::{env, path::PathBuf};

#[derive(Default)]
struct App {
    player: egui_mpv_glow::MpvPlayer,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut app: Self = Default::default();
        app.player.init_with_eframe(cc).unwrap();
        if let Some(filename) = env::args().skip(1).next() {
            app.player.play(&PathBuf::from(filename));
        }
        app
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let mut exit = false;
        ctx.input(|input| {
            if input.key_pressed(egui::Key::Escape) {
                exit = true;
            }
        });
        if exit {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            let window_rect = ui.available_rect_before_wrap();
            let Some(img) = self.player.image(&window_rect, ui.painter(), frame) else {
                ui.spinner();
                return;
            };

            img.paint_at(ui, window_rect);
            // paint_at doesn't allocate space, so we can now draw video controls on top of the video here.
        });
    }
}

fn main() -> eframe::Result {
    env_logger::init();
    eframe::run_native(
        "EguiMpvGlow Demo",
        eframe::NativeOptions::default(),
        Box::new(|cc| {
            Ok(Box::new(App::new(cc)))
        }),
    )
}