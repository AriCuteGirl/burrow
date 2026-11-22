mod actions;
mod archive;
mod config;
mod drives;
mod keys;
mod net;
mod ops;
mod rename;
mod search;
mod share;
mod theme;
mod thumbs;
mod transfer;
mod ui;
mod util;

use gtk::gio;
use gtk::prelude::*;

pub const APP_ID: &str = "world.puppygirl.Burrow";

fn main() -> gtk::glib::ExitCode {
    // A second copy for testing: its own app-id, so it doesn't hand over to the one in use
    let id = std::env::var("BURROW_APP_ID").ok().filter(|i| gio::Application::id_is_valid(i)).unwrap_or_else(|| APP_ID.into());
    let app = gtk::Application::builder()
        .application_id(id)
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
