//! Process-wide operational diagnostics. Language diagnostics remain independent.
//!
//! Only first-party targets are accepted, even with RUST_LOG. Records are bounded,
//! single-line, and redacted at the final formatting boundary. Hosts own the sink.
use regex::Regex;
use std::{
    collections::BTreeSet,
    fmt::{self, Write as _},
    sync::{
        LazyLock, OnceLock, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};
use tracing::{
    Event, Subscriber,
    field::{Field, Visit},
    level_filters::LevelFilter,
};
use tracing_subscriber::{
    Layer,
    fmt::{FmtContext, FormatEvent, FormatFields, format::Writer},
    layer::SubscriberExt,
    registry::LookupSpan,
};

pub const MAX_VERBOSITY: u8 = 3;
const MAX_RECORD_BYTES: usize = 8192;
static CAPTURE_ACQUIRED_SECRETS: AtomicBool = AtomicBool::new(false);
static SECRETS: LazyLock<RwLock<BTreeSet<String>>> = LazyLock::new(|| RwLock::new(BTreeSet::new()));
static URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)[a-z][a-z0-9+.-]*://[^\s<>\"']+"#).unwrap());
static CREDENTIAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(authorization|proxy-authorization|cookie|set-cookie|[\w.-]*(?:token|password|passwd|secret|api[_-]?key|private[_-]?key|credential)[\w.-]*)["']?\s*[=:]\s*(?:\"[^\"]*\"|'[^']*'|[^,;\r\n]+)"#).unwrap()
});

/// Register credentials at acquisition boundaries, never at individual log sites.
pub fn register_secret(secret: &str) {
    if !secret.is_empty() {
        let mut secrets = SECRETS.write().unwrap_or_else(|e| e.into_inner());
        secrets.insert(secret.to_owned());
        let escaped = format!("{secret:?}");
        secrets.insert(escaped[1..escaped.len() - 1].to_owned());
    }
}

/// Credential libraries retain no extra plaintext unless the executable opted in.
pub fn register_acquired_secret(secret: &str) {
    if CAPTURE_ACQUIRED_SECRETS.load(Ordering::Relaxed) {
        register_secret(secret);
    }
}

/// Sanitize diagnostic text, including errors from HTTP clients and subprocesses.
/// This is deliberately not applied to a Fidan program's own stdout.
pub fn redact(text: &str) -> String {
    let secrets = SECRETS.read().unwrap_or_else(|e| e.into_inner());
    let mut text = text.to_owned();
    // Longest first so overlapping credentials cannot expose suffixes.
    let mut ordered: Vec<_> = secrets.iter().collect();
    ordered.sort_by_key(|s| std::cmp::Reverse(s.len()));
    for secret in ordered {
        text = text.replace(secret, "[REDACTED]");
    }
    drop(secrets);
    text = URL
        .replace_all(&text, |captures: &regex::Captures<'_>| {
            let raw = &captures[0];
            let (scheme, rest) = raw.split_once("://").unwrap();
            let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
            let authority = rest[..end].rsplit('@').next().unwrap_or("");
            // Paths can contain bearer credentials as well as query strings. Log only origin.
            format!("{scheme}://{authority}/[REDACTED]")
        })
        .into_owned();
    CREDENTIAL.replace_all(&text, "$1=[REDACTED]").into_owned()
}

pub fn capture_environment_secrets() {
    CAPTURE_ACQUIRED_SECRETS.store(true, Ordering::Relaxed);
    for (name, value) in std::env::vars_os() {
        let name = name.to_string_lossy().to_ascii_lowercase();
        if [
            "token",
            "password",
            "passwd",
            "secret",
            "api_key",
            "apikey",
            "private_key",
            "credential",
            "database_url",
        ]
        .iter()
        .any(|part| name.contains(part))
        {
            register_secret(&value.to_string_lossy());
        }
    }
}

#[derive(Clone, Debug)]
pub struct Configuration {
    levels: Vec<(String, LevelFilter)>,
}
impl Configuration {
    /// Environment lookup is explicit; embedded consumers can use `new` instead.
    pub fn from_environment(verbosity: u8) -> Result<Self, String> {
        let environment = if verbosity == 0 {
            filter_environment("RUST_LOG")?
        } else {
            None
        };
        Self::new(verbosity, environment.as_deref())
    }

