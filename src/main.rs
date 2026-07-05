use std::{
    env, fs,
    io::{self, Write as _},
    path::{Path, PathBuf},
    process::{self, Command},
    sync::{Mutex, OnceLock},
};

use anyhow::{anyhow, bail, Context as _, Result};
use rime_api::{
    create_session, finalize, full_deploy_and_wait, initialize, setup, DeployResult, KeyEvent,
    KeyStatus, Session, Traits,
};
use serde::Deserialize;

#[cfg(target_os = "macos")]
mod mac;

const DEFAULT_CONFIG_FILE: &str = "rime-poc.toml";
const DEFAULT_TRIGGER: &str = ";;";
const DEFAULT_MAX_BUFFER_CHARS: usize = 4096;
const DEFAULT_INJECT_DELAY_MS: i32 = 1;
const MIN_MAX_BUFFER_CHARS: usize = 16;

#[cfg(target_os = "macos")]
static LISTENER_RUNTIME: OnceLock<Mutex<ListenerRuntime>> = OnceLock::new();

#[derive(Debug, Clone)]
struct Options {
    shared_data_dir: PathBuf,
    user_data_dir: PathBuf,
    schema: String,
    config_path: Option<PathBuf>,
    config: AppConfig,
    body_mode: bool,
    doctor: bool,
    listen: bool,
    max_buffer_chars: usize,
    inject_delay_ms: i32,
    log_events: bool,
    input: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AppConfig {
    trigger_prefix: String,
    trigger_suffix: String,
    conversion_mode: ConversionMode,
}

#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    trigger_prefix: Option<String>,
    trigger_suffix: Option<String>,
    conversion_mode: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
enum Token {
    Pinyin(String),
    Separator(String),
    RimeAuto(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConversionMode {
    Segmented,
    RimeAuto,
}

#[derive(Debug)]
struct ConvertedSegment {
    raw: String,
    normalized: String,
    preedit: String,
    first: String,
}

#[derive(Debug)]
struct ConversionOutput {
    body: String,
    output: String,
    tokens: Vec<Token>,
    segments: Vec<ConvertedSegment>,
}

#[derive(Debug)]
struct TriggerMatch {
    full_text: String,
    body: String,
}

#[derive(Debug)]
struct CaptureState {
    buffer: String,
    config: AppConfig,
    max_buffer_chars: usize,
}

#[cfg(target_os = "macos")]
#[derive(Debug)]
struct ListenerRuntime {
    options: Options,
    capture: CaptureState,
}

fn main() -> Result<()> {
    let options = Options::parse()?;

    if options.doctor {
        print_doctor(&options);
        if options.input.is_empty() && !options.listen {
            return Ok(());
        }
    }

    if !options.shared_data_dir.exists() {
        bail!(
            "Rime shared data directory does not exist: {}",
            options.shared_data_dir.display()
        );
    }

    std::fs::create_dir_all(&options.user_data_dir).with_context(|| {
        format!(
            "failed to create Rime user data directory: {}",
            options.user_data_dir.display()
        )
    })?;

    if options.input.is_empty() && !options.listen {
        bail!("empty input");
    }

    let mut traits = Traits::new();
    traits
        .set_shared_data_dir(path_to_str(&options.shared_data_dir)?)
        .set_user_data_dir(path_to_str(&options.user_data_dir)?)
        .set_distribution_name("rime-poc")
        .set_distribution_code_name("rime-poc")
        .set_distribution_version(env!("CARGO_PKG_VERSION"))
        .set_app_name("rime-poc")
        .set_min_log_level(2);

    setup(&mut traits);
    initialize(&mut traits);

    let result = if options.listen {
        run_listener(options)
    } else {
        let body = extract_body(&options.input, &options.config, options.body_mode)?;
        ensure_body_has_pinyin(&body)?;
        run_conversion(&options, body)
    };

    finalize();

    result
}

impl Options {
    fn parse() -> Result<Self> {
        let mut shared_data_dir = env::var_os("RIME_SHARED_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(default_shared_data_dir);
        let mut user_data_dir = env::var_os("RIME_USER_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(default_user_data_dir);
        let mut schema = env::var("RIME_SCHEMA").unwrap_or_else(|_| "luna_pinyin_simp".to_owned());
        let mut config_path = env::var_os("RIME_POC_CONFIG").map(PathBuf::from);
        let mut body_mode = false;
        let mut doctor = false;
        let mut listen = false;
        let mut log_events = env_flag("RIME_POC_LOG_EVENTS");
        let mut conversion_mode_override = env::var("RIME_POC_CONVERSION_MODE")
            .ok()
            .map(|value| ConversionMode::parse(&value))
            .transpose()?;
        let mut max_buffer_chars = env::var("RIME_POC_MAX_BUFFER_CHARS")
            .ok()
            .map(|value| parse_usize(&value, "RIME_POC_MAX_BUFFER_CHARS"))
            .transpose()?
            .unwrap_or(DEFAULT_MAX_BUFFER_CHARS);
        let mut inject_delay_ms = env::var("RIME_POC_INJECT_DELAY_MS")
            .ok()
            .map(|value| parse_i32(&value, "RIME_POC_INJECT_DELAY_MS"))
            .transpose()?
            .unwrap_or(DEFAULT_INJECT_DELAY_MS);
        let mut input_parts = Vec::new();

        let mut args = env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--shared-data-dir" => {
                    shared_data_dir = PathBuf::from(next_arg(&mut args, "--shared-data-dir")?);
                }
                "--user-data-dir" => {
                    user_data_dir = PathBuf::from(next_arg(&mut args, "--user-data-dir")?);
                }
                "--schema" => {
                    schema = next_arg(&mut args, "--schema")?;
                }
                "--config" => {
                    config_path = Some(PathBuf::from(next_arg(&mut args, "--config")?));
                }
                "--conversion-mode" => {
                    conversion_mode_override = Some(ConversionMode::parse(&next_arg(
                        &mut args,
                        "--conversion-mode",
                    )?)?);
                }
                "--listen" => listen = true,
                "--log-events" => log_events = true,
                "--max-buffer-chars" => {
                    max_buffer_chars = parse_usize(
                        &next_arg(&mut args, "--max-buffer-chars")?,
                        "--max-buffer-chars",
                    )?;
                }
                "--inject-delay-ms" => {
                    inject_delay_ms = parse_i32(
                        &next_arg(&mut args, "--inject-delay-ms")?,
                        "--inject-delay-ms",
                    )?;
                }
                "--body" => body_mode = true,
                "--doctor" => doctor = true,
                "-h" | "--help" => {
                    print_help();
                    std::process::exit(0);
                }
                _ if arg.starts_with("--") => bail!("unknown option: {arg}"),
                _ => input_parts.push(arg),
            }
        }

        let (mut config, config_path) = load_config(config_path)?;
        if let Some(conversion_mode) = conversion_mode_override {
            config.conversion_mode = conversion_mode;
        }
        validate_config(&config)?;
        validate_runtime_options(max_buffer_chars, inject_delay_ms)?;

        Ok(Self {
            shared_data_dir,
            user_data_dir,
            schema,
            config_path,
            config,
            body_mode,
            doctor,
            listen,
            max_buffer_chars,
            inject_delay_ms,
            log_events,
            input: input_parts.join(" "),
        })
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            trigger_prefix: DEFAULT_TRIGGER.to_owned(),
            trigger_suffix: DEFAULT_TRIGGER.to_owned(),
            conversion_mode: ConversionMode::Segmented,
        }
    }
}

impl ConversionMode {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "segmented" | "segment" => Ok(Self::Segmented),
            "rime-auto" | "rime_auto" | "auto" => Ok(Self::RimeAuto),
            _ => {
                bail!("invalid conversion mode {value:?}; expected \"segmented\" or \"rime-auto\"")
            }
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Segmented => "segmented",
            Self::RimeAuto => "rime-auto",
        }
    }
}

