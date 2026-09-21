//! Console and file logging in the shape a Minecraft server uses.
//!
//! Every line is `[HH:MM:SS] [Server thread/LEVEL]: message`, the same layout
//! vanilla prints, with the level coloured so a warning or an error stands out
//! in a busy console. The console gets ANSI colours; `logs/latest.log` gets the
//! same lines without them, because a file full of escape codes is useless.
//!
//! Events may carry two extra fields to influence presentation:
//!
//! * `component` — replaces `Server thread`, for work that is not the main loop.
//! * `style` — one of `join`, `leave`, `banner`, `chat`, which colours the
//!   message text itself so player traffic is distinguishable at a glance.
//!
//! Unknown fields are appended as ` key=value`, dimmed, so the structured
//! context the code already logs stays visible without drowning the message.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

use chrono::Local;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields, MakeWriter};
use tracing_subscriber::prelude::*;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// Colour published by the terminal for each level. Green for the ordinary
/// chatter, amber for anything the operator should look at, red for failures.
fn level_colour(level: &Level) -> &'static str {
    match *level {
        Level::ERROR => "\x1b[1;31m",
        Level::WARN => "\x1b[1;33m",
        Level::INFO => "\x1b[1;32m",
        Level::DEBUG => "\x1b[1;34m",
        Level::TRACE => "\x1b[90m",
    }
}

/// A message colour a caller can ask for, so different *kinds* of line are
/// distinguishable even when they share a level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Join,
    Leave,
    Banner,
    Chat,
}

impl Style {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "join" => Some(Style::Join),
            "leave" => Some(Style::Leave),
            "banner" => Some(Style::Banner),
            "chat" => Some(Style::Chat),
            _ => None,
        }
    }

    fn colour(self) -> &'static str {
        match self {
            Style::Join => "\x1b[1;32m",
            Style::Leave => "\x1b[1;33m",
            Style::Banner => "\x1b[1;36m",
            Style::Chat => "\x1b[1;37m",
        }
    }
}

/// What the formatter needs out of an event.
#[derive(Default)]
struct Fields {
    message: String,
    component: Option<String>,
    style: Option<Style>,
    extras: Vec<(String, String)>,
}

impl Fields {
    fn push(&mut self, field: &Field, value: String) {
        match field.name() {
            "message" => self.message = value,
            "component" => self.component = Some(value),
            "style" => self.style = Style::parse(&value),
            name => self.extras.push((name.to_string(), value)),
        }
    }
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.push(field, value.to_string());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.push(field, format!("{value:?}"));
    }
}

/// The `[time] [component/LEVEL]: message` layout.
pub struct VanillaFormat {
    ansi: bool,
}

impl VanillaFormat {
    pub fn new(ansi: bool) -> Self {
        Self { ansi }
    }
}

impl<S, N> FormatEvent<S, N> for VanillaFormat
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        _ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let metadata = event.metadata();
        let level = *metadata.level();
        let mut fields = Fields::default();
        event.record(&mut fields);

        let component = fields.component.as_deref().unwrap_or("Server thread");
        let timestamp = Local::now().format("%H:%M:%S");

        if self.ansi {
            write!(writer, "\x1b[90m[{timestamp}]\x1b[0m ")?;
            write!(writer, "\x1b[37m[{component}/\x1b[0m")?;
            write!(writer, "{}{level}\x1b[0m", level_colour(&level))?;
            write!(writer, "\x1b[37m]\x1b[0m: ")?;
        } else {
            write!(writer, "[{timestamp}] [{component}/{level}]: ")?;
        }

        match fields.style {
            Some(style) if self.ansi => {
                write!(writer, "{}{}\x1b[0m", style.colour(), fields.message)?;
            }
            _ => write!(writer, "{}", fields.message)?,
        }

        for (key, value) in &fields.extras {
            if self.ansi {
                write!(writer, " \x1b[90m{key}={value}\x1b[0m")?;
            } else {
                write!(writer, " {key}={value}")?;
            }
        }
        writeln!(writer)
    }
}

/// Owned writer handle so the file can be shared between the subscriber and the
/// shutdown path, which flushes it one last time.
pub struct LogFile {
    file: Arc<Mutex<File>>,
}

impl Clone for LogFile {
    fn clone(&self) -> Self {
        Self {
            file: Arc::clone(&self.file),
        }
    }
}

impl LogFile {
    /// A writer that swallows everything, for when the log file cannot be
    /// opened: losing the log is not a reason to refuse to run.
    pub fn sink() -> Self {
        let devnull = if cfg!(windows) { "NUL" } else { "/dev/null" };
        let file = OpenOptions::new()
            .write(true)
            .open(devnull)
            .unwrap_or_else(|_| {
                File::create(std::env::temp_dir().join("loadstone-sink.log"))
                    .expect("a sink to log to")
            });
        Self {
            file: Arc::new(Mutex::new(file)),
        }
    }

