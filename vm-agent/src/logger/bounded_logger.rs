use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use log::{Record, Metadata, Level, LevelFilter, SetLoggerError};
use vsock::VsockStream;
use std::io::Write;

/// The number of log messages to buffer before blocking/dropping.
const LOG_CHANNEL_CAPACITY: usize = 1000;

/// Logger that sends log messages to a bounded async channel.
/// A background task reads from the channel and writes to the vsock stream.
pub struct BoundedVsockLogger {
    sender: mpsc::Sender<String>,
}

impl BoundedVsockLogger {
    /// Initializes the logger and spawns the background task.
    pub async fn init(port: u32) -> Arc<Self> {
        let (tx, mut rx) = mpsc::channel::<String>(LOG_CHANNEL_CAPACITY);

        // Connect to the vsock log listener
        let vsock_stream = Arc::new(Mutex::new(
            loop {
                match VsockStream::connect_with_cid_port(vsock::VMADDR_CID_HOST, port) {
                    Ok(stream) => break stream,
                    Err(e) => {
                        eprintln!(
                            "Logger: failed to connect to log listener on port {} ({}), retrying in 1s...",
                            port, e
                        );
                        std::thread::sleep(std::time::Duration::from_secs(1));
                    }
                }
            }
        ));

        // Spawn background task for writing logs
        let vsock_stream_clone = vsock_stream.clone();
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                let mut stream = vsock_stream_clone.lock().await;
                let _ = stream.write_all(msg.as_bytes());
                let _ = stream.flush();
            }
        });

        Arc::new(Self { sender: tx })
    }

    /// Enqueue a log message. If the channel is full, the message is dropped.
    pub fn enqueue(&self, msg: String) {
        // It's okay to drop logs if the channel is full to avoid OOM.
        let _ = self.sender.try_send(msg);
    }
}

/// Combined logger that logs to both console and vsock (via bounded channel).
pub struct CombinedLogger {
    vsock_logger: Arc<BoundedVsockLogger>,
    level: log::LevelFilter,
}

impl CombinedLogger {
    pub fn new(vsock_logger: Arc<BoundedVsockLogger>, level: log::LevelFilter) -> Self {
        Self { vsock_logger, level }
    }
}

impl log::Log for CombinedLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            let msg = format!("{} - {}\n", record.level(), record.args());
            // Log to console
            print!("{}", msg);

            // Log to vsock (enqueue, non-blocking, may drop if full)
            self.vsock_logger.enqueue(msg);
        }
    }

    fn flush(&self) {}
}

/// Initializes the global logger with the bounded vsock logger.
pub fn init_combined_logger(vsock_logger: Arc<BoundedVsockLogger>, log_level: LevelFilter) -> Result<(), SetLoggerError> {
    let logger = Box::leak(Box::new(CombinedLogger::new(vsock_logger, log_level)));
    log::set_logger(logger)?;
    log::set_max_level(log_level);
    Ok(())
}
