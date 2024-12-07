use crate::errors::DeviceSysLogError;
use crate::{device::DeviceClient, devices::SingleDevice};
use regex::Regex;
use rusty_libimobiledevice::service::ServiceClient;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;
use std::{fmt, fs};

/// Enum for controlling logging behavior
#[derive(Debug, Clone)]
pub enum LoggerCommand {
    StartLogging,
    StopLogging,
}

/// Struct to store parsed log data
#[derive(Debug, Default)]
struct LogsData<'a> {
    date: &'a str,
    device: &'a str,
    process: &'a str,
    pid: Option<&'a str>,
    severity: Option<&'a str>,
    message: &'a str,
}

fn get_parsed_log(log: &LogsData) -> String {
    format!(
        "[{}] {} {} [{}] <{}>: {}",
        log.date,
        log.device,
        log.process,
        log.pid.unwrap_or("None"),
        log.severity.unwrap_or("None"),
        log.message
    )
}

fn get_parsed_log_colored(log: &LogsData) -> String {
    format!(
        "[\x1b[34m{}\x1b[0m] \x1b[32m{}\x1b[0m \x1b[36m{}\x1b[0m [{}] <\x1b[31m{}\x1b[0m>: \x1b[37m{}\x1b[0m",
        log.date,
        log.device,
        log.process,
        log.pid.unwrap_or("None"),
        log.severity.unwrap_or("None"),
        log.message
    )
}
fn process_log_line<'a>(line: &'a str, log_regex: &Regex) -> Option<LogsData<'a>> {
    log_regex.captures(line).map(|captures| LogsData {
        date: captures.name("date").unwrap().as_str(),
        device: captures.name("device").unwrap().as_str(),
        process: captures.name("process").unwrap().as_str(),
        pid: captures.name("pid").map(|m| m.as_str()), // Optional
        severity: captures.name("severity").map(|m| m.as_str()), // Optional
        message: captures.name("message").unwrap().as_str(),
    })
}

fn process_logs(line: &str) -> LogsData<'_> {
    let log_regex = Regex::new(r"^(?P<date>\w{3}\s+\d{1,2}\s+\d{2}:\d{2}:\d{2})\s+(?P<device>\S+)\s+(?P<process>[^\[\(<]+(?:\([^\)]+\))?)(?:\[(?P<pid>\d+)\])?\s*(?:<(?P<severity>\w+)>:\s*)?(?P<message>.+)$").unwrap();

    process_log_line(line, &log_regex).unwrap_or_default()
}

pub struct DeviceSysLog<T> {
    devices: Arc<DeviceClient<T>>,
    sender: mpsc::Sender<LoggerCommand>,
    receiver: Arc<Mutex<mpsc::Receiver<LoggerCommand>>>,
    _phantom: std::marker::PhantomData<T>,
}

impl<T> DeviceSysLog<T> {
    pub fn new(devices: DeviceClient<T>) -> DeviceSysLog<T> {
        let (tx, rx) = mpsc::channel();
        DeviceSysLog {
            devices: Arc::new(devices),
            sender: tx,
            receiver: Arc::new(Mutex::new(rx)),
            _phantom: std::marker::PhantomData::<T>,
        }
    }
}

impl DeviceSysLog<SingleDevice> {
    /// Starts the logger service on a new thread
    fn _start_service<F>(&self, callback: F)
    where
        F: Fn(LogsData) + 'static + Sync + Send,
    {
        let devices_clone = Arc::clone(&self.devices);
        let receiver_clone = Arc::clone(&self.receiver);

        // Spawn a new thread to handle logging at the background
        thread::spawn(move || {
            let mut current_status: LoggerCommand = LoggerCommand::StopLogging;

            let device = devices_clone.get_device().unwrap();
            let mut lockdown = devices_clone.get_lockdown_client().unwrap();
            let lockdown_service = lockdown
                .start_service("com.apple.syslog_relay", true)
                .unwrap();
            let service = ServiceClient::new(device, lockdown_service).unwrap();

            loop {
                // Listen for commands to start/stop logging

                let reciver = receiver_clone.lock();

                if let Err(err) = reciver {
                    eprint!("Error: {:?}", err);
                    continue;
                }
                let reciver = reciver.unwrap();

                if let Ok(command) = reciver.try_recv() {
                    current_status = command;
                }

                match current_status {
                    LoggerCommand::StartLogging => match service.receive(1024) {
                        Ok(data) => {
                            let logs_raw_string = String::from_utf8_lossy(&data);

                            for line in logs_raw_string.split_terminator('\n') {
                                let line = line.trim_matches('\0'); // Remove null characters
                                callback(process_logs(line));
                            }
                        }
                        Err(err) => {
                            eprintln!("Failed to receive data: {}", err);
                            thread::sleep(Duration::from_secs(1));
                        }
                    },
                    LoggerCommand::StopLogging => break,
                }
            }
        });
    }

    pub fn log_to_stdout(&self) {
        self.sender
            .send(LoggerCommand::StartLogging)
            .unwrap_or_default();
        self._start_service(|logs| println!("{}", get_parsed_log_colored(&logs)));
    }

    pub fn log_to_file<S>(&self, file_path: &S) -> Result<(), DeviceSysLogError>
    where
        S: AsRef<Path> + ?Sized + Sync,
    {
        self.sender.send(LoggerCommand::StartLogging)?;
        let file_path = file_path.as_ref().to_path_buf();

        self._start_service(move |logs| {
            let parsed_logs = get_parsed_log(&logs);

            // resolved path, just in case
            let resolved_path = fs::canonicalize(&file_path).unwrap_or_default();

            let mut file = match OpenOptions::new()
                .append(true)
                .create(true)
                .open(resolved_path)
            {
                Ok(file) => file,
                Err(_) => {
                    // Fallback to temp.log if the file cannot be opened or created
                    eprintln!("Failed to open log file, using default temp.log");
                    File::create("temp.log").unwrap()
                }
            };

            if let Err(e) = file.write_all(parsed_logs.as_bytes()) {
                eprintln!("Error writing to file: {}", e);
            }

            if let Err(e) = file.flush() {
                eprintln!("Error flushing to file: {}", e);
            }
        });
        Ok(())
    }

    pub fn stop_logging(&self) {
        self.sender.send(LoggerCommand::StopLogging).unwrap();
    }
}

impl fmt::Debug for DeviceSysLog<SingleDevice> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceSysLog")
            .field("devices", &self.devices)
            .finish()
    }
}