fn load_config(config_path: Option<PathBuf>) -> Result<(AppConfig, Option<PathBuf>)> {
    let path = match config_path {
        Some(path) => Some(path),
        None => {
            let default = PathBuf::from(DEFAULT_CONFIG_FILE);
            if default.exists() {
                Some(default)
            } else {
                packaged_config_file()
            }
        }
    };

    let Some(path) = path else {
        return Ok((AppConfig::default(), None));
    };

    let content = fs::read_to_string(&path)
        .with_context(|| format!("failed to read config file: {}", path.display()))?;
    let file_config: FileConfig = toml::from_str(&content)
        .with_context(|| format!("failed to parse config file: {}", path.display()))?;
    let defaults = AppConfig::default();

    Ok((
        AppConfig {
            trigger_prefix: file_config
                .trigger_prefix
                .unwrap_or(defaults.trigger_prefix),
            trigger_suffix: file_config
                .trigger_suffix
                .unwrap_or(defaults.trigger_suffix),
            conversion_mode: file_config
                .conversion_mode
                .map(|value| ConversionMode::parse(&value))
                .transpose()?
                .unwrap_or(defaults.conversion_mode),
        },
        Some(path),
    ))
}

fn packaged_config_file() -> Option<PathBuf> {
    let path = env::current_exe().ok()?.parent()?.join(DEFAULT_CONFIG_FILE);
    path.exists().then_some(path)
}