    /// Explicit verbosity wins over RUST_LOG. With neither, logging stays off.
    /// RUST_LOG accepts a level or comma-separated first-party target=level pairs.
    pub fn new(verbosity: u8, environment: Option<&str>) -> Result<Self, String> {
        if verbosity > MAX_VERBOSITY {
            return Err("at most three verbosity occurrences are supported (-vvv)".into());
        }
        let level = match verbosity {
            1 => LevelFilter::INFO,
            2 => LevelFilter::DEBUG,
            3 => LevelFilter::TRACE,
            _ => LevelFilter::OFF,
        };
        let mut levels = vec![("fidan".into(), level)];
        if verbosity == 0
            && let Some(environment) = environment.filter(|s| !s.trim().is_empty())
        {
            for directive in environment.split(',') {
                let (target, level) = directive
                    .trim()
                    .split_once('=')
                    .unwrap_or(("fidan", directive.trim()));
                if !is_fidan_target(target)
                    || !target.chars().all(|character| {
                        character.is_ascii_alphanumeric() || matches!(character, '_' | ':')
                    })
                {
                    return Err("RUST_LOG accepts only Fidan targets (fidan or fidan_*), without field/regex filters".into());
                }
                let level = level.parse::<LevelFilter>().map_err(|_| {
                    "RUST_LOG levels: off, error, warn, info, debug, trace".to_string()
                })?;
                levels.retain(|(existing, _)| existing != target);
                levels.push((target.to_owned(), level));
            }
        }
        levels.sort_by_key(|(target, _)| std::cmp::Reverse(target.len()));
        Ok(Self { levels })
    }
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        is_fidan_target(metadata.target())
            && self
                .levels
                .iter()
                .find(|(target, _)| metadata.target().starts_with(target))
                .is_some_and(|(_, level)| *metadata.level() <= *level)
    }
}
fn is_fidan_target(target: &str) -> bool {
    target == "fidan" || target.starts_with("fidan::") || target.starts_with("fidan_")
}

/// Build a subscriber without installing it. Executables/embedded hosts own the sink
/// and decide whether to install globally or use a scoped dispatcher.
pub fn subscriber<W>(configuration: Configuration, writer: W) -> impl Subscriber + Send + Sync
where
    W: for<'a> tracing_subscriber::fmt::MakeWriter<'a> + Send + Sync + 'static,
{
    let layer = tracing_subscriber::fmt::layer()
        .event_format(SafeEvent)
        .with_ansi(false)
        .with_writer(writer)
        .with_filter(tracing_subscriber::filter::filter_fn(move |metadata| {
            configuration.enabled(metadata)
        }));
    tracing_subscriber::registry().with(layer)
}

const CHILD_FILTER_ENV: &str = "FIDAN_INTERNAL_DIAGNOSTICS";
static CHILD_FILTER: OnceLock<String> = OnceLock::new();
/// Called explicitly by the CLI after its subscriber was installed successfully.
pub fn configure_children(configuration: &Configuration) -> Result<(), String> {
    let directives = configuration
        .levels
        .iter()
        .map(|(target, level)| format!("{target}={level}"))
        .collect::<Vec<_>>()
        .join(",");
    CHILD_FILTER
        .set(directives)
        .map_err(|_| "child diagnostics configuration already installed".to_string())
}
pub fn configure_child(command: &mut std::process::Command) {
    if let Some(filter) = CHILD_FILTER.get() {
        command.env(CHILD_FILTER_ENV, filter);
    }
}
pub fn child_configuration() -> Result<Configuration, String> {
    Configuration::new(0, filter_environment(CHILD_FILTER_ENV)?.as_deref())
}

fn filter_environment(name: &str) -> Result<Option<String>, String> {
    match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(format!("{name} must be valid Unicode")),
    }
}

