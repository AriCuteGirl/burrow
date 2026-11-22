mod archive;
mod config;
mod drives;
mod net;
mod ops;
mod rename;
mod share;
mod theme;
mod transfer;
mod ui;
mod util;

use gtk::gio;
use gtk::prelude::*;

pub const APP_ID: &str = "world.puppygirl.Burrow";

fn main() -> gtk::glib::ExitCode {
    let mut flags = gio::ApplicationFlags::HANDLES_OPEN;
    // For testing a second copy next to the one in use (it would otherwise hand over to it)
    if std::env::var_os("BURROW_NON_UNIQUE").is_some() {
        flags |= gio::ApplicationFlags::NON_UNIQUE;
    }
    let app = gtk::Application::builder()
        .application_id(APP_ID)
        .flags(flags)
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