fn validate_config(config: &AppConfig) -> Result<()> {
    validate_trigger_part("trigger_prefix", &config.trigger_prefix)?;
    validate_trigger_part("trigger_suffix", &config.trigger_suffix)?;
    Ok(())
}

fn validate_runtime_options(max_buffer_chars: usize, inject_delay_ms: i32) -> Result<()> {
    if max_buffer_chars < MIN_MAX_BUFFER_CHARS {
        bail!("max buffer chars must be at least {MIN_MAX_BUFFER_CHARS}");
    }

    if inject_delay_ms < 0 {
        bail!("inject delay must be greater than or equal to 0");
    }

    Ok(())
}

fn parse_usize(value: &str, name: &str) -> Result<usize> {
    value
        .parse()
        .with_context(|| format!("failed to parse {name} as a positive integer"))
}

fn parse_i32(value: &str, name: &str) -> Result<i32> {
    value
        .parse()
        .with_context(|| format!("failed to parse {name} as an integer"))
}

fn env_flag(name: &str) -> bool {
    env::var(name)
        .map(|value| {
            let value = value.trim();
            !value.is_empty()
                && value != "0"
                && !value.eq_ignore_ascii_case("false")
                && !value.eq_ignore_ascii_case("no")
        })
        .unwrap_or(false)
}

fn validate_trigger_part(name: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        bail!("{name} cannot be empty");
    }

    for ch in value.chars() {
        if is_reserved_separator(ch) {
            bail!("{name} contains reserved separator character: {ch:?}");
        }
    }

    Ok(())
}

fn extract_body(input: &str, config: &AppConfig, body_mode: bool) -> Result<String> {
    if body_mode {
        return Ok(input.to_owned());
    }

    if !input.starts_with(&config.trigger_prefix) {
        bail!(
            "input does not start with configured trigger_prefix: {:?}",
            config.trigger_prefix
        );
    }

    if !input.ends_with(&config.trigger_suffix) {
        bail!(
            "input does not end with configured trigger_suffix: {:?}",
            config.trigger_suffix
        );
    }

    let body_start = config.trigger_prefix.len();
    let body_end = input
        .len()
        .checked_sub(config.trigger_suffix.len())
        .ok_or_else(|| anyhow!("input is shorter than configured triggers"))?;
    if body_start > body_end {
        bail!("input is shorter than configured triggers");
    }

    Ok(input[body_start..body_end].to_owned())
}

fn ensure_body_has_pinyin(body: &str) -> Result<()> {
    if tokenize_body(body)
        .iter()
        .all(|token| !matches!(token, Token::Pinyin(_)))
    {
        bail!("input body has no pinyin runs");
    }

    Ok(())
}

fn run_conversion(options: &Options, body: String) -> Result<()> {
    match full_deploy_and_wait() {
        DeployResult::Success => {}
        DeployResult::Failure => bail!("Rime deployment failed"),
    }

    let output = convert_body(options, body)?;

    println!("input:           {}", options.input);
    println!("body:            {}", output.body);
    println!("schema:          {}", options.schema);
    println!("trigger_prefix:  {:?}", options.config.trigger_prefix);
    println!("trigger_suffix:  {:?}", options.config.trigger_suffix);
    println!(
        "conversion_mode: {}",
        options.config.conversion_mode.as_str()
    );
    match &options.config_path {
        Some(path) => println!("config:          {}", path.display()),
        None => println!("config:          <defaults>"),
    }
    println!("output:          {}", output.output);
    println!("delete_chars:    {}", options.input.chars().count());

    println!("tokens:");
    for token in &output.tokens {
        match token {
            Token::Pinyin(raw) => println!("  pinyin:    {raw}"),
            Token::Separator(value) => println!("  separator: {value}"),
            Token::RimeAuto(raw) => println!("  rime-auto: {raw}"),
        }
    }

    println!("segments:");
    for (index, segment) in output.segments.iter().enumerate() {
        println!(
            "  {}. {} -> {} -> {}",
            index + 1,
            segment.raw,
            segment.preedit,
            segment.first
        );
        if segment.normalized != segment.raw {
            println!("     normalized: {}", segment.normalized);
        }
    }

    Ok(())
}

