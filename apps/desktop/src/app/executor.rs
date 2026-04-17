//! `App::execute_command` — the single place where every `AppCommand` is realized.
//!
//! All user actions (keyboard, mouse, menu, QA server, MCP) map to an `AppCommand` and
//! pass through here. Continuous input (scroll zoom, mouse drag) stays inline in the
//! window-event handler.

use super::App;
use crate::commands::AppCommand;
use crate::input;
use crate::navigation::directory;
use crate::pixels::{Logical, from_physical_size, to_logical_pos, to_logical_size};
#[cfg(target_os = "macos")]
use crate::settings;
use crate::window;
use winit::event_loop::ActiveEventLoop;

impl App {
    /// Central command executor. All user actions — keyboard, mouse, menu, QA server —
    /// are mapped to `AppCommand` and dispatched here.
    pub(super) fn execute_command(&mut self, event_loop: &ActiveEventLoop, command: AppCommand) {
        match command {
            AppCommand::SendKey(key_name) => {
                if let Some(cmd) = input::qa_key_to_command(&key_name) {
                    self.execute_command(event_loop, cmd);
                }
            }
            AppCommand::Navigate(forward) => self.navigate(forward),
            AppCommand::ZoomIn => {
                let old_zoom = self.zoom.view.zoom;
                self.zoom.view.keyboard_zoom(true);
                if self.zoom.auto_fit {
                    let (cx, cy) = self.window_center_logical();
                    self.auto_fit_after_zoom(old_zoom, cx, cy);
                }
                self.update_transform_and_redraw();
            }
            AppCommand::ZoomOut => {
                let old_zoom = self.zoom.view.zoom;
                self.zoom.view.keyboard_zoom(false);
                if self.zoom.auto_fit {
                    let (cx, cy) = self.window_center_logical();
                    self.auto_fit_after_zoom(old_zoom, cx, cy);
                }
                self.update_transform_and_redraw();
            }
            AppCommand::SetZoom(level) => {
                let old_zoom = self.zoom.view.zoom;
                self.zoom.view.set_zoom(level);
                if self.zoom.auto_fit {
                    let (cx, cy) = self.window_center_logical();
                    self.auto_fit_after_zoom(old_zoom, cx, cy);
                }
                self.update_transform_and_redraw();
            }
            AppCommand::FitToWindow => {
                let old_zoom = self.zoom.view.zoom;
                self.zoom.view.fit_to_window();
                if self.zoom.auto_fit {
                    let (cx, cy) = self.window_center_logical();
                    self.auto_fit_after_zoom(old_zoom, cx, cy);
                }
                self.update_transform_and_redraw();
            }
            AppCommand::ActualSize => {
                let old_zoom = self.zoom.view.zoom;
                self.zoom.view.actual_size();
                if self.zoom.auto_fit {
                    let (cx, cy) = self.window_center_logical();
                    self.auto_fit_after_zoom(old_zoom, cx, cy);
                }
                self.update_transform_and_redraw();
            }
            AppCommand::ToggleFit => {
                self.zoom.view.toggle_fit();
                self.update_transform_and_redraw();
            }
            AppCommand::ToggleFullscreen => {
                if let Some(win) = &self.window {
                    window::toggle_fullscreen(win);
                    self.update_shared_state();
                }
            }
            AppCommand::SetFullscreen(on) => {
                if let Some(win) = &self.window {
                    window::set_fullscreen(win, on);
                    self.update_shared_state();
                }
            }
            AppCommand::SetAutoFitWindow(enabled) => {
                self.zoom.auto_fit = enabled;
                log::debug!("Auto-fit window set to: {enabled}");
                let mut s = settings::Settings::load();
                s.auto_fit_window = enabled;
                s.save();
                if let Some(menu) = &self.app_menu {
                    menu.auto_fit_item.set_checked(enabled);
                    // "Enlarge small images" is irrelevant when auto-fit is on
                    menu.enlarge_small_item.set_enabled(!enabled);
                }
                if enabled
                    && let (Some(win), Some((iw, ih))) =
                        (&self.window, self.navigation.current_image_size)
                {
                    window::resize_to_fit_image(win, iw, ih, self.content_offset_y());
                }
                // Re-apply zoom: auto-fit changes whether min_zoom can go below 1.0
                self.apply_initial_zoom();
                self.update_transform_and_redraw();
            }
            AppCommand::SetEnlargeSmallImages(enabled) => {
                self.zoom.enlarge = enabled;
                log::debug!("Enlarge small images set to: {enabled}");
                let mut s = settings::Settings::load();
                s.enlarge_small_images = enabled;
                s.save();
                if let Some(menu) = &self.app_menu {
                    menu.enlarge_small_item.set_checked(enabled);
                }
                // Re-apply zoom: toggling this changes whether small images enlarge or not
                self.apply_initial_zoom();
                self.update_transform_and_redraw();
            }
            AppCommand::SetIccColorManagement(enabled) => {
                self.color.icc_enabled = enabled;
                log::info!("ICC color management set to: {enabled}");
                let mut s = settings::Settings::load();
                s.icc_color_management = enabled;
                s.save();
                if let Some(menu) = &self.app_menu {
                    menu.icc_color_management_item.set_checked(enabled);
                    // "Color match display" and "Relative colorimetric" depend on ICC being enabled
                    menu.color_match_item.set_enabled(enabled);
                    menu.relative_colorimetric_item.set_enabled(enabled);
                }
                self.apply_icc_settings();
            }
            AppCommand::SetColorMatchDisplay(enabled) => {
                self.color.match_display = enabled;
                log::info!("Color match display set to: {enabled}");
                let mut s = settings::Settings::load();
                s.color_match_display = enabled;
                s.save();
                if let Some(menu) = &self.app_menu {
                    menu.color_match_item.set_checked(enabled);
                }
                self.apply_icc_settings();
            }
            AppCommand::SetRelativeColorimetric(enabled) => {
                self.color.relative_col = enabled;
                log::info!(
                    "Rendering intent set to: {}",
                    if enabled {
                        "relative colorimetric"
                    } else {
                        "perceptual"
                    }
                );
                let mut s = settings::Settings::load();
                s.use_relative_colorimetric = enabled;
                s.save();
                if let Some(menu) = &self.app_menu {
                    menu.relative_colorimetric_item.set_checked(enabled);
                }
                self.flush_and_redisplay();
            }
            AppCommand::SetScrollToZoom(enabled) => {
                self.zoom.scroll_to_zoom = enabled;
                log::debug!("Scroll to zoom set to: {enabled}");
                let mut s = settings::Settings::load();
                s.scroll_to_zoom = enabled;
                s.save();
                self.update_shared_state();
            }
            AppCommand::SetTitleBar(enabled) => {
                self.title_bar = enabled;
                log::debug!("Title bar set to: {enabled}");
                let mut s = settings::Settings::load();
                s.title_bar = enabled;
                s.save();
                self.apply_content_offset();
                self.update_shared_state();
            }
            #[cfg(target_os = "macos")]
            AppCommand::DisplayChanged => {
                self.handle_display_changed();
            }
            AppCommand::ShowAbout => self.show_about_dialog(),
            AppCommand::ShowSettings => self.show_settings_dialog(),
            AppCommand::ShowSettingsSection(ref section) => {
                #[cfg(target_os = "macos")]
                crate::settings::switch_settings_section(section);
            }
            AppCommand::CloseSettings => {
                #[cfg(target_os = "macos")]
                crate::settings::close_settings_window();
            }
            AppCommand::Exit => {
                // Escape exits fullscreen first, then exits the app
                if let Some(win) = &self.window
                    && window::is_fullscreen(win)
                {
                    log::info!("Fullscreen off");
                    window::set_fullscreen(win, false);
                    self.update_shared_state();
                    return;
                }
                log::info!("Exiting");
                if let Some(preloader) = self.navigation.preloader.take() {
                    preloader.shutdown();
                }
                event_loop.exit();
            }
            AppCommand::OpenFile(path) => {
                let resolved = path.canonicalize().unwrap_or(path);
                if !resolved.is_file() {
                    log::warn!("OpenFile: not a file: {}", resolved.display());
                    return;
                }

                // If we were waiting for a file (Finder double-click), initialize the app now
                if self.waiting_for_file {
                    log::info!("File received via Apple Event, initializing viewer");
                    self.waiting_for_file = false;
                    self.wait_start = None;
                    self.file_path = resolved.clone();

                    // Close the onboarding window if it's showing
                    #[cfg(target_os = "macos")]
                    crate::onboarding::close_window();

                    // Initialize the full viewer (window, renderer, etc.) via resumed()
                    // by switching control flow — resumed() will be called next
                    self.initialize_viewer(event_loop);
                    return;
                }

                self.file_path = resolved.clone();
                self.navigation.dir_list = directory::DirectoryList::from_file(&resolved);
                self.display_image(&resolved);

                if let Some(dir) = &self.navigation.dir_list
                    && let Some(win) = &self.window
                {
                    win.set_title(&window::window_title_with_position(
                        &resolved,
                        dir.current_index(),
                        dir.len(),
                    ));
                }

                self.update_shared_state();
            }
            AppCommand::SetWindowGeometry {
                x,
                y,
                width,
                height,
            } => {
                if let Some(win) = &self.window {
                    if let Some(w) = width
                        && let Some(h) = height
                    {
                        let _ = win.request_inner_size(to_logical_size(
                            Logical(w as f64),
                            Logical(h as f64),
                        ));
                    }
                    if x.is_some() || y.is_some() {
                        let current = win.outer_position().unwrap_or_default();
                        let new_x = x.unwrap_or(current.x);
                        let new_y = y.unwrap_or(current.y);
                        win.set_outer_position(to_logical_pos(
                            Logical(new_x as f64),
                            Logical(new_y as f64),
                        ));
                    }
                    if let Some(renderer) = &mut self.renderer {
                        let (pw, ph) = from_physical_size(win.inner_size());
                        renderer.resize(pw, ph);
                        if let Some((iw, ih)) = self.navigation.current_image_size {
                            self.zoom.view.update_dimensions(
                                iw,
                                ih,
                                renderer.logical_width(),
                                renderer.logical_height(),
                            );
                        }
                    }
                    self.update_min_zoom();
                    if let Some(renderer) = &self.renderer {
                        renderer.update_transform(&self.zoom.view.transform());
                    }
                    self.request_redraw();
                    self.update_shared_state();
                }
            }
            AppCommand::ScrollZoom {
                delta,
                cursor_x,
                cursor_y,
            } => {
                let old_zoom = self.zoom.view.zoom;
                let image_cy = cursor_y - self.content_offset_y().0;
                self.zoom
                    .view
                    .scroll_zoom(delta, Logical(cursor_x), Logical(image_cy));
                if self.zoom.auto_fit {
                    self.auto_fit_after_zoom(
                        old_zoom,
                        Logical(cursor_x as f64),
                        Logical(cursor_y as f64),
                    );
                }
                self.update_transform_and_redraw();
            }
            AppCommand::Refresh => {
                if let Some(path) = self
                    .navigation
                    .dir_list
                    .as_ref()
                    .map(|d| d.current().to_path_buf())
                {
                    self.display_image(&path);
                    self.update_shared_state();
                }
            }
            AppCommand::TakeScreenshot(sender) => {
                let png_bytes = if let Some(renderer) = &self.renderer {
                    renderer.capture_screenshot()
                } else {
                    Vec::new()
                };
                let _ = sender.send(png_bytes);
            }
            AppCommand::Sync(sender) => {
                self.update_shared_state();
                let _ = sender.send(());
            }
        }
    }
}
