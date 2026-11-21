mod archive;
mod config;
mod drives;
mod ops;
mod theme;
mod transfer;
mod ui;
mod util;

use gtk::gio;
use gtk::prelude::*;

pub const APP_ID: &str = "world.puppygirl.Burrow";

fn main() -> gtk::glib::ExitCode {
    let app = gtk::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();
    app.connect_startup(|_| ui::startup());
    app.connect_activate(|app| ui::open_window(app, None));
    app.connect_open(|app, files, _| {
        for f in files {
            ui::open_window(app, Some(f.clone()));
        }
    });
    app.run()
}