#[cfg(target_os = "macos")]
fn run_listener(options: Options) -> Result<()> {
    ensure_accessibility_interactive()?;
    ensure_input_monitoring_interactive()?;

    match full_deploy_and_wait() {
        DeployResult::Success => {}
        DeployResult::Failure => bail!("Rime deployment failed"),
    }

    let capture = CaptureState::new(options.config.clone(), options.max_buffer_chars);
    let runtime = ListenerRuntime { options, capture };
    LISTENER_RUNTIME
        .set(Mutex::new(runtime))
        .map_err(|_| anyhow!("listener runtime was already initialized"))?;

    let runtime = LISTENER_RUNTIME
        .get()
        .ok_or_else(|| anyhow!("listener runtime was not initialized"))?
        .lock()
        .map_err(|_| anyhow!("listener runtime lock is poisoned"))?;
    println!("rime-poc listener started");
    println!("pid:             {}", process::id());
    println!("accessibility:   {}", mac::is_accessibility_trusted(false));
    println!("input_monitoring: {}", mac::has_input_monitoring_access());
    println!("schema:          {}", runtime.options.schema);
    println!(
        "trigger_prefix:  {:?}",
        runtime.options.config.trigger_prefix
    );
    println!(
        "trigger_suffix:  {:?}",
        runtime.options.config.trigger_suffix
    );
    println!("max_buffer_chars: {}", runtime.options.max_buffer_chars);
    println!("inject_delay_ms: {}", runtime.options.inject_delay_ms);
    println!("log_events:      {}", runtime.options.log_events);
    drop(runtime);

    mac::start_event_loop(mac_event_callback);
}

#[cfg(target_os = "macos")]
fn ensure_accessibility_interactive() -> Result<()> {
    if mac::is_accessibility_trusted(false) {
        return Ok(());
    }

    eprintln!();
    eprintln!("rime-poc needs macOS Accessibility permission before it can listen to keys and inject text.");
    eprintln!("I will open System Settings > Privacy & Security > Accessibility.");
    eprintln!("Enable the current terminal app or the rime-poc binary, then return here.");
    eprintln!();

    let _ = mac::is_accessibility_trusted(true);
    open_accessibility_settings();

    loop {
        eprint!("After granting permission, press Enter to retry, or type q then Enter to quit: ");
        io::stderr().flush().ok();

        let mut input = String::new();
        let bytes = io::stdin()
            .read_line(&mut input)
            .context("failed to read permission prompt input")?;
        if bytes == 0 {
            bail!("Accessibility permission was not granted");
        }

        if input.trim().eq_ignore_ascii_case("q") {
            bail!("Accessibility permission was not granted");
        }

        if mac::is_accessibility_trusted(false) {
            eprintln!("Accessibility permission detected. Starting listener.");
            return Ok(());
        }

        eprintln!("Permission is still not active.");
        eprintln!("If you just enabled it, macOS may require restarting the terminal or rerunning this command.");
        open_accessibility_settings();
    }
}

#[cfg(target_os = "macos")]
fn ensure_input_monitoring_interactive() -> Result<()> {
    if mac::has_input_monitoring_access() {
        return Ok(());
    }

    eprintln!();
    eprintln!(
        "rime-poc needs macOS Input Monitoring permission before it can receive global key events."
    );
    eprintln!("I will request Input Monitoring access and open System Settings.");
    eprintln!("Enable the rime-poc binary, then return here.");
    eprintln!();

    let _ = mac::request_input_monitoring_access();
    open_input_monitoring_settings();

    loop {
        eprint!(
            "After granting Input Monitoring, press Enter to retry, or type q then Enter to quit: "
        );
        io::stderr().flush().ok();

        let mut input = String::new();
        let bytes = io::stdin()
            .read_line(&mut input)
            .context("failed to read permission prompt input")?;
        if bytes == 0 {
            bail!("Input Monitoring permission was not granted");
        }

        if input.trim().eq_ignore_ascii_case("q") {
            bail!("Input Monitoring permission was not granted");
        }

        if mac::has_input_monitoring_access() {
            eprintln!("Input Monitoring permission detected. Starting listener.");
            return Ok(());
        }

        eprintln!("Input Monitoring permission is still not active.");
        eprintln!("If you just enabled it, macOS may require quitting and rerunning rime-poc.");
        open_input_monitoring_settings();
    }
}

#[cfg(target_os = "macos")]
fn open_accessibility_settings() {
    if env::var_os("RIME_POC_SKIP_OPEN_SETTINGS").is_some() {
        return;
    }

    open_settings_pane(
        "Accessibility",
        "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
    );
}

#[cfg(target_os = "macos")]
fn open_input_monitoring_settings() {
    if env::var_os("RIME_POC_SKIP_OPEN_SETTINGS").is_some() {
        return;
    }

    open_settings_pane(
        "Input Monitoring",
        "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent",
    );
}

#[cfg(target_os = "macos")]
fn open_settings_pane(label: &str, url: &str) {
    let status = Command::new("open").arg(url).status();

    match status {
        Ok(status) if status.success() => return,
        Ok(status) => {
            eprintln!("Unable to open {label} settings directly: open exited with {status}");
        }
        Err(error) => {
            eprintln!("Unable to open {label} settings directly: {error}");
        }
    }

    for bundle_id in ["com.apple.SystemSettings", "com.apple.systempreferences"] {
        match Command::new("open").arg("-b").arg(bundle_id).status() {
            Ok(status) if status.success() => return,
            _ => {}
        }
    }

    eprintln!("Unable to open System Settings automatically; please open it manually.");
}

