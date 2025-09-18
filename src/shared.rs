use crossbeam::channel::Sender;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MpvEvent {
    CoreUpdate,
    RenderUpdate,
}

#[derive(Clone, Debug)]
pub(crate) struct CallbackContext {
    ctx: egui::Context,
    events: Sender<MpvEvent>,
}

impl CallbackContext {
    pub(crate) fn new(ctx: egui::Context, sender: Sender<MpvEvent>) -> Self {
        Self {
            ctx,
            events: sender,
        }
    }

    pub(crate) fn send(&self, event: MpvEvent) {
        self.events.send(event).expect("client disconnected");
    }

    pub(crate) fn request_repaint(&self) {
        self.ctx.request_repaint();
    }
}
