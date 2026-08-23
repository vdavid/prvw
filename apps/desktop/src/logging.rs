//! Logger setup, and the question of where a Windows build's output can go.
//!
//! On macOS and Linux this is `env_logger` writing to stderr, and `RUST_LOG` picks the level.
//!
//! Windows is the interesting one. `prvw.exe` is a GUI-subsystem binary (see the
//! `windows_subsystem` attribute in `main.rs`), so double-clicking it doesn't flash up a console
//! window, and the flip side is that it starts with no stderr at all. So we look, in order:
//!
//! 1. **stderr already goes somewhere** (a terminal that ran `cargo run`, a shell redirect, the
//!    E2E harness's pipe): write there and touch nothing.
//! 2. **A parent console exists** (`prvw.exe` typed into PowerShell, which doesn't hand a GUI app
//!    its handles): attach to it and write there.
//! 3. **Neither** (Explorer, the Start menu, a taskbar pin): write to `prvw.log` in the app data
//!    directory, so a bug report has something to attach.
//!
//! ANSI colors go on only where they'll render: a real console with virtual-terminal processing.
//! A pipe or a log file gets plain text.

use std::io::Write;

/// Where log lines end up. `env_logger` needs to know before the first line is written.
enum Sink {
    Stderr {
        colors: bool,
    },
    /// Only a Windows launch with no console ever reaches this.
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    File(std::fs::File),
}

/// Install the global logger. Call once, first thing in `main`.
pub fn init() {
    let sink = pick_sink();
    let colors = matches!(sink, Sink::Stderr { colors: true });

    let mut builder =
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"));
    builder
        .filter_module("wgpu", log::LevelFilter::Warn)
        .filter_module("wgpu_core", log::LevelFilter::Warn)
        .filter_module("wgpu_hal", log::LevelFilter::Warn)
        .filter_module("naga", log::LevelFilter::Warn)
        .filter_module("muda", log::LevelFilter::Warn)
        // rawler emits best-effort WARNs the user can't act on: "Decoder has no
        // preview image support" (our quick-preview probes every RAW for one),
        // lens-DB match misses, and TIFF-parse noise on exotic files. Show only
        // errors. Our own `decoding::*` logs are a different target, unaffected.
        .filter_module("rawler", log::LevelFilter::Error)
        .format(move |buf, record| {
            let now = chrono::Local::now();
            let ts = now.format("%H:%M:%S%.3f");
            let target = record
                .target()
                .strip_prefix("prvw::")
                .unwrap_or(record.target());
            let level = record.level();
            let (color, reset) = if colors {
                let color = match level {
                    log::Level::Error => "\x1b[31m",
                    log::Level::Warn => "\x1b[33m",
                    log::Level::Info => "\x1b[32m",
                    log::Level::Debug => "\x1b[36m",
                    log::Level::Trace => "\x1b[35m",
                };
                (color, "\x1b[0m")
            } else {
                ("", "")
            };
            writeln!(
                buf,
                "{ts} {color}{level:<5}{reset} {target:<16} {}",
                record.args()
            )
        });

    if let Sink::File(file) = sink {
        builder.target(env_logger::Target::Pipe(Box::new(file)));
    }
    builder.init();
}

#[cfg(not(target_os = "windows"))]
fn pick_sink() -> Sink {
    Sink::Stderr { colors: true }
}

#[cfg(target_os = "windows")]
fn pick_sink() -> Sink {
    match crate::platform::windows::connect_stderr() {
        Some(colors) => Sink::Stderr { colors },
        None => match open_log_file() {
            Some(file) => Sink::File(file),
            // Nothing left to try. The writes go nowhere, which is what happens today anyway.
            None => Sink::Stderr { colors: false },
        },
    }
}

/// The log file for a launch with no console: `prvw.log` beside the settings.
///
/// It's opened for appending so two windows opened at once don't cut each other off, and reset
/// when it gets big, since nothing else ever prunes it.
#[cfg(target_os = "windows")]
fn open_log_file() -> Option<std::fs::File> {
    /// Past this, the next launch starts the file over. Roughly a thousand runs at `info`.
    const MAX_BYTES: u64 = 1024 * 1024;

    let dir = crate::settings::persistence::data_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join("prvw.log");
    let too_big = std::fs::metadata(&path).is_ok_and(|meta| meta.len() > MAX_BYTES);

    let mut options = std::fs::OpenOptions::new();
    options.create(true);
    if too_big {
        options.write(true).truncate(true);
    } else {
        options.append(true);
    }
    let mut file = options.open(&path).ok()?;
    let _ = writeln!(file, "--- prvw {} ---", env!("CARGO_PKG_VERSION"));
    Some(file)
}