#[cfg(not(target_os = "macos"))]
fn run_listener(_options: Options) -> Result<()> {
    bail!("--listen is only supported on macOS")
}

#[cfg(target_os = "macos")]
extern "C" fn mac_event_callback(event: mac::InputEvent) {
    let Some(runtime) = LISTENER_RUNTIME.get() else {
        return;
    };

    let runtime = runtime.lock();
    let Ok(mut runtime) = runtime else {
        eprintln!("listener runtime lock is poisoned");
        return;
    };

    if let Err(error) = runtime.handle_event(event) {
        eprintln!("listener error: {error:#}");
    }
}

fn convert_body(options: &Options, body: String) -> Result<ConversionOutput> {
    match options.config.conversion_mode {
        ConversionMode::Segmented => convert_body_segmented(options, body),
        ConversionMode::RimeAuto => convert_body_rime_auto(options, body),
    }
}

fn convert_body_segmented(options: &Options, body: String) -> Result<ConversionOutput> {
    let tokens = tokenize_body(&body);
    let mut output = String::new();
    let mut segments = Vec::new();
    let mut quote_state = QuoteState::default();

    for token in &tokens {
        match token {
            Token::Pinyin(raw) => {
                let normalized = normalize_pinyin_run(raw);
                if normalized.is_empty() {
                    continue;
                }
                let segment = convert_pinyin_run(options, raw, &normalized)?;
                output.push_str(&segment.first);
                segments.push(segment);
            }
            Token::Separator(value) => {
                output.push_str(&map_separator(value, &mut quote_state));
            }
            Token::RimeAuto(_) => {
                unreachable!("rime-auto tokens are not produced by segmented tokenization")
            }
        }
    }

    Ok(ConversionOutput {
        body,
        output,
        tokens,
        segments,
    })
}

fn convert_body_rime_auto(options: &Options, body: String) -> Result<ConversionOutput> {
    let mut session = create_selected_session(&options.schema)?;
    let mut output = String::new();

    for ch in body.chars() {
        let status = session.process_key(KeyEvent::new(ch as u32, 0));
        if matches!(status, KeyStatus::Pass) {
            output.push(ch);
        }

        if let Some(commit) = session.commit() {
            output.push_str(commit.text());
        }
    }

    let mut preedit = "<none>".to_owned();
    let mut final_candidate = None;
    if let Some(context) = session.context() {
        let composition = context.composition();
        preedit = composition.preedit.unwrap_or("<none>").to_owned();
        final_candidate = context
            .menu()
            .candidates
            .first()
            .map(|candidate| candidate.text.to_owned());
    }

    if let Some(candidate) = final_candidate {
        output.push_str(&candidate);
    }

    let segment = ConvertedSegment {
        raw: body.clone(),
        normalized: body.clone(),
        preedit,
        first: output.clone(),
    };

    session.close().context("failed to close Rime session")?;

    Ok(ConversionOutput {
        body: body.clone(),
        output,
        tokens: vec![Token::RimeAuto(body)],
        segments: vec![segment],
    })
}

impl CaptureState {
    fn new(config: AppConfig, max_buffer_chars: usize) -> Self {
        Self {
            buffer: String::new(),
            config,
            max_buffer_chars,
        }
    }

    fn push_text(&mut self, text: &str) -> Option<TriggerMatch> {
        for ch in text.chars() {
            if ch.is_control() {
                continue;
            }
            self.buffer.push(ch);
            self.trim_buffer();
        }

        self.try_match()
    }

    fn backspace(&mut self) {
        self.buffer.pop();
    }

    fn clear(&mut self) {
        self.buffer.clear();
    }

    fn trim_buffer(&mut self) {
        while self.buffer.chars().count() > self.max_buffer_chars {
            let Some(first) = self.buffer.chars().next() else {
                return;
            };
            self.buffer.drain(..first.len_utf8());
        }
    }

    fn try_match(&self) -> Option<TriggerMatch> {
        if !self.buffer.ends_with(&self.config.trigger_suffix) {
            return None;
        }

        let suffix_start = self.buffer.len() - self.config.trigger_suffix.len();
        let before_suffix = &self.buffer[..suffix_start];
        let prefix_start = before_suffix.rfind(&self.config.trigger_prefix)?;
        let body_start = prefix_start + self.config.trigger_prefix.len();
        let body = before_suffix[body_start..].to_owned();
        let full_text = self.buffer[prefix_start..].to_owned();

        Some(TriggerMatch { full_text, body })
    }
}

