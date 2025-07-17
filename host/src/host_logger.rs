use log::{Level, LevelFilter, Metadata, Record, SetLoggerError};
use std::env;
use std::io::{self, Write};

pub struct HostLogger;

impl log::Log for HostLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        // Enable all log levels (customize as needed)
        metadata.level() <= Level::Info
            || metadata.level() == Level::Error
            || metadata.level() == Level::Warn
            || metadata.level() == Level::Debug
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            // Determine origin
            let module_path = record.module_path().unwrap_or("");
            let target = record.target();

            // Adjust this to match your crate root/module
            let is_host = module_path.starts_with("hyperlight_agents")
                || target.starts_with("hyperlight_agents");

            let msg_str = format!("{}", record.args());
            let already_prefixed = msg_str.starts_with('[');

            let msg = if already_prefixed {
                format!("{}\n", msg_str)
            } else {
                let prefix = if is_host {
                    "host".to_string()
                } else {
                    // Use the crate name (first segment of target)
                    target.split("::").next().unwrap_or(target).to_string()
                };
                format!("[{}] {}\n", prefix, msg_str)
            };

            match record.level() {
                Level::Error => {
                    let _ = io::stderr().write_all(msg.as_bytes());
                }
                _ => {
                    let _ = io::stdout().write_all(msg.as_bytes());
                }
            }
        }
    }

    fn flush(&self) {}
}

static LOGGER: HostLogger = HostLogger;

pub fn init_logger() {
    env_logger::init();
}
