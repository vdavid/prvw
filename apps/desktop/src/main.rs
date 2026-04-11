mod directory;
mod image_loader;
mod menu;
mod preloader;
mod renderer;
mod view;
mod window;

use clap::Parser;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

#[derive(Parser)]
#[command(name = "prvw", about = "A fast, minimal image viewer")]
struct Cli {
    /// Path to the image file to open
    file: PathBuf,
}

fn main() {
    env_logger::init();
    let cli = Cli::parse();

    let file_path = cli.file.canonicalize().unwrap_or_else(|e| {
        eprintln!("Couldn't resolve path {}: {e}", cli.file.display());
        std::process::exit(1);
    });

    if !file_path.is_file() {
        eprintln!("Not a file: {}", file_path.display());
        std::process::exit(1);
    }

    let event_loop = EventLoop::new().expect("Failed to create event loop");
    event_loop.set_control_flow(ControlFlow::Wait);

    let mut app = App::new(file_path);
    event_loop
        .run_app(&mut app)
        .expect("Event loop terminated unexpectedly");
}

/// Application state, created before the event loop starts.
/// The window and renderer are initialized in `resumed()` (required by winit 0.30 on macOS).
struct App {
    file_path: PathBuf,
    window: Option<Arc<Window>>,
    renderer: Option<renderer::Renderer>,
    view_state: view::ViewState,
    menu_ids: Option<menu::MenuIds>,
    dir_list: Option<directory::DirectoryList>,
    preloader: Option<preloader::Preloader>,
    image_cache: preloader::ImageCache,
    /// Keyboard modifier state (Cmd, Shift, etc.)
    modifiers: ModifiersState,
    /// Mouse drag tracking
    drag_start: Option<(f64, f64)>,
    last_mouse_pos: (f64, f64),
    /// Double-click detection
    last_click_time: Option<Instant>,
    /// Whether we need to re-render next frame
    needs_redraw: bool,
}

impl App {
    fn new(file_path: PathBuf) -> Self {
        Self {
            file_path,
            window: None,
            renderer: None,
            view_state: view::ViewState::new(),
            menu_ids: None,
            dir_list: None,
            preloader: None,
            image_cache: preloader::ImageCache::new(),
            modifiers: ModifiersState::empty(),
            drag_start: None,
            last_mouse_pos: (0.0, 0.0),
            last_click_time: None,
            needs_redraw: false,
        }
    }

    /// Load and display an image, updating the renderer and view state.
    fn display_image(&mut self, path: &Path) {
        let renderer = match &mut self.renderer {
            Some(r) => r,
            None => return,
        };

        match image_loader::load_image(path) {
            Ok(image) => {
                self.view_state.update_dimensions(
                    image.width,
                    image.height,
                    renderer.surface_width(),
                    renderer.surface_height(),
                );
                self.view_state.fit_to_window();
                renderer.set_image(&image);
                renderer.update_transform(&self.view_state.transform());
                self.request_redraw();
            }
            Err(msg) => {
                log::error!("{msg}");
                if let Some(win) = &self.window {
                    win.set_title(&format!("Prvw - {msg}"));
                }
            }
        }
    }

    /// Display an image from the cache or load it fresh.
    fn display_cached_or_load(&mut self, index: usize, path: PathBuf) {
        let renderer = match &mut self.renderer {
            Some(r) => r,
            None => return,
        };

        if let Some(image) = self.image_cache.get(index) {
            self.view_state.update_dimensions(
                image.width,
                image.height,
                renderer.surface_width(),
                renderer.surface_height(),
            );
            self.view_state.fit_to_window();
            renderer.set_image(image);
            renderer.update_transform(&self.view_state.transform());
            self.request_redraw();
        } else {
            // Not cached yet: show "Loading..." and load directly
            if let Some(win) = &self.window {
                win.set_title("Loading...");
            }
            self.display_image(&path);
        }
    }