#[cfg(target_os = "macos")]
impl ListenerRuntime {
    fn handle_event(&mut self, event: mac::InputEvent) -> Result<()> {
        if self.options.log_events {
            self.log_event(&event);
        }

        if event.status != mac::STATUS_PRESSED {
            return Ok(());
        }

        if event.event_type == mac::EVENT_MOUSE {
            if self.options.log_events {
                println!("[listener] mouse event -> clear buffer");
            }
            self.capture.clear();
            return Ok(());
        }

        if event.event_type != mac::EVENT_KEYBOARD {
            return Ok(());
        }

        match event.key_code {
            mac::KEY_BACKSPACE => {
                self.capture.backspace();
                if self.options.log_events {
                    println!(
                        "[listener] backspace -> buffer_chars={}",
                        self.capture.buffer.chars().count()
                    );
                }
                return Ok(());
            }
            mac::KEY_ENTER
            | mac::KEY_RETURN
            | mac::KEY_ESCAPE
            | mac::KEY_ARROW_LEFT
            | mac::KEY_ARROW_RIGHT
            | mac::KEY_ARROW_DOWN
            | mac::KEY_ARROW_UP => {
                self.capture.clear();
                if self.options.log_events {
                    println!("[listener] key {} -> clear buffer", event.key_code);
                }
                return Ok(());
            }
            _ => {}
        }

        let text = event.text();
        if text.is_empty() {
            if self.options.log_events {
                println!("[listener] key {} has empty text", event.key_code);
            }
            return Ok(());
        }

        if let Some(matched) = self.capture.push_text(&text) {
            self.handle_match(matched)?;
        } else if self.options.log_events {
            println!(
                "[listener] pushed text={text:?} buffer_chars={} buffer_tail={:?}",
                self.capture.buffer.chars().count(),
                buffer_tail(&self.capture.buffer, 80)
            );
        }

        Ok(())
    }

    fn handle_match(&mut self, matched: TriggerMatch) -> Result<()> {
        self.capture.clear();
        let delete_count = matched.full_text.chars().count();
        println!(
            "[listener] trigger matched delete_chars={} body={:?}",
            delete_count, matched.body
        );
        ensure_body_has_pinyin(&matched.body)?;

        let output = convert_body(&self.options, matched.body)?;
        println!(
            "[listener] converted segments={} output={:?}",
            output.segments.len(),
            output.output
        );
        println!(
            "[listener] injecting delete_chars={} output_chars={}",
            delete_count,
            output.output.chars().count()
        );

        mac::inject_backspaces(delete_count, self.options.inject_delay_ms);
        mac::inject_string(&output.output, self.options.inject_delay_ms)?;

        println!("converted: {:?} -> {:?}", matched.full_text, output.output);
        Ok(())
    }

    fn log_event(&self, event: &mac::InputEvent) {
        println!(
            "[event] type={} status={} key={} text={:?} buffer_chars={}",
            event.event_type,
            event.status,
            event.key_code,
            event.text(),
            self.capture.buffer.chars().count()
        );
    }
}

fn buffer_tail(value: &str, max_chars: usize) -> String {
    let chars = value.chars().collect::<Vec<_>>();
    let start = chars.len().saturating_sub(max_chars);
    chars[start..].iter().collect()
}

fn convert_pinyin_run(options: &Options, raw: &str, normalized: &str) -> Result<ConvertedSegment> {
    let mut session = create_selected_session(&options.schema)?;

    for ch in normalized.chars() {
        let status = session.process_key(KeyEvent::new(ch as u32, 0));
        if matches!(status, KeyStatus::Pass) {
            bail!("Rime did not accept key {ch:?} while converting {raw:?}");
        }
    }

    let context = session
        .context()
        .ok_or_else(|| anyhow!("Rime did not return a context for {raw:?}"))?;
    let composition = context.composition();
    let menu = context.menu();
    let Some(first) = menu.candidates.first() else {
        bail!("Rime returned no candidates for {raw:?}");
    };

    let segment = ConvertedSegment {
        raw: raw.to_owned(),
        normalized: normalized.to_owned(),
        preedit: composition.preedit.unwrap_or("<none>").to_owned(),
        first: first.text.to_owned(),
    };

    session.close().context("failed to close Rime session")?;
    Ok(segment)
}

fn create_selected_session(schema: &str) -> Result<Session> {
    let session = create_session().context("failed to create Rime session")?;
    session
        .select_schema(schema)
        .with_context(|| format!("failed to select Rime schema: {schema}"))?;
    Ok(session)
}