    pub fn flush(&self) {
        if let Ok(mut file) = self.file.lock() {
            let _ = file.flush();
        }
    }
}

impl io::Write for LogFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.file.lock() {
            Ok(mut file) => file.write(buf),
            Err(_) => Ok(buf.len()),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.file.lock() {
            Ok(mut file) => file.flush(),
            Err(_) => Ok(()),
        }
    }
}

/// Per-write handle for `MakeWriter`. It re-locks per write instead of handing
/// out a guard, because a `MutexGuard` cannot outlive the borrow `make_writer`
/// is given.
pub struct LogFileHandle {
    file: Arc<Mutex<File>>,
}

impl io::Write for LogFileHandle {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.file.lock() {
            Ok(mut file) => file.write(buf),
            Err(_) => Ok(buf.len()),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.file.lock() {
            Ok(mut file) => file.flush(),
            Err(_) => Ok(()),
        }
    }
}

impl<'a> MakeWriter<'a> for LogFile {
    type Writer = LogFileHandle;

    fn make_writer(&'a self) -> Self::Writer {
        LogFileHandle {
            file: Arc::clone(&self.file),
        }
    }
}

/// Create `logs/`, roll the previous `latest.log` aside, and open a new one.
///
/// Rotation renames rather than compresses: the file is plain text either way,
/// and a `.log` next to the current one is easier to grep than a gzip.
pub fn open_log_file(logs_dir: &Path) -> io::Result<LogFile> {
    fs::create_dir_all(logs_dir)?;
    let latest = logs_dir.join("latest.log");
    if latest.exists() {
        let stamp = Local::now().format("%Y-%m-%d");
        let mut index = 1;
        loop {
            let rolled = logs_dir.join(format!("{stamp}-{index}.log"));
            if !rolled.exists() {
                let _ = fs::rename(&latest, &rolled);
                break;
            }
            index += 1;
        }
    }
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&latest)?;
    Ok(LogFile {
        file: Arc::new(Mutex::new(file)),
    })
}

/// Install the console and file layers, returning the file handle so the
/// shutdown path can flush it.
///
/// `ansi` is decided by the caller rather than probed here, because a panel
/// renders escape codes even when the process has no terminal attached.
pub fn init(log_file: LogFile, ansi: bool, filter: EnvFilter) -> LogFile {
    let console = tracing_subscriber::fmt::layer()
        .event_format(VanillaFormat::new(ansi))
        .with_writer(io::stdout)
        .with_filter(filter.clone());

    let file_log = tracing_subscriber::fmt::layer()
        .event_format(VanillaFormat::new(false))
        .with_ansi(false)
        .with_writer(log_file.clone())
        .with_filter(filter);

    tracing_subscriber::registry()
        .with(console)
        .with(file_log)
        .init();

    log_file
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_map_to_distinct_colours() {
        let colours: Vec<&str> = [
            Level::ERROR,
            Level::WARN,
            Level::INFO,
            Level::DEBUG,
            Level::TRACE,
        ]
        .iter()
        .map(level_colour)
        .collect();
        let unique: std::collections::HashSet<_> = colours.iter().collect();
        assert_eq!(
            unique.len(),
            colours.len(),
            "each level needs its own colour"
        );
        assert_eq!(level_colour(&Level::ERROR), "\x1b[1;31m");
        assert_eq!(level_colour(&Level::WARN), "\x1b[1;33m");
    }

    #[test]
    fn styles_round_trip_from_their_field_values() {
        assert_eq!(Style::parse("join"), Some(Style::Join));
        assert_eq!(Style::parse("leave"), Some(Style::Leave));
        assert_eq!(Style::parse("banner"), Some(Style::Banner));
        assert_eq!(Style::parse("chat"), Some(Style::Chat));
        assert_eq!(Style::parse("nonsense"), None);
    }

    #[test]
    fn rotation_keeps_the_previous_log() {
        let dir = std::env::temp_dir().join(format!("loadstone-logs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);

        let first = open_log_file(&dir).unwrap();
        let mut handle = first.clone();
        writeln!(handle, "first run").unwrap();
        first.flush();

        let second = open_log_file(&dir).unwrap();
        let mut handle = second.clone();
        writeln!(handle, "second run").unwrap();
        second.flush();

        let latest = fs::read_to_string(dir.join("latest.log")).unwrap();
        assert!(latest.contains("second run"));

        let rolled: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.ends_with(".log") && name != "latest.log")
            .collect();
        assert_eq!(
            rolled.len(),
            1,
            "the previous log should be kept: {rolled:?}"
        );
        let kept = fs::read_to_string(dir.join(&rolled[0])).unwrap();
        assert!(kept.contains("first run"));
    }
}
