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

#[derive(Debug, PartialEq, Eq)]
enum CaptureAction {
    Convert(ConversionAction),
    EndSession(EndSessionAction),
}

#[derive(Debug, PartialEq, Eq)]
struct ConversionAction {
    typed_text: String,
    body: String,
    restore_text: String,
    delete_chars: usize,
    stay_active: bool,
}

#[derive(Debug, PartialEq, Eq)]
struct EndSessionAction {
    typed_text: String,
    replacement_text: String,
    delete_chars: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RestoreAction {
    original_text: String,
    replacement_text: String,
    delete_remaining_chars: usize,
    quote_state_before: QuoteState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReversibleConversion {
    original_text: String,
    inserted_text: String,
    quote_state_before: QuoteState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureMode {
    Idle,
    Active,
}

#[derive(Debug)]
struct CaptureState {
    buffer: String,
    config: AppConfig,
    max_buffer_chars: usize,
    mode: CaptureMode,
    prefix_visible: bool,
    marker_chars_visible: usize,
    last_conversion: Option<ReversibleConversion>,
}

#[cfg(target_os = "macos")]
#[derive(Debug)]
struct ListenerRuntime {
    options: Options,
    capture: CaptureState,
    quote_state: QuoteState,
    session_input_source: Option<String>,
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
    if !body_has_pinyin(body) {
        bail!("input body has no pinyin runs");
    }

    Ok(())
}

fn body_has_pinyin(body: &str) -> bool {
    tokenize_body(body)
        .iter()
        .any(|token| matches!(token, Token::Pinyin(_)))
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
    let runtime = ListenerRuntime {
        options,
        capture,
        quote_state: QuoteState::default(),
        session_input_source: None,
    };
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
    let mut quote_state = QuoteState::default();
    convert_body_with_quote_state(options, body, &mut quote_state)
}

fn convert_body_with_quote_state(
    options: &Options,
    body: String,
    quote_state: &mut QuoteState,
) -> Result<ConversionOutput> {
    let tokens = tokenize_body(&body);
    let mut output = String::new();
    let mut segments = Vec::new();

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
                output.push_str(&map_separator(value, quote_state));
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
            mode: CaptureMode::Idle,
            prefix_visible: false,
            marker_chars_visible: 0,
            last_conversion: None,
        }
    }

    fn push_text(&mut self, text: &str) -> Option<CaptureAction> {
        let mut action = None;

        for ch in text.chars() {
            if ch.is_control() {
                continue;
            }

            if let Some(next_action) = self.push_char(ch) {
                action = Some(next_action);
            }
        }

        action
    }

    fn is_active(&self) -> bool {
        self.mode == CaptureMode::Active || self.prefix_visible || self.marker_chars_visible > 0
    }

    fn push_char(&mut self, ch: char) -> Option<CaptureAction> {
        self.last_conversion = None;
        self.buffer.push(ch);
        self.trim_buffer();

        match self.mode {
            CaptureMode::Idle => {
                if self.buffer.ends_with(&self.config.trigger_prefix) {
                    self.buffer = self.config.trigger_prefix.clone();
                    self.mode = CaptureMode::Active;
                    self.prefix_visible = true;
                    self.marker_chars_visible = 0;
                    self.last_conversion = None;
                }
                None
            }
            CaptureMode::Active => {
                if let Some(action) = self.try_end_session() {
                    return Some(action);
                }

                if self.is_commit_separator(ch) {
                    return self.try_incremental_conversion();
                }

                None
            }
        }
    }

    fn backspace(&mut self) {
        self.last_conversion = None;
        if self.buffer.pop().is_some() {
            if self.buffer.is_empty() {
                self.prefix_visible = false;
                if self.marker_chars_visible > 0 {
                    self.mode = CaptureMode::Active;
                } else {
                    self.mode = CaptureMode::Idle;
                    self.marker_chars_visible = 0;
                }
            } else if self.prefix_visible && !self.buffer.starts_with(&self.config.trigger_prefix) {
                self.marker_chars_visible = self.buffer.chars().count();
                self.buffer.clear();
                self.mode = CaptureMode::Active;
                self.prefix_visible = false;
            }
            return;
        }

        if self.marker_chars_visible > 0 {
            self.marker_chars_visible -= 1;
            self.prefix_visible = false;
            if self.marker_chars_visible > 0 {
                self.mode = CaptureMode::Active;
                return;
            }
        }
        self.mode = CaptureMode::Idle;
    }

    fn delete_previous_word(&mut self) {
        self.last_conversion = None;

        while self.buffer.chars().last().is_some_and(char::is_whitespace) {
            self.buffer.pop();
        }

        while self.buffer.chars().last().is_some_and(is_pinyin_char) {
            self.buffer.pop();
        }
    }

    fn clear(&mut self) {
        self.buffer.clear();
        self.mode = CaptureMode::Idle;
        self.prefix_visible = false;
        self.marker_chars_visible = 0;
        self.last_conversion = None;
    }

    fn record_conversion(
        &mut self,
        original_text: String,
        inserted_text: String,
        quote_state_before: QuoteState,
    ) {
        self.last_conversion = Some(ReversibleConversion {
            original_text,
            inserted_text,
            quote_state_before,
        });
    }

    fn restore_last_conversion(&mut self) -> Option<RestoreAction> {
        let conversion = self.last_conversion.take()?;
        let marker_text = self.config.trigger_prefix.clone();
        let delete_remaining_chars =
            conversion.inserted_text.chars().count() + self.marker_chars_visible.saturating_sub(1);

        self.buffer = conversion.original_text.clone();
        self.mode = CaptureMode::Active;
        self.prefix_visible = false;
        self.marker_chars_visible = self.config.trigger_prefix.chars().count();
        let replacement_text = format!("{}{}", marker_text, conversion.original_text);

        Some(RestoreAction {
            original_text: conversion.original_text,
            replacement_text,
            delete_remaining_chars,
            quote_state_before: conversion.quote_state_before,
        })
    }

    fn trim_buffer(&mut self) {
        while self.buffer.chars().count() > self.max_buffer_chars {
            let Some(first) = self.buffer.chars().next() else {
                return;
            };
            self.buffer.drain(..first.len_utf8());
        }
    }

    fn try_end_session(&mut self) -> Option<CaptureAction> {
        if !self.buffer.ends_with(&self.config.trigger_suffix) {
            return None;
        }

        let suffix_start = self.buffer.len() - self.config.trigger_suffix.len();
        if self.prefix_visible && suffix_start < self.config.trigger_prefix.len() {
            return None;
        }

        let typed_text = self.buffer.clone();
        let body = self.active_body_from(&self.buffer[..suffix_start]);
        let delete_chars = self.current_segment_delete_chars(&typed_text);
        self.buffer.clear();
        self.mode = CaptureMode::Idle;
        self.prefix_visible = false;
        self.marker_chars_visible = 0;
        self.last_conversion = None;

        if body_has_pinyin(&body) {
            Some(CaptureAction::Convert(ConversionAction {
                typed_text,
                restore_text: body.clone(),
                delete_chars,
                body,
                stay_active: false,
            }))
        } else {
            Some(CaptureAction::EndSession(EndSessionAction {
                typed_text,
                replacement_text: body,
                delete_chars,
            }))
        }
    }

    fn try_incremental_conversion(&mut self) -> Option<CaptureAction> {
        let typed_text = self.buffer.clone();
        let body = self.active_body_from(&typed_text);
        let body = trim_commit_only_whitespace(&body).to_owned();
        if !body_has_pinyin(&body) {
            return None;
        }

        let delete_chars = self.current_segment_delete_chars(&typed_text);
        self.buffer.clear();
        self.prefix_visible = false;
        self.marker_chars_visible = self.config.trigger_prefix.chars().count();
        self.mode = CaptureMode::Active;
        self.last_conversion = None;

        Some(CaptureAction::Convert(ConversionAction {
            typed_text,
            restore_text: body.clone(),
            delete_chars,
            body,
            stay_active: true,
        }))
    }

    fn active_body_from(&self, value: &str) -> String {
        if self.prefix_visible {
            value
                .strip_prefix(&self.config.trigger_prefix)
                .unwrap_or(value)
                .to_owned()
        } else {
            value.to_owned()
        }
    }

    fn is_commit_separator(&self, ch: char) -> bool {
        !is_pinyin_char(ch)
            && !self.config.trigger_prefix.contains(ch)
            && !self.config.trigger_suffix.contains(ch)
    }

    fn current_segment_delete_chars(&self, typed_text: &str) -> usize {
        let typed_chars = typed_text.chars().count();
        if self.prefix_visible {
            typed_chars
        } else {
            self.marker_chars_visible + typed_chars
        }
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
            self.clear_capture_context("mouse event");
            return Ok(());
        }

        if event.event_type == mac::EVENT_CONTEXT {
            let context_reason = event.text();
            if context_reason.is_empty() {
                self.clear_capture_context("context changed");
            } else {
                self.clear_capture_context(&format!("context changed: {context_reason}"));
            }
            return Ok(());
        }

        if event.event_type != mac::EVENT_KEYBOARD {
            return Ok(());
        }

        let input_source = event_input_source_fingerprint(&event);
        if self.skip_if_input_source_is_not_system(&input_source) {
            return Ok(());
        }

        if self.clear_if_session_input_source_changed(&input_source) {
            return Ok(());
        }

        if matches!(event.key_code, mac::KEY_SHIFT_LEFT | mac::KEY_SHIFT_RIGHT) {
            self.clear_capture_context("shift key");
            return Ok(());
        }

        if event.has_command_modifier() && matches!(event.key_code, mac::KEY_TAB | mac::KEY_GRAVE) {
            self.clear_capture_context("window switch shortcut");
            return Ok(());
        }

        if event.has_text_modifier() {
            self.handle_modified_key(event);
            return Ok(());
        }

        match event.key_code {
            mac::KEY_BACKSPACE => {
                if let Some(action) = self.capture.restore_last_conversion() {
                    self.handle_restore(action)?;
                } else {
                    let was_active = self.capture.is_active();
                    let buffer_chars = self.capture.buffer.chars().count();
                    let buffer_tail = buffer_tail(&self.capture.buffer, 80);
                    self.capture.backspace();
                    if was_active && !self.capture.is_active() {
                        self.quote_state = QuoteState::default();
                        self.session_input_source = None;
                        println!(
                            "[listener] active session cleared reason=backspace buffer_chars={} buffer_tail={:?}",
                            buffer_chars, buffer_tail
                        );
                    } else if self.options.log_events {
                        println!(
                            "[listener] backspace -> buffer_chars={}",
                            self.capture.buffer.chars().count()
                        );
                    }
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
                self.clear_capture_context(&format!("key {}", event.key_code));
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

        let was_active = self.capture.is_active();
        let action = self.capture.push_text(&text);
        self.record_session_input_source_if_opened(was_active, &input_source);

        if let Some(action) = action {
            self.handle_action(action)?;
        } else if self.options.log_events {
            println!(
                "[listener] pushed text={text:?} buffer_chars={} buffer_tail={:?}",
                self.capture.buffer.chars().count(),
                buffer_tail(&self.capture.buffer, 80)
            );
        }

        Ok(())
    }

    fn skip_if_input_source_is_not_system(&mut self, input_source: &str) -> bool {
        if input_source_is_system(input_source) {
            return false;
        }

        let reason = format!("non-system input source: {input_source:?}");
        if self.capture.is_active() {
            self.clear_capture_context(&reason);
        } else {
            self.capture.clear();
            self.quote_state = QuoteState::default();
            self.session_input_source = None;
            if self.options.log_events {
                println!("[listener] {reason} -> skip inactive input");
            }
        }
        true
    }

    fn clear_if_session_input_source_changed(&mut self, current_input_source: &str) -> bool {
        if !self.capture.is_active() {
            return false;
        }

        let Some(previous_input_source) = self.session_input_source.as_deref() else {
            self.session_input_source = Some(current_input_source.to_owned());
            return false;
        };

        if previous_input_source == current_input_source {
            return false;
        }

        let reason =
            format!("input source changed: {previous_input_source:?} -> {current_input_source:?}");
        self.clear_capture_context(&reason);
        true
    }

    fn record_session_input_source_if_opened(&mut self, was_active: bool, input_source: &str) {
        if was_active || !self.capture.is_active() {
            return;
        }

        self.session_input_source = Some(input_source.to_owned());
        println!("[listener] active session opened input_source={input_source:?}");
    }

    fn clear_capture_context(&mut self, reason: &str) {
        let was_active = self.capture.is_active();
        let buffer_chars = self.capture.buffer.chars().count();
        let buffer_tail = buffer_tail(&self.capture.buffer, 80);
        self.capture.clear();
        self.quote_state = QuoteState::default();
        self.session_input_source = None;
        if was_active {
            println!(
                "[listener] active session cleared reason={} buffer_chars={} buffer_tail={:?}",
                reason, buffer_chars, buffer_tail
            );
        } else if self.options.log_events {
            println!("[listener] {reason} -> clear inactive buffer");
        }
    }

    fn handle_modified_key(&mut self, event: mac::InputEvent) {
        if event.has_control_modifier() && event.key_code == mac::KEY_W {
            self.capture.delete_previous_word();
            if self.options.log_events {
                println!(
                    "[listener] ctrl+w -> delete previous word buffer_chars={}",
                    self.capture.buffer.chars().count()
                );
            }
            return;
        }

        if event.has_control_modifier() && event.key_code == mac::KEY_C {
            self.clear_capture_context("control-c shortcut");
            return;
        }

        if event.has_command_modifier()
            && matches!(
                event.key_code,
                mac::KEY_A | mac::KEY_C | mac::KEY_V | mac::KEY_X | mac::KEY_Z
            )
        {
            self.clear_capture_context(&format!("command shortcut key {}", event.key_code));
            return;
        }

        if self.options.log_events {
            println!(
                "[listener] modified key {} ignored for capture buffer",
                event.key_code
            );
        }
    }

    fn handle_action(&mut self, action: CaptureAction) -> Result<()> {
        match action {
            CaptureAction::Convert(action) => self.handle_conversion(action),
            CaptureAction::EndSession(action) => {
                self.quote_state = QuoteState::default();
                self.session_input_source = None;
                let delete_count = action.delete_chars;
                println!(
                    "[listener] session ended delete_chars={} typed={:?}",
                    delete_count, action.typed_text
                );
                mac::inject_backspaces(delete_count, self.options.inject_delay_ms);
                if !action.replacement_text.is_empty() {
                    mac::inject_string(&action.replacement_text, self.options.inject_delay_ms)?;
                }
                Ok(())
            }
        }
    }

    fn handle_conversion(&mut self, action: ConversionAction) -> Result<()> {
        let delete_count = action.delete_chars;
        let conversion_kind = if action.stay_active {
            "incremental"
        } else {
            "final"
        };
        println!(
            "[listener] {conversion_kind} conversion triggered delete_chars={} body={:?}",
            delete_count, action.body
        );
        ensure_body_has_pinyin(&action.body)?;

        let quote_state_before = self.quote_state;
        let mut quote_state_after = self.quote_state;
        let output =
            convert_body_with_quote_state(&self.options, action.body, &mut quote_state_after)?;
        println!(
            "[listener] converted segments={} output={:?}",
            output.segments.len(),
            output.output
        );
        let mut injected_output = output.output.clone();
        if action.stay_active {
            injected_output.push_str(&self.capture.config.trigger_prefix);
        }
        println!(
            "[listener] injecting delete_chars={} output_chars={}",
            delete_count,
            injected_output.chars().count()
        );

        mac::inject_backspaces(delete_count, self.options.inject_delay_ms);
        mac::inject_string(&injected_output, self.options.inject_delay_ms)?;
        if action.stay_active {
            self.capture.record_conversion(
                action.restore_text,
                output.output.clone(),
                quote_state_before,
            );
        }
        self.quote_state = if action.stay_active {
            quote_state_after
        } else {
            self.session_input_source = None;
            QuoteState::default()
        };

        println!(
            "converted: {:?} -> {:?}",
            action.typed_text, injected_output
        );
        if !action.stay_active {
            println!(
                "[listener] session ended after final conversion typed={:?}",
                action.typed_text
            );
        }
        Ok(())
    }

    fn handle_restore(&mut self, action: RestoreAction) -> Result<()> {
        self.quote_state = action.quote_state_before;
        println!(
            "[listener] restoring original text delete_remaining_chars={} original={:?}",
            action.delete_remaining_chars, action.original_text
        );
        mac::inject_backspaces(action.delete_remaining_chars, self.options.inject_delay_ms);
        mac::inject_string(&action.replacement_text, self.options.inject_delay_ms)?;
        Ok(())
    }

    fn log_event(&self, event: &mac::InputEvent) {
        println!(
            "[event] type={} status={} key={} modifiers={} text={:?} input_source={:?} buffer_chars={}",
            event.event_type,
            event.status,
            event.key_code,
            event.modifier_flags,
            event.text(),
            event_input_source_fingerprint(event),
            self.capture.buffer.chars().count()
        );
    }
}

fn event_input_source_fingerprint(event: &mac::InputEvent) -> String {
    let input_source = event.input_source_fingerprint();
    if input_source.is_empty() {
        "<unknown>".to_owned()
    } else {
        input_source
    }
}

fn input_source_is_system(input_source: &str) -> bool {
    input_source
        .split('|')
        .find_map(|part| part.strip_prefix("source="))
        .is_some_and(|source| source.starts_with("com.apple."))
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

fn trim_commit_only_whitespace(value: &str) -> &str {
    value.trim_end_matches(char::is_whitespace)
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
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

    fn test_capture_state() -> CaptureState {
        CaptureState::new(AppConfig::default(), 128)
    }

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
    fn detects_system_input_source() {
        assert!(!input_source_is_system(
            "source=com.bytedance.inputmethod.doubaoime.pinyin|mode=com.bytedance.inputmethod.doubaoime.pinyin|type=TISTypeKeyboardInputMode|ascii=0"
        ));
        assert!(!input_source_is_system(
            "source=com.bytedance.inputmethod.doubaoime.pinyin|mode=com.bytedance.inputmethod.doubaoime.pinyin|type=TISTypeKeyboardInputMode|ascii=1"
        ));
        assert!(input_source_is_system(
            "source=com.apple.keylayout.ABC|mode=|type=TISTypeKeyboardLayout|ascii=1"
        ));
        assert!(!input_source_is_system("<unknown>"));
    }

    #[test]
    fn starts_active_capture_after_trigger_prefix() {
        let mut capture = test_capture_state();

        assert_eq!(capture.push_text(";;"), None);

        assert_eq!(capture.mode, CaptureMode::Active);
        assert!(capture.prefix_visible);
        assert_eq!(capture.buffer, ";;");
    }

    #[test]
    fn entering_active_state_isolates_previous_idle_buffer() {
        let mut capture = test_capture_state();

        assert_eq!(capture.push_text("old text ;;"), None);

        assert_eq!(capture.mode, CaptureMode::Active);
        assert_eq!(capture.buffer, ";;");

        let action = capture.push_text("ceshi ");

        assert_eq!(
            action,
            Some(CaptureAction::Convert(ConversionAction {
                typed_text: ";;ceshi ".to_owned(),
                body: "ceshi".to_owned(),
                restore_text: "ceshi".to_owned(),
                delete_chars: ";;ceshi ".chars().count(),
                stay_active: true,
            }))
        );
    }

    #[test]
    fn space_triggers_incremental_conversion_without_restoring_space() {
        let mut capture = test_capture_state();
        capture.push_text(";;");

        let action = capture.push_text("woyaoceshi ");

        assert_eq!(
            action,
            Some(CaptureAction::Convert(ConversionAction {
                typed_text: ";;woyaoceshi ".to_owned(),
                body: "woyaoceshi".to_owned(),
                restore_text: "woyaoceshi".to_owned(),
                delete_chars: ";;woyaoceshi ".chars().count(),
                stay_active: true,
            }))
        );
        assert_eq!(capture.mode, CaptureMode::Active);
        assert!(!capture.prefix_visible);
        assert_eq!(
            capture.marker_chars_visible,
            capture.config.trigger_prefix.chars().count()
        );
        assert!(capture.buffer.is_empty());
    }

    #[test]
    fn punctuation_triggers_incremental_conversion_and_keeps_separator() {
        let mut capture = test_capture_state();
        capture.push_text(";;");

        let action = capture.push_text("woyaoceshi,");

        assert_eq!(
            action,
            Some(CaptureAction::Convert(ConversionAction {
                typed_text: ";;woyaoceshi,".to_owned(),
                body: "woyaoceshi,".to_owned(),
                restore_text: "woyaoceshi,".to_owned(),
                delete_chars: ";;woyaoceshi,".chars().count(),
                stay_active: true,
            }))
        );
    }

    #[test]
    fn arbitrary_non_pinyin_separator_triggers_incremental_conversion() {
        let mut capture = test_capture_state();
        capture.push_text(";;");

        let action = capture.push_text("woyaoceshi:");

        assert_eq!(
            action,
            Some(CaptureAction::Convert(ConversionAction {
                typed_text: ";;woyaoceshi:".to_owned(),
                body: "woyaoceshi:".to_owned(),
                restore_text: "woyaoceshi:".to_owned(),
                delete_chars: ";;woyaoceshi:".chars().count(),
                stay_active: true,
            }))
        );
    }

    #[test]
    fn trigger_characters_do_not_trigger_incremental_conversion() {
        let mut capture = test_capture_state();
        capture.push_text(";;");

        let action = capture.push_text("woyaoceshi;");

        assert_eq!(action, None);
        assert_eq!(capture.mode, CaptureMode::Active);
        assert_eq!(capture.buffer, ";;woyaoceshi;");
    }

    #[test]
    fn clear_exits_active_state_and_drops_restore_history() {
        let mut capture = test_capture_state();
        capture.push_text(";;woyaoceshi");
        capture.record_conversion(
            "woyaoceshi".to_owned(),
            "我要测试".to_owned(),
            QuoteState::default(),
        );

        capture.clear();

        assert_eq!(capture.mode, CaptureMode::Idle);
        assert!(capture.buffer.is_empty());
        assert_eq!(capture.restore_last_conversion(), None);
    }

    #[test]
    fn delete_previous_word_keeps_active_state() {
        let mut capture = test_capture_state();
        capture.buffer = ";;woyao ceshi".to_owned();
        capture.mode = CaptureMode::Active;
        capture.prefix_visible = true;

        capture.delete_previous_word();

        assert_eq!(capture.mode, CaptureMode::Active);
        assert_eq!(capture.buffer, ";;woyao ");

        capture.delete_previous_word();

        assert_eq!(capture.mode, CaptureMode::Active);
        assert_eq!(capture.buffer, ";;");
    }

    #[test]
    fn suffix_still_converts_pending_body_and_ends_session() {
        let mut capture = test_capture_state();

        let action = capture.push_text(";;woyaoceshi;;");

        assert_eq!(
            action,
            Some(CaptureAction::Convert(ConversionAction {
                typed_text: ";;woyaoceshi;;".to_owned(),
                body: "woyaoceshi".to_owned(),
                restore_text: "woyaoceshi".to_owned(),
                delete_chars: ";;woyaoceshi;;".chars().count(),
                stay_active: false,
            }))
        );
        assert_eq!(capture.mode, CaptureMode::Idle);
        assert!(capture.buffer.is_empty());
    }

    #[test]
    fn suffix_after_incremental_conversion_removes_visible_marker() {
        let mut capture = test_capture_state();
        capture.push_text(";;");
        capture.push_text("woyaoceshi ");
        capture.record_conversion(
            "woyaoceshi".to_owned(),
            "我要测试".to_owned(),
            QuoteState::default(),
        );

        let action = capture.push_text(";;");

        assert_eq!(
            action,
            Some(CaptureAction::EndSession(EndSessionAction {
                typed_text: ";;".to_owned(),
                replacement_text: String::new(),
                delete_chars: ";;;;".chars().count(),
            }))
        );
        assert_eq!(capture.mode, CaptureMode::Idle);
    }

    #[test]
    fn suffix_after_incremental_conversion_converts_pending_body_and_removes_marker() {
        let mut capture = test_capture_state();
        capture.push_text(";;");
        capture.push_text("woyao ");
        capture.record_conversion("woyao".to_owned(), "我要".to_owned(), QuoteState::default());

        let action = capture.push_text("ceshi;;");

        assert_eq!(
            action,
            Some(CaptureAction::Convert(ConversionAction {
                typed_text: "ceshi;;".to_owned(),
                body: "ceshi".to_owned(),
                restore_text: "ceshi".to_owned(),
                delete_chars: ";;ceshi;;".chars().count(),
                stay_active: false,
            }))
        );
        assert_eq!(capture.mode, CaptureMode::Idle);
    }

    #[test]
    fn backspace_after_conversion_restores_original_text() {
        let mut capture = test_capture_state();
        let quote_state_before = QuoteState { next_is_open: true };
        capture.marker_chars_visible = capture.config.trigger_prefix.chars().count();
        capture.mode = CaptureMode::Active;
        capture.record_conversion(
            "woyaoceshi".to_owned(),
            "我要测试".to_owned(),
            quote_state_before,
        );

        let restore = capture.restore_last_conversion();

        assert_eq!(
            restore,
            Some(RestoreAction {
                original_text: "woyaoceshi".to_owned(),
                replacement_text: ";;woyaoceshi".to_owned(),
                delete_remaining_chars: 5,
                quote_state_before,
            })
        );
        assert_eq!(capture.mode, CaptureMode::Active);
        assert_eq!(capture.buffer, "woyaoceshi");
        assert!(!capture.prefix_visible);
    }

    #[test]
    fn backspace_to_empty_raw_buffer_keeps_active_when_marker_is_visible() {
        let mut capture = test_capture_state();
        capture.marker_chars_visible = capture.config.trigger_prefix.chars().count();
        capture.mode = CaptureMode::Active;
        capture.buffer = "bug".to_owned();

        capture.backspace();
        capture.backspace();
        capture.backspace();

        assert_eq!(capture.mode, CaptureMode::Active);
        assert_eq!(
            capture.marker_chars_visible,
            capture.config.trigger_prefix.chars().count()
        );
        assert!(capture.buffer.is_empty());

        let action = capture.push_text("le ");

        assert_eq!(
            action,
            Some(CaptureAction::Convert(ConversionAction {
                typed_text: "le ".to_owned(),
                body: "le".to_owned(),
                restore_text: "le".to_owned(),
                delete_chars: ";;le ".chars().count(),
                stay_active: true,
            }))
        );
    }

    #[test]
    fn backspace_with_empty_raw_buffer_deletes_visible_marker() {
        let mut capture = test_capture_state();
        capture.marker_chars_visible = capture.config.trigger_prefix.chars().count();
        capture.mode = CaptureMode::Active;

        capture.backspace();

        assert_eq!(capture.mode, CaptureMode::Active);
        assert_eq!(capture.marker_chars_visible, 1);
    }

    #[test]
    fn backspace_after_deleting_all_marker_chars_exits_active_state() {
        let mut capture = test_capture_state();
        capture.marker_chars_visible = capture.config.trigger_prefix.chars().count();
        capture.mode = CaptureMode::Active;

        capture.backspace();

        assert_eq!(capture.mode, CaptureMode::Active);
        assert_eq!(capture.marker_chars_visible, 1);

        capture.backspace();

        assert_eq!(capture.mode, CaptureMode::Idle);
        assert_eq!(capture.marker_chars_visible, 0);
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