fn tokenize_body(input: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut pinyin = String::new();
    let chars: Vec<char> = input.chars().collect();
    let mut index = 0;

    while index < chars.len() {
        if starts_with_ascii_ellipsis(&chars, index) {
            flush_pinyin(&mut tokens, &mut pinyin);
            tokens.push(Token::Separator("...".to_owned()));
            index += 3;
            continue;
        }

        if starts_with_chinese_ellipsis_pair(&chars, index) {
            flush_pinyin(&mut tokens, &mut pinyin);
            tokens.push(Token::Separator("……".to_owned()));
            index += 2;
            continue;
        }

        let ch = chars[index];
        if is_pinyin_char(ch) {
            pinyin.push(ch.to_ascii_lowercase());
        } else {
            flush_pinyin(&mut tokens, &mut pinyin);
            tokens.push(Token::Separator(ch.to_string()));
        }

        index += 1;
    }

    flush_pinyin(&mut tokens, &mut pinyin);
    tokens
}

fn starts_with_ascii_ellipsis(chars: &[char], index: usize) -> bool {
    chars.get(index) == Some(&'.')
        && chars.get(index + 1) == Some(&'.')
        && chars.get(index + 2) == Some(&'.')
}

fn starts_with_chinese_ellipsis_pair(chars: &[char], index: usize) -> bool {
    chars.get(index) == Some(&'…') && chars.get(index + 1) == Some(&'…')
}

fn flush_pinyin(tokens: &mut Vec<Token>, pinyin: &mut String) {
    if !pinyin.is_empty() {
        tokens.push(Token::Pinyin(std::mem::take(pinyin)));
    }
}

fn normalize_pinyin_run(input: &str) -> String {
    input
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '\'')
        .map(|ch| ch.to_ascii_lowercase())
        .collect()
}

fn is_pinyin_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '\''
}

fn is_reserved_separator(ch: char) -> bool {
    matches!(
        ch,
        ',' | '.'
            | '?'
            | '!'
            | '-'
            | '+'
            | '~'
            | '"'
            | '，'
            | '。'
            | '？'
            | '！'
            | '－'
            | '＋'
            | '～'
            | '〜'
            | '…'
            | '“'
            | '”'
            | '＂'
    )
}

#[derive(Debug, Default)]
struct QuoteState {
    next_is_open: bool,
}

fn map_separator(value: &str, quote_state: &mut QuoteState) -> String {
    match value {
        "," => "，".to_owned(),
        "." => "。".to_owned(),
        "?" => "？".to_owned(),
        "!" => "！".to_owned(),
        "-" => "－".to_owned(),
        "+" => "＋".to_owned(),
        "..." => "……".to_owned(),
        "……" => "……".to_owned(),
        "~" => "～".to_owned(),
        "\"" => {
            quote_state.next_is_open = !quote_state.next_is_open;
            if quote_state.next_is_open {
                "“".to_owned()
            } else {
                "”".to_owned()
            }
        }
        "…" => "……".to_owned(),
        _ => value.to_owned(),
    }
}

fn print_doctor(options: &Options) {
    println!("rime-poc doctor");
    println!("shared_data_dir: {}", options.shared_data_dir.display());
    println!("  exists: {}", options.shared_data_dir.exists());
    println!("user_data_dir:   {}", options.user_data_dir.display());
    println!("  exists: {}", options.user_data_dir.exists());
    println!("schema:          {}", options.schema);
    println!("config:          {:?}", options.config_path);
    println!("trigger_prefix:  {:?}", options.config.trigger_prefix);
    println!("trigger_suffix:  {:?}", options.config.trigger_suffix);
    println!(
        "conversion_mode: {}",
        options.config.conversion_mode.as_str()
    );
    println!("body_mode:       {}", options.body_mode);
    println!("listen:          {}", options.listen);
    println!("max_buffer_chars: {}", options.max_buffer_chars);
    println!("inject_delay_ms: {}", options.inject_delay_ms);
    println!("log_events:      {}", options.log_events);
}

fn default_shared_data_dir() -> PathBuf {
    if let Some(path) = packaged_data_dir("shared") {
        return path;
    }

    let candidates = [
        "/Library/Input Methods/Squirrel.app/Contents/SharedSupport",
        "/opt/homebrew/share/rime-data",
        "/usr/local/share/rime-data",
        "/usr/share/rime-data",
    ];

    candidates
        .iter()
        .map(PathBuf::from)
        .find(|path| path.exists())
        .unwrap_or_else(|| PathBuf::from("/opt/homebrew/share/rime-data"))
}

fn default_user_data_dir() -> PathBuf {
    if let Some(path) = packaged_data_dir("user") {
        return path;
    }

    home_dir()
        .map(|home| home.join("Library/Rime"))
        .unwrap_or_else(|| PathBuf::from("rime-user"))
}