    fn navigate(&mut self, forward: bool) {
        let moved = if let Some(dir) = &mut self.dir_list {
            if forward {
                dir.go_next()
            } else {
                dir.go_prev()
            }
        } else {
            false
        };

        if !moved {
            return;
        }

        // Extract what we need from dir_list before mutable borrow
        let (current_path, current_index, total, preload_indices) = {
            let dir = self.dir_list.as_ref().unwrap();
            let indices = dir.preload_range(preloader::preload_count());
            (
                dir.current().to_path_buf(),
                dir.current_index(),
                dir.len(),
                indices,
            )
        };

        // Update window title
        if let Some(win) = &self.window {
            win.set_title(&window::window_title_with_position(
                &current_path,
                current_index,
                total,
            ));
        }

        // Display the current image
        self.display_cached_or_load(current_index, current_path);

        // Request preloading of adjacent images
        if let Some(dir) = &self.dir_list {
            let to_preload: Vec<(usize, PathBuf)> = preload_indices
                .iter()
                .filter(|&&i| !self.image_cache.contains(i))
                .filter_map(|&i| dir.get(i).map(|p| (i, p.to_path_buf())))
                .collect();

            if let Some(preloader) = &self.preloader
                && !to_preload.is_empty()
            {
                preloader.request_preload(to_preload);
            }
        }
    }

    fn update_transform_and_redraw(&mut self) {
        if let Some(renderer) = &self.renderer {
            renderer.update_transform(&self.view_state.transform());
        }
        self.request_redraw();
    }

    fn request_redraw(&mut self) {
        self.needs_redraw = true;
        if let Some(win) = &self.window {
            win.request_redraw();
        }
    }

    /// Drain preloader responses and cache the results.
    fn poll_preloader(&mut self) {
        let Some(preloader) = &self.preloader else {
            return;
        };
        while let Ok(response) = preloader.response_rx.try_recv() {
            match response {
                preloader::PreloadResponse::Ready { index, image } => {
                    log::debug!("Preloaded image at index {index}");
                    self.image_cache.insert(index, image);
                }
                preloader::PreloadResponse::Failed {
                    index,
                    path,
                    reason,
                } => {
                    log::warn!(
                        "Preload failed for index {index} ({}): {reason}",
                        path.display()
                    );
                }
            }
        }
    }

