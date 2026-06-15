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
use crate::settings;
use crate::window;
use std::path::Path;
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
            AppCommand::Navigate(forward) => {
                // Immediate path (QA / MCP / HTTP). Flush any pending
                // debounced delta first so tests see a deterministic move.
                self.flush_pending_nav();
                self.navigate_by(if forward { 1 } else { -1 });
            }
            AppCommand::NavigateDebounced(forward) => {
                self.queue_nav_step(event_loop, if forward { 1 } else { -1 });
            }
            AppCommand::GoToFirst => {
                self.flush_pending_nav();
                self.navigate_to_first();
            }
            AppCommand::GoToLast => {
                self.flush_pending_nav();
                self.navigate_to_last();
            }
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
                }
                if enabled
                    && let (Some(win), Some((iw, ih))) =
                        (&self.window, self.navigation.current_image_size)
                    && let Some(size) =
                        window::resize_to_fit_image(win, iw, ih, self.content_offset_y())
                {
                    // Push the new window size into the view BEFORE re-fitting zoom. The OS
                    // resize is async, so `apply_initial_zoom` would otherwise fit against the
                    // stale (larger) window and leave the image at its old, inflated zoom.
                    let (pw, ph) = from_physical_size(size);
                    if let Some(renderer) = &mut self.renderer {
                        renderer.resize(pw, ph);
                        self.zoom.view.update_dimensions(
                            iw,
                            ih,
                            renderer.logical_width(),
                            renderer.logical_height(),
                        );
                    }
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
            AppCommand::SetPreloadNeighbors(enabled) => {
                self.navigation.preload_neighbors = enabled;
                log::info!("Preload neighbors set to: {enabled}");
                let mut s = settings::Settings::load();
                s.preload_neighbors = enabled;
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
            AppCommand::ToggleHistogram => {
                self.histogram.visible = !self.histogram.visible;
                log::debug!("Histogram visible: {}", self.histogram.visible);
                let mut s = settings::Settings::load();
                s.histogram_visible = self.histogram.visible;
                s.save();
                if let Some(menu) = &self.app_menu {
                    menu.histogram_item.set_checked(self.histogram.visible);
                }
                if self.histogram.visible {
                    // Lazy compute on first toggle: per-image we skip the
                    // scan when the panel is hidden, so the data may be
                    // missing when the user enables it. Compute now from
                    // the cached `DecodedImage`. If no image is currently
                    // displayed (rare), leave it as `None` — the next
                    // display call will fill it in.
                    if self.histogram.data.is_none()
                        && let Some(dir) = self.navigation.dir_list.as_ref()
                        && let Some(image) = self.navigation.image_cache.peek(dir.current())
                    {
                        self.histogram.data =
                            Some(crate::histogram::compute::compute(&image.pixels));
                    }
                } else {
                    self.histogram.hover_bin = None;
                }
                self.update_histogram_hover();
                self.request_redraw();
                self.update_shared_state();
            }
            AppCommand::ToggleExifInfo => {
                self.exif_overlay.visible = !self.exif_overlay.visible;
                log::debug!("Exif info visible: {}", self.exif_overlay.visible);
                let mut s = settings::Settings::load();
                s.exif_visible = self.exif_overlay.visible;
                s.save();
                if let Some(menu) = &self.app_menu {
                    menu.exif_info_item.set_checked(self.exif_overlay.visible);
                }
                self.request_redraw();
                self.update_shared_state();
            }
            AppCommand::ToggleLoopNavigation => {
                self.navigation.loop_navigation = !self.navigation.loop_navigation;
                let enabled = self.navigation.loop_navigation;
                log::info!("Loop navigation: {enabled}");
                let mut s = settings::Settings::load();
                s.loop_navigation = enabled;
                s.save();
                if let Some(menu) = &self.app_menu {
                    menu.loop_navigation_item.set_checked(enabled);
                }
                self.refresh_preload_window();
                self.update_shared_state();
            }
            AppCommand::SetSortBy(new) => {
                let Some(dir) = self.navigation.dir_list.as_mut() else {
                    return;
                };
                if dir.sort_by() == new {
                    return;
                }

                log::info!("Sort by: {new:?}");

                // Cancel all in-flight preload tasks: their captured slot
                // index now points at a different file in the new ordering,
                // which would mis-target `pending_current` matching in
                // `poll_preloader`.
                if let Some(p) = self.navigation.preloader.as_mut() {
                    p.cancel_all();
                }

                // Stash the path of any cache-miss target the user is staring
                // at so we can re-prioritize it under its new index.
                let pending_path = self
                    .navigation
                    .pending_current
                    .and_then(|i| dir.get(i).map(Path::to_path_buf));

                // Re-sort in place; cache stays valid by path.
                dir.set_sort_by(new);

                // Re-resolve pending_current to its new slot and re-issue the
                // priority-zero decode under that slot.
                if let Some(path) = pending_path.as_ref() {
                    let new_pending = dir.files_ref().iter().position(|p| p == path);
                    self.navigation.pending_current = new_pending;
                    if let Some(idx) = new_pending {
                        let total = dir.len();
                        if let Some(preloader) = self.navigation.preloader.as_mut() {
                            preloader.prioritize_target(idx, path.clone(), total);
                        }
                    }
                }

                let mut s = settings::Settings::load();
                s.sort_by = new;
                s.save();

                if let Some(menu) = &self.app_menu {
                    menu.sort_by_name_item
                        .set_checked(matches!(new, crate::navigation::SortBy::Name));
                    menu.sort_by_date_item
                        .set_checked(matches!(new, crate::navigation::SortBy::Date));
                    menu.sort_by_file_type_item
                        .set_checked(matches!(new, crate::navigation::SortBy::FileType));
                }

                self.refresh_preload_window();
                self.update_shared_state();
                self.request_redraw();
            }
            AppCommand::SetCursorPosition { x, y } => {
                self.last_mouse_pos = (Logical(x), Logical(y));
                self.update_histogram_hover();
            }
            AppCommand::ToggleSlideshow => {
                self.toggle_slideshow();
            }
            AppCommand::IncreaseSlideshowSpeed => {
                self.adjust_slideshow_speed(true);
            }
            AppCommand::DecreaseSlideshowSpeed => {
                self.adjust_slideshow_speed(false);
            }
            AppCommand::SetSlideshowSeconds(seconds) => {
                let clamped = crate::slideshow::clamp_seconds(seconds);
                self.slideshow.seconds = clamped;
                log::debug!("Slideshow seconds set to: {clamped}");
                let mut s = settings::Settings::load();
                s.slideshow_seconds = clamped;
                s.save();
                self.slideshow_bump_timer();
                self.update_shared_state();
            }
            AppCommand::SetSlideshowCrossfade(enabled) => {
                self.slideshow.crossfade_enabled = enabled;
                log::debug!("Slideshow crossfade set to: {enabled}");
                let mut s = settings::Settings::load();
                s.slideshow_crossfade = enabled;
                s.save();
            }
            AppCommand::SetSlideshowLoop(enabled) => {
                self.slideshow.loop_enabled = enabled;
                log::debug!("Slideshow loop set to: {enabled}");
                let mut s = settings::Settings::load();
                s.slideshow_loop = enabled;
                s.save();
            }
            AppCommand::SetRawPipelineFlags(flags) => {
                self.raw_flags = flags;
                log::info!(
                    "RAW pipeline flags updated: {} step(s) disabled",
                    flags.disabled_step_labels().len()
                );
                let mut s = settings::Settings::load();
                s.raw = flags;
                s.save();
                self.apply_raw_flag_change();
            }
            AppCommand::SetCustomDcpDir(dir) => {
                log::info!(
                    "Custom DCP directory updated: {}",
                    dir.as_deref().unwrap_or("<cleared>")
                );
                let mut s = settings::Settings::load();
                s.custom_dcp_dir = dir.clone();
                s.save();
                self.apply_custom_dcp_dir_change(dir.as_deref());
            }
            #[cfg(target_os = "macos")]
            AppCommand::DisplayChanged => {
                self.handle_display_changed();
            }
            AppCommand::CopyImage => {
                let current = self
                    .navigation
                    .dir_list
                    .as_ref()
                    .map(|d| d.current().to_path_buf());
                match current {
                    Some(path) => {
                        #[cfg(target_os = "macos")]
                        if crate::platform::macos::clipboard::copy_image_file(&path) {
                            log::info!("Copied image to clipboard: {}", path.display());
                        }
                        #[cfg(not(target_os = "macos"))]
                        {
                            let _ = path;
                            log::debug!("Copy image is not supported on this platform");
                        }
                    }
                    None => log::debug!("Copy image: no image open"),
                }
            }
            AppCommand::Print => {
                #[cfg(target_os = "macos")]
                self.print_current_image();
            }
            AppCommand::ShowAbout => self.show_about_dialog(),
            AppCommand::ShowSettings => self.show_settings_dialog(),
            AppCommand::ShowSettingsSection(ref _section) => {
                #[cfg(target_os = "macos")]
                crate::settings::switch_settings_section(_section);
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
                let sort_by = self
                    .navigation
                    .dir_list
                    .as_ref()
                    .map(|d| d.sort_by())
                    .unwrap_or_default();
                self.navigation.dir_list = directory::DirectoryList::from_file(&resolved, sort_by);
                self.display_image(&resolved);

                if let Some(dir) = &self.navigation.dir_list
                    && let Some(win) = &self.window
                {
                    window::set_title_keeping_buttons(
                        win,
                        &window::window_title_with_position(
                            &resolved,
                            dir.current_index(),
                            dir.len(),
                        ),
                    );
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
            #[cfg(all(debug_assertions, target_os = "macos"))]
            AppCommand::GetWindowNumber(sender) => {
                let number = self.main_window_number().unwrap_or(0);
                let _ = sender.send(number);
            }
            AppCommand::Sync(sender) => {
                self.update_shared_state();
                let _ = sender.send(());
            }

            AppCommand::PreloaderProgress => {
                // No-op handler. Sending ANY user event wakes winit's
                // event loop, and `about_to_wait` runs after, which
                // polls the preloader response channel. The preloader
                // worker thread fires this after every response so a
                // freshly-decoded image is displayed immediately even
                // when the user is idle (no key, no mouse).
            }

            #[cfg(target_os = "macos")]
            AppCommand::ThumbnailsAvailable => {
                // Drain every queued completion in one go. The
                // completion blocks fire this command **only when the
                // queue was previously empty** (see
                // `quicklook::push_delivery`), so a burst of N thumb
                // completions sends 1–2 user events instead of N. Each
                // user event is a winit dispatch; per-event cost adds
                // up enough that 38 events were starving keyboard input
                // for ~12 s during the initial folder scan.
                let batch = self.thumbnails.requests.drain_pending();
                let mut redraw_for_pending = false;
                for delivery in batch {
                    if delivery.folder_generation != self.thumbnails.generation() {
                        log::debug!(
                            "Thumb arrived from stale folder (gen {} != {}), dropping index {}",
                            delivery.folder_generation,
                            self.thumbnails.generation(),
                            delivery.index
                        );
                        continue;
                    }
                    match delivery.result {
                        Ok(pixels) => {
                            log::info!(
                                "Thumb ready: index={} {}x{}",
                                delivery.index,
                                pixels.width,
                                pixels.height
                            );
                            self.thumbnails.mark_ready(
                                delivery.index,
                                pixels.width,
                                pixels.height,
                                pixels.rgba,
                                delivery.request_id,
                            );
                            if self.navigation.pending_current == Some(delivery.index) {
                                redraw_for_pending = true;
                            }
                        }
                        Err(()) => {
                            self.thumbnails
                                .mark_failed(delivery.index, delivery.request_id);
                        }
                    }
                }
                if redraw_for_pending && let Some(index) = self.navigation.pending_current {
                    self.display_thumbnail_placeholder(index);
                }
                self.pump_thumbnail_requests();
            }
        }
    }
}