fn packaged_data_dir(name: &str) -> Option<PathBuf> {
    let path = env::current_exe().ok()?.parent()?.join("data").join(name);
    path.exists().then_some(path)
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}

fn path_to_str(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| anyhow!("path is not valid UTF-8: {}", path.display()))
}

fn next_arg(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String> {
    args.next()
        .ok_or_else(|| anyhow!("missing value for {flag}"))
}

fn print_help() {
    println!(
        "Usage: rime-poc [OPTIONS] <triggered-text>\n\n\
Options:\n  \
--config <FILE>          Trigger config file [env: RIME_POC_CONFIG] [default: ./rime-poc.toml if present]\n  \
--conversion-mode <MODE> Conversion mode: segmented or rime-auto [env: RIME_POC_CONVERSION_MODE]\n  \
--listen                 Start macOS global listener mode\n  \
--log-events             Print every key/mouse event seen by the listener [env: RIME_POC_LOG_EVENTS]\n  \
--body                   Treat input as body text without requiring trigger prefix/suffix\n  \
--max-buffer-chars <N>   Maximum listener buffer length [env: RIME_POC_MAX_BUFFER_CHARS] [default: 4096]\n  \
--inject-delay-ms <N>    Delay between injected key events [env: RIME_POC_INJECT_DELAY_MS] [default: 1]\n  \
--shared-data-dir <DIR>  Rime shared data directory [env: RIME_SHARED_DATA_DIR]\n  \
--user-data-dir <DIR>    Rime user data directory [env: RIME_USER_DATA_DIR]\n  \
--schema <ID>            Rime schema id [env: RIME_SCHEMA] [default: luna_pinyin_simp]\n  \
--doctor                 Print resolved paths and config before running\n  \
-h, --help               Print help"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_body_with_configured_triggers() {
        let config = AppConfig {
            trigger_prefix: "[[".to_owned(),
            trigger_suffix: "]]".to_owned(),
            conversion_mode: ConversionMode::Segmented,
        };

        let body = extract_body("[[woyaoceshi]]", &config, false).unwrap();

        assert_eq!(body, "woyaoceshi");
    }

    #[test]
    fn rejects_reserved_trigger_chars() {
        let config = AppConfig {
            trigger_prefix: ";;".to_owned(),
            trigger_suffix: "?".to_owned(),
            conversion_mode: ConversionMode::Segmented,
        };

        let error = validate_config(&config).unwrap_err().to_string();

        assert!(error.contains("trigger_suffix"));

        let config = AppConfig {
            trigger_prefix: "!".to_owned(),
            trigger_suffix: ";;".to_owned(),
            conversion_mode: ConversionMode::Segmented,
        };

        let error = validate_config(&config).unwrap_err().to_string();

        assert!(error.contains("trigger_prefix"));
    }

    #[test]
    fn tokenizes_pinyin_runs_and_separators() {
        assert_eq!(
            tokenize_body("woyaoceshi,nihaoma?\"hao...zaijian\""),
            vec![
                Token::Pinyin("woyaoceshi".to_owned()),
                Token::Separator(",".to_owned()),
                Token::Pinyin("nihaoma".to_owned()),
                Token::Separator("?".to_owned()),
                Token::Separator("\"".to_owned()),
                Token::Pinyin("hao".to_owned()),
                Token::Separator("...".to_owned()),
                Token::Pinyin("zaijian".to_owned()),
                Token::Separator("\"".to_owned()),
            ]
        );

        assert_eq!(
            tokenize_body("hao……zaijian"),
            vec![
                Token::Pinyin("hao".to_owned()),
                Token::Separator("……".to_owned()),
                Token::Pinyin("zaijian".to_owned()),
            ]
        );
    }

    #[test]
    fn maps_half_width_separators_to_chinese_punctuation() {
        let mut quote_state = QuoteState::default();
        let mapped = [",", ".", "?", "!", "-", "+", "...", "~", "\"", "\""]
            .into_iter()
            .map(|item| map_separator(item, &mut quote_state))
            .collect::<String>();

        assert_eq!(mapped, "，。？！－＋……～“”");

        let mut quote_state = QuoteState::default();
        let mapped_quotes = ["\"", "\"", "\"", "\""]
            .into_iter()
            .map(|item| map_separator(item, &mut quote_state))
            .collect::<String>();

        assert_eq!(mapped_quotes, "“”“”");
    }

    #[test]
    fn parses_conversion_modes() {
        assert_eq!(
            ConversionMode::parse("segmented").unwrap(),
            ConversionMode::Segmented
        );
        assert_eq!(
            ConversionMode::parse("rime-auto").unwrap(),
            ConversionMode::RimeAuto
        );
        assert!(ConversionMode::parse("unknown").is_err());
    }
}