    fn handle_menu_event(&mut self, event_loop: &ActiveEventLoop) {
        let Some(ids) = &self.menu_ids else { return };
        let Some(event) = menu::poll_menu_event() else {
            return;
        };

        if event.id() == &ids.zoom_in {
            self.view_state.keyboard_zoom(true);
            self.update_transform_and_redraw();
        } else if event.id() == &ids.zoom_out {
            self.view_state.keyboard_zoom(false);
            self.update_transform_and_redraw();
        } else if event.id() == &ids.actual_size {
            self.view_state.actual_size();
            self.update_transform_and_redraw();
        } else if event.id() == &ids.fit_to_window {
            self.view_state.fit_to_window();
            self.update_transform_and_redraw();
        } else if event.id() == &ids.fullscreen {
            if let Some(win) = &self.window {
                window::toggle_fullscreen(win);
            }
        } else if event.id() == &ids.previous {
            self.navigate(false);
        } else if event.id() == &ids.next {
            self.navigate(true);
        } else {
            // Handle predefined menu items (Quit, Close)
            // muda handles Quit/Close automatically on macOS via NSApplication
            let _ = event_loop;
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return; // Already initialized
        }

        // Create window
        let win = window::create_window(event_loop, &self.file_path);
        self.window = Some(win.clone());

        // Create renderer (wgpu surface must be created here, in resumed())
        self.renderer = Some(renderer::Renderer::new(win));

        // Create native menu bar
        self.menu_ids = Some(menu::create_menu_bar());

        // Scan directory for image files
        self.dir_list = directory::DirectoryList::from_file(&self.file_path);

        // Start preloader thread
        let preloader = preloader::Preloader::start();

        // Load and display the initial image
        let initial_path = self.file_path.clone();
        self.display_image(&initial_path);

        // Cache the initial image and request preloading of adjacent images
        if let Some(dir) = &self.dir_list {
            let current_index = dir.current_index();
            let total = dir.len();

            // Update window title with position
            if let Some(win) = &self.window {
                win.set_title(&window::window_title_with_position(
                    &self.file_path,
                    current_index,
                    total,
                ));
            }

            // Request preloading of adjacent images
            let to_preload: Vec<(usize, PathBuf)> = dir
                .preload_range(preloader::preload_count())
                .iter()
                .filter_map(|&i| dir.get(i).map(|p| (i, p.to_path_buf())))
                .collect();

            if !to_preload.is_empty() {
                preloader.request_preload(to_preload);
            }
        }

        self.preloader = Some(preloader);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        // Poll preloader responses and menu events on every window event
        self.poll_preloader();
        self.handle_menu_event(event_loop);

        match event {
            WindowEvent::CloseRequested => {
                if let Some(preloader) = &self.preloader {
                    preloader.shutdown();
                }
                event_loop.exit();
            }

            WindowEvent::Resized(size) => {
                if let Some(renderer) = &mut self.renderer {
                    renderer.resize(size.width, size.height);
                    if let Some(dir) = &self.dir_list {
                        // Re-derive view dimensions for the current image
                        if let Some(image) = self.image_cache.get(dir.current_index()) {
                            self.view_state.update_dimensions(
                                image.width,
                                image.height,
                                size.width,
                                size.height,
                            );
                        }
                    }
                    renderer.update_transform(&self.view_state.transform());
                }
                self.request_redraw();
            }

            WindowEvent::RedrawRequested => {
                if self.needs_redraw {
                    if let Some(renderer) = &self.renderer {
                        renderer.render();
                    }
                    self.needs_redraw = false;
                }
            }

            WindowEvent::ModifiersChanged(mods) => {
                self.modifiers = mods.state();
            }

            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                let super_pressed = self.modifiers.super_key();
                match event.logical_key.as_ref() {
                    Key::Named(NamedKey::Escape) => {
                        if let Some(win) = &self.window {
                            if window::is_fullscreen(win) {
                                window::toggle_fullscreen(win);
                            } else {
                                if let Some(preloader) = &self.preloader {
                                    preloader.shutdown();
                                }
                                event_loop.exit();
                            }
                        }
                    }
                    Key::Named(NamedKey::ArrowLeft) => self.navigate(false),
                    Key::Named(NamedKey::ArrowRight) => self.navigate(true),
                    Key::Named(NamedKey::F11) => {
                        if let Some(win) = &self.window {
                            window::toggle_fullscreen(win);
                        }
                    }
                    Key::Character("f") if super_pressed => {
                        if let Some(win) = &self.window {
                            window::toggle_fullscreen(win);
                        }
                    }
                    Key::Character("=") | Key::Character("+") if super_pressed => {
                        self.view_state.keyboard_zoom(true);
                        self.update_transform_and_redraw();
                    }
                    Key::Character("-") if super_pressed => {
                        self.view_state.keyboard_zoom(false);
                        self.update_transform_and_redraw();
                    }
                    Key::Character("=") | Key::Character("+") => {
                        self.view_state.keyboard_zoom(true);
                        self.update_transform_and_redraw();
                    }
                    Key::Character("-") => {
                        self.view_state.keyboard_zoom(false);
                        self.update_transform_and_redraw();
                    }
                    Key::Character("0") => {
                        self.view_state.fit_to_window();
                        self.update_transform_and_redraw();
                    }
                    Key::Character("1") if super_pressed => {
                        self.view_state.actual_size();
                        self.update_transform_and_redraw();
                    }
                    _ => {}
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                let scroll_y = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(pos) => pos.y as f32 / 50.0,
                };
                if scroll_y.abs() > f32::EPSILON {
                    let (cx, cy) = self.last_mouse_pos;
                    self.view_state.scroll_zoom(scroll_y, cx as f32, cy as f32);
                    self.update_transform_and_redraw();
                }
            }

            WindowEvent::CursorMoved { position, .. } => {
                let prev = self.last_mouse_pos;
                self.last_mouse_pos = (position.x, position.y);

                if self.drag_start.is_some() {
                    let dx = position.x - prev.0;
                    let dy = position.y - prev.1;
                    self.view_state.pan(dx as f32, dy as f32);
                    self.update_transform_and_redraw();
                }
            }

            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => match state {
                ElementState::Pressed => {
                    let now = Instant::now();
                    if let Some(last) = self.last_click_time
                        && now.duration_since(last).as_millis() < 400
                    {
                        // Double-click: toggle fit-to-window vs actual size
                        self.view_state.toggle_fit();
                        self.update_transform_and_redraw();
                        self.last_click_time = None;
                        self.drag_start = None;
                        return;
                    }
                    self.last_click_time = Some(now);
                    self.drag_start = Some(self.last_mouse_pos);
                }
                ElementState::Released => {
                    self.drag_start = None;
                }
            },

            _ => {}
        }
    }
}
