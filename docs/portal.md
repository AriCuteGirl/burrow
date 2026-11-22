# Burrow as the file chooser of other apps

Status: researched, not built. Nothing here is promised.

## Is it possible?
Yes. Apps that use portals ask `xdg-desktop-portal`, which hands the request to a *backend*
over D-Bus. A backend for file dialogs implements `org.freedesktop.impl.portal.FileChooser`
(checked against the interface file of xdg-desktop-portal 1.22):

| Method | Used for |
|---|---|
| `OpenFile` | pick files, or folders with `directory=true`, several with `multiple=true` |
| `SaveFile` | save as; `current_name`, `current_folder`, `current_file` |
| `SaveFiles` | save several files into one folder (`files`) |

Each gets `(handle, app_id, parent_window, title, options)` and answers with
`(response, results)`: response 0 = ok, 1 = cancelled, 2 = other; results hold `uris`, and
`choices`, `current_filter`, `writable` where they apply.

`gio::DBusConnection::register_object` can serve this, so it needs no new dependency.

## What it would take
1. `burrow --portal`: own the bus name `org.freedesktop.impl.portal.desktop.burrow`, no window.
2. A chooser window: the normal pane plus Open/Save buttons, name field, filters (globs and
   MIME types), `choices`, overwrite question, multi-select rules.
3. `org.freedesktop.impl.portal.Request` on `handle`, so an app can cancel its dialog.
4. Attach to the asking window: `parent_window` is `wayland:<token>`, imported through
   `xdg-foreign`. gdk4-wayland has this, but that crate is a new dependency, so it needs an OK.
   Without it the dialog works but isn't tied to the window (a float/center rule helps).
5. Files: `burrow.portal`, a D-Bus service file, and in `~/.config/xdg-desktop-portal/`
   `hyprland-portals.conf`: `org.freedesktop.impl.portal.FileChooser = burrow;gtk`
   (with gtk as the fallback). Portal and D-Bus files must be in the *system* data dirs to be
   found reliably, so installing needs root once.

## Limits to know
- Only apps that use the portal are covered: Flatpaks, Firefox/LibreWolf (with
  `widget.use-xdg-desktop-portal.file-picker = 1`), Chromium-based browsers, Electron, GTK4's
  `FileDialog`. Programs with their own dialog (GIMP, Wine, old GTK/Qt apps) keep it.
- It has to be dependable: a crash means "can't save" in every app. It needs its own tests
  and a fallback to gtk before anyone makes it the default.

Size: about as much work as Phase 2. A good next project, best done on its own.