struct BoundedText {
    text: String,
    oversized: bool,
}
impl fmt::Write for BoundedText {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        let remaining = MAX_RECORD_BYTES.saturating_sub(self.text.len());
        let mut end = remaining.min(value.len());
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        if end < value.len() {
            self.oversized = true;
        }
        self.text.push_str(&value[..end]);
        Ok(())
    }
}
struct Fields<'a>(&'a mut BoundedText);
impl Visit for Fields<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        let _ = write!(self.0, " {}={value:?}", field.name());
    }
}
struct SafeEvent;
impl<S, N> FormatEvent<S, N> for SafeEvent
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let mut text = BoundedText {
            text: String::new(),
            oversized: false,
        };
        write!(
            text,
            "{} {}",
            event.metadata().level(),
            event.metadata().target()
        )?;
        if let Some(scope) = ctx.event_scope() {
            for span in scope.from_root() {
                write!(text, "/{}", span.name())?;
            }
        }
        event.record(&mut Fields(&mut text));
        let sanitized = if text.oversized {
            format!(
                "{} {} record omitted: exceeds diagnostic size limit",
                event.metadata().level(),
                event.metadata().target()
            )
        } else {
            redact(&text.text)
        };
        let sanitized = if sanitized.len() > MAX_RECORD_BYTES {
            format!(
                "{} {} record omitted: exceeds diagnostic size limit after redaction",
                event.metadata().level(),
                event.metadata().target()
            )
        } else {
            sanitized
        };
        // Prevent terminal control injection and forged multi-line records.
        for character in sanitized.chars() {
            if character.is_control() {
                write!(writer, "{}", character.escape_default())?;
            } else {
                write!(writer, "{character}")?;
            }
        }
        writeln!(writer)
    }
}

/// Coarse phase timing only; no work is added to AST/VM instruction hot loops.
pub fn phase<T>(name: &'static str, operation: impl FnOnce() -> T) -> T {
    let _span = tracing::debug_span!("phase", phase = name).entered();
    tracing::debug!(phase = name, "phase started");
    let start = tracing::enabled!(tracing::Level::TRACE).then(Instant::now);
    let result = operation();
    if let Some(start) = start {
        tracing::trace!(
            phase = name,
            elapsed_us = start.elapsed().as_micros() as u64,
            "phase returned"
        );
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filters_have_explicit_precedence_and_reject_external_targets() {
        assert!(Configuration::new(4, None).is_err());
        assert!(Configuration::new(0, Some("hyper=trace")).is_err());
        assert!(Configuration::new(0, Some("fidan_cli[span]=trace")).is_err());
        assert!(Configuration::new(0, Some("fidan_cli=trace")).is_ok());
        assert!(Configuration::new(2, Some("invalid")).is_ok());
    }
    #[test]
    fn redacts_registered_credentials_urls_headers_and_controls() {
        register_secret("synthetic-unique-credential");
        let result = redact(
            "synthetic-unique-credential https://user:pass@host/path?signature=private Authorization: Bearer abc\npassword=hidden",
        );
        for secret in [
            "synthetic-unique-credential",
            "user:pass",
            "signature=private",
            "Bearer abc",
            "hidden",
        ] {
            assert!(!result.contains(secret), "{result}");
        }
        assert!(result.contains("https://host/"));
    }
    #[test]
    fn remote_json_credentials_are_redacted() {
        let text = redact(
            r#"{"access_token":"unregistered-json-token","password":"unregistered-json-password"}"#,
        );
        assert!(!text.contains("unregistered-json-token"), "{text}");
        assert!(!text.contains("unregistered-json-password"), "{text}");
    }
}

#[cfg(test)]
mod transport_tests {
    use super::*;
    use std::{
        io,
        sync::{Arc, Mutex},
    };
    #[derive(Clone)]
    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl io::Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn embedded_hosts_own_transport_and_scoped_subscribers() {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let capture = Capture(bytes.clone());
        register_secret("secret-with-\n-newline");
        let subscriber = subscriber(Configuration::new(3, None).unwrap(), move || {
            capture.clone()
        });
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target:"fidan_embed", "host event");
            tracing::trace!(target:"hyper", "external trace must be filtered");
            tracing::debug!(target:"fidan_embed", token="unregistered-credential", "secret-with-\n-newline");
            tracing::info!(target:"fidan_embed", value=%"a".repeat(MAX_RECORD_BYTES*2), "oversized");
            tracing::trace!(target:"fidan_embed", "control \u{1b}[31m\nforged INFO");
        });
        let text = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        assert!(text.contains("host event"));
        assert!(!text.contains("external trace"));
        assert!(!text.contains("unregistered-credential"));
        assert!(!text.contains("secret-with-"));
        assert!(text.contains("record omitted"));
        assert!(!text.contains('\u{1b}'));
        assert_eq!(text.lines().count(), 4);
    }
}
