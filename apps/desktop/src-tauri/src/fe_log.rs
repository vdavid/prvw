use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Warning,
    Error,
}

#[derive(Deserialize)]
pub struct FrontendLogEntry {
    pub level: LogLevel,
    pub category: String,
    pub message: String,
}

/// Receives batched log entries from the frontend and re-emits them through the Rust `log` facade.
/// This ensures frontend logs appear in the terminal alongside Rust logs, prefixed with `FE:`.
#[tauri::command]
pub fn batch_fe_logs(entries: Vec<FrontendLogEntry>) {
    for entry in &entries {
        let target = format!("FE:{}", entry.category);
        match entry.level {
            LogLevel::Debug => log::debug!(target: &target, "{}", entry.message),
            LogLevel::Info => log::info!(target: &target, "{}", entry.message),
            LogLevel::Warn | LogLevel::Warning => log::warn!(target: &target, "{}", entry.message),
            LogLevel::Error => log::error!(target: &target, "{}", entry.message),
        }
    }
}
