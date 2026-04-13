use tauri::menu::{Menu, MenuBuilder, MenuItemBuilder, SubmenuBuilder};
use tauri::{AppHandle, Emitter, Manager, Wry};

/// Build the native menu bar for the app.
pub fn build_menu(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    let about = MenuItemBuilder::with_id("about", "About Prvw").build(app)?;
    let settings = MenuItemBuilder::with_id("settings", "Settings...")
        .accelerator("CmdOrCtrl+,")
        .build(app)?;

    // App submenu (macOS puts the first submenu under the app name)
    let app_submenu = SubmenuBuilder::new(app, "Prvw")
        .item(&about)
        .separator()
        .item(&settings)
        .separator()
        .hide()
        .hide_others()
        .show_all()
        .separator()
        .quit()
        .build()?;

    // File submenu
    let file_submenu = SubmenuBuilder::new(app, "File").close_window().build()?;

    // View submenu
    let zoom_in = MenuItemBuilder::with_id("zoom_in", "Zoom in")
        .accelerator("CmdOrCtrl+=")
        .build(app)?;
    let zoom_out = MenuItemBuilder::with_id("zoom_out", "Zoom out")
        .accelerator("CmdOrCtrl+-")
        .build(app)?;
    let actual_size = MenuItemBuilder::with_id("actual_size", "Actual size")
        .accelerator("CmdOrCtrl+1")
        .build(app)?;
    let fit_to_window = MenuItemBuilder::with_id("fit_to_window", "Fit to window")
        .accelerator("CmdOrCtrl+0")
        .build(app)?;
    let fullscreen = MenuItemBuilder::with_id("fullscreen", "Enter full screen")
        .accelerator("Ctrl+CmdOrCtrl+F")
        .build(app)?;

    let view_submenu = SubmenuBuilder::new(app, "View")
        .item(&zoom_in)
        .item(&zoom_out)
        .separator()
        .item(&actual_size)
        .item(&fit_to_window)
        .separator()
        .item(&fullscreen)
        .build()?;

    // Navigate submenu
    let next = MenuItemBuilder::with_id("next", "Next image")
        .accelerator("Right")
        .build(app)?;
    let previous = MenuItemBuilder::with_id("previous", "Previous image")
        .accelerator("Left")
        .build(app)?;

    let nav_submenu = SubmenuBuilder::new(app, "Navigate")
        .item(&previous)
        .item(&next)
        .build()?;

    let menu = MenuBuilder::new(app)
        .item(&app_submenu)
        .item(&file_submenu)
        .item(&view_submenu)
        .item(&nav_submenu)
        .build()?;

    log::debug!("Menu bar created");
    Ok(menu)
}

/// Handle menu item clicks by emitting Tauri events that the JS frontend listens to.
pub fn handle_menu_event(app: &AppHandle, event: tauri::menu::MenuEvent) {
    let id = event.id();
    log::debug!("Menu event: {}", id.as_ref());

    match id.as_ref() {
        "about" => {
            crate::do_open_dialog_window(app, "about", "About Prvw", 360.0, 240.0).ok();
        }
        "settings" => {
            crate::do_open_dialog_window(app, "settings", "Settings", 480.0, 360.0).ok();
        }
        "zoom_in" => {
            app.emit("menu-action", "zoom_in").ok();
        }
        "zoom_out" => {
            app.emit("menu-action", "zoom_out").ok();
        }
        "actual_size" => {
            app.emit("menu-action", "actual_size").ok();
        }
        "fit_to_window" => {
            app.emit("menu-action", "fit_to_window").ok();
        }
        "fullscreen" => {
            app.emit("menu-action", "fullscreen").ok();
        }
        "next" | "previous" => {
            let forward = id.as_ref() == "next";
            let state_mutex = app.state::<std::sync::Mutex<crate::AppState>>();
            let mut state = state_mutex.lock().unwrap();
            if let Some(window) = app.get_webview_window("main") {
                crate::do_navigate(&mut state, forward, &window);
                if let Some(dir) = &state.dir_list {
                    let path = dir.current().to_string_lossy().to_string();
                    let _ = app.emit(
                        "state-changed",
                        serde_json::json!({
                            "filePath": path,
                            "index": dir.current_index(),
                            "total": dir.len()
                        }),
                    );
                }
            }
        }
        _ => {}
    }
}
