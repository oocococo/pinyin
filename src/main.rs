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

mod rime_direct;

#[cfg(target_os = "macos")]
mod mac;

const DEFAULT_CONFIG_FILE: &str = "rime-poc.toml";
const DEFAULT_TRIGGER: &str = ";;";
const DEFAULT_MAX_BUFFER_CHARS: usize = 4096;
const DEFAULT_INJECT_DELAY_MS: i32 = 1;
const DEFAULT_CANDIDATE_COUNT: usize = 5;
const DEFAULT_CANDIDATE_SELECT_KEYS: &str = "1234567890";
const DEFAULT_CANDIDATE_PAGE_NEXT_KEY: &str = "=";
const DEFAULT_CANDIDATE_PAGE_PREVIOUS_KEY: &str = "-";
const DEFAULT_ENGLISH_COMMIT_KEY: &str = "`";
const MIN_MAX_BUFFER_CHARS: usize = 16;
const MIN_CANDIDATE_COUNT: usize = 1;
const MAX_CANDIDATE_COUNT: usize = 10;

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
    candidate_layout: CandidateLayout,
    candidate_count: usize,
    candidate_select_keys: String,
    candidate_page_next_key: String,
    candidate_page_previous_key: String,
    english_commit_key: String,
}

#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    trigger_prefix: Option<String>,
    trigger_suffix: Option<String>,
    conversion_mode: Option<String>,
    candidate_layout: Option<String>,
    candidate_count: Option<usize>,
    candidate_select_keys: Option<String>,
    candidate_page_next_key: Option<String>,
    candidate_page_previous_key: Option<String>,
    english_commit_key: Option<String>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CandidateLayout {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CandidateInteractionKey {
    Select(usize),
    NextPage,
    PreviousPage,
    EnglishCommit,
}

#[derive(Debug)]
struct ConvertedSegment {
    raw: String,
    normalized: String,
    preedit: String,
    first: String,
}

#[derive(Debug)]
struct CandidatePreview {
    preedit: String,
    candidates: Vec<String>,
    page_no: usize,
    is_last_page: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CandidateSelection {
    Complete {
        text: String,
    },
    Partial {
        selected_text: String,
        remaining_pinyin: String,
    },
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
    StartSession(StartSessionAction),
    Convert(ConversionAction),
    InsertLiteral(InsertLiteralAction),
    EndSession(EndSessionAction),
}

#[derive(Debug, PartialEq, Eq)]
struct StartSessionAction {
    trigger_text: String,
    delete_chars: usize,
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
struct InsertLiteralAction {
    text: String,
}

#[derive(Debug, PartialEq, Eq)]
struct PendingCommitAction {
    original_text: String,
    replacement_text: String,
    delete_chars: usize,
}

#[derive(Debug, PartialEq, Eq)]
struct PendingCandidateAction {
    original_text: String,
    selected_text: String,
    remaining_pinyin: Option<String>,
    replacement_text: String,
    delete_chars: usize,
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
struct DeletePreviousWordOutcome {
    removed_chars: usize,
    removed_visible_chars: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeletePreviousWordDisposition {
    Consume,
    PassThrough,
    ClearAndPassThrough,
}

impl DeletePreviousWordOutcome {
    fn disposition(self) -> DeletePreviousWordDisposition {
        if self.removed_chars == 0
            || (self.removed_visible_chars > 0 && self.removed_visible_chars < self.removed_chars)
        {
            DeletePreviousWordDisposition::ClearAndPassThrough
        } else if self.removed_visible_chars == 0 {
            DeletePreviousWordDisposition::Consume
        } else {
            DeletePreviousWordDisposition::PassThrough
        }
    }
}

#[derive(Debug)]
struct RewriteTransactionGuard {
    abort: Option<fn()>,
}

impl RewriteTransactionGuard {
    fn new(abort: fn()) -> Self {
        Self { abort: Some(abort) }
    }

    #[cfg(target_os = "macos")]
    fn begin() -> Self {
        mac::begin_rewrite_transaction();
        Self::new(mac::cancel_rewrite_transaction)
    }

    fn mark_committed(&mut self) {
        self.abort = None;
    }
}

impl Drop for RewriteTransactionGuard {
    fn drop(&mut self) {
        if let Some(abort) = self.abort.take() {
            abort();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureMode {
    Idle,
    Active,
}

#[derive(Debug, Clone)]
struct CaptureState {
    buffer: String,
    buffer_visible: Vec<bool>,
    config: AppConfig,
    max_buffer_chars: usize,
    mode: CaptureMode,
    committed_output_chars: usize,
    hidden_prefix_backspaces_remaining: usize,
    candidate_page: usize,
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
        let mut candidate_layout_override = env::var("RIME_POC_CANDIDATE_LAYOUT")
            .ok()
            .map(|value| CandidateLayout::parse(&value))
            .transpose()?;
        let mut candidate_count_override = env::var("RIME_POC_CANDIDATE_COUNT")
            .ok()
            .map(|value| parse_usize(&value, "RIME_POC_CANDIDATE_COUNT"))
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
                "--candidate-layout" => {
                    candidate_layout_override = Some(CandidateLayout::parse(&next_arg(
                        &mut args,
                        "--candidate-layout",
                    )?)?);
                }
                "--candidate-count" => {
                    candidate_count_override = Some(parse_usize(
                        &next_arg(&mut args, "--candidate-count")?,
                        "--candidate-count",
                    )?);
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
        if let Some(candidate_layout) = candidate_layout_override {
            config.candidate_layout = candidate_layout;
        }
        if let Some(candidate_count) = candidate_count_override {
            config.candidate_count = candidate_count;
        }
        validate_config(&config)?;
        validate_runtime_options(max_buffer_chars, inject_delay_ms, config.candidate_count)?;

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
            candidate_layout: CandidateLayout::Horizontal,
            candidate_count: DEFAULT_CANDIDATE_COUNT,
            candidate_select_keys: DEFAULT_CANDIDATE_SELECT_KEYS.to_owned(),
            candidate_page_next_key: DEFAULT_CANDIDATE_PAGE_NEXT_KEY.to_owned(),
            candidate_page_previous_key: DEFAULT_CANDIDATE_PAGE_PREVIOUS_KEY.to_owned(),
            english_commit_key: DEFAULT_ENGLISH_COMMIT_KEY.to_owned(),
        }
    }
}

impl AppConfig {
    fn candidate_interaction_key(&self, key: char) -> Option<CandidateInteractionKey> {
        if self.english_commit_key.starts_with(key) {
            return Some(CandidateInteractionKey::EnglishCommit);
        }
        if let Some(index) = self
            .candidate_select_keys
            .chars()
            .position(|candidate_key| candidate_key == key)
        {
            return Some(CandidateInteractionKey::Select(index));
        }
        if self.candidate_page_next_key.starts_with(key) {
            return Some(CandidateInteractionKey::NextPage);
        }
        if self.candidate_page_previous_key.starts_with(key) {
            return Some(CandidateInteractionKey::PreviousPage);
        }
        None
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

impl CandidateLayout {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "horizontal" | "h" => Ok(Self::Horizontal),
            "vertical" | "v" => Ok(Self::Vertical),
            _ => {
                bail!("invalid candidate layout {value:?}; expected \"horizontal\" or \"vertical\"")
            }
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Horizontal => "horizontal",
            Self::Vertical => "vertical",
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
            candidate_layout: file_config
                .candidate_layout
                .map(|value| CandidateLayout::parse(&value))
                .transpose()?
                .unwrap_or(defaults.candidate_layout),
            candidate_count: file_config
                .candidate_count
                .unwrap_or(defaults.candidate_count),
            candidate_select_keys: file_config
                .candidate_select_keys
                .unwrap_or(defaults.candidate_select_keys),
            candidate_page_next_key: file_config
                .candidate_page_next_key
                .unwrap_or(defaults.candidate_page_next_key),
            candidate_page_previous_key: file_config
                .candidate_page_previous_key
                .unwrap_or(defaults.candidate_page_previous_key),
            english_commit_key: file_config
                .english_commit_key
                .unwrap_or(defaults.english_commit_key),
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
    validate_interaction_keys(config)?;
    Ok(())
}

fn validate_interaction_keys(config: &AppConfig) -> Result<()> {
    let select_keys = config.candidate_select_keys.chars().collect::<Vec<_>>();
    if select_keys.is_empty() {
        bail!("candidate_select_keys must not be empty");
    }
    if select_keys.iter().any(|key| !key.is_ascii_digit()) {
        bail!("candidate_select_keys must contain only ASCII digits");
    }

    let mut unique_select_keys = select_keys.clone();
    unique_select_keys.sort_unstable();
    unique_select_keys.dedup();
    if unique_select_keys.len() != select_keys.len() {
        bail!("candidate_select_keys must not contain duplicate digits");
    }
    if config.candidate_count > select_keys.len() {
        bail!(
            "candidate_count ({}) exceeds candidate_select_keys capacity ({})",
            config.candidate_count,
            select_keys.len()
        );
    }

    let page_next =
        parse_interaction_key("candidate_page_next_key", &config.candidate_page_next_key)?;
    let page_previous = parse_interaction_key(
        "candidate_page_previous_key",
        &config.candidate_page_previous_key,
    )?;
    let english_commit = parse_interaction_key("english_commit_key", &config.english_commit_key)?;
    let named_keys = [
        ("candidate_page_next_key", page_next),
        ("candidate_page_previous_key", page_previous),
        ("english_commit_key", english_commit),
    ];

    for (name, key) in named_keys {
        if is_pinyin_char(key) {
            bail!("{name} must not be an ASCII pinyin character: {key:?}");
        }
        if select_keys.contains(&key) {
            bail!("{name} conflicts with candidate_select_keys: {key:?}");
        }
        if config.trigger_prefix.contains(key) || config.trigger_suffix.contains(key) {
            bail!("{name} conflicts with a trigger character: {key:?}");
        }
    }

    if page_next == page_previous || page_next == english_commit || page_previous == english_commit
    {
        bail!("candidate page keys and english_commit_key must be distinct");
    }

    for key in select_keys {
        if config.trigger_prefix.contains(key) || config.trigger_suffix.contains(key) {
            bail!("candidate_select_keys conflicts with a trigger character: {key:?}");
        }
    }

    Ok(())
}

fn parse_interaction_key(name: &str, value: &str) -> Result<char> {
    let mut chars = value.chars();
    let Some(key) = chars.next() else {
        bail!("{name} must contain exactly one character");
    };
    if chars.next().is_some() || key.is_control() {
        bail!("{name} must contain exactly one printable character");
    }
    Ok(key)
}

fn validate_runtime_options(
    max_buffer_chars: usize,
    inject_delay_ms: i32,
    candidate_count: usize,
) -> Result<()> {
    if max_buffer_chars < MIN_MAX_BUFFER_CHARS {
        bail!("max buffer chars must be at least {MIN_MAX_BUFFER_CHARS}");
    }

    if inject_delay_ms < 0 {
        bail!("inject delay must be greater than or equal to 0");
    }

    if candidate_count < MIN_CANDIDATE_COUNT {
        bail!("candidate count must be at least {MIN_CANDIDATE_COUNT}");
    }

    if candidate_count > MAX_CANDIDATE_COUNT {
        bail!("candidate count must be at most {MAX_CANDIDATE_COUNT}");
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
    tokenize_body(body).iter().any(|token| {
        matches!(
            token,
            Token::Pinyin(value) if value.chars().any(|ch| ch.is_ascii_alphabetic())
        )
    })
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
    println!(
        "candidate_layout: {}",
        options.config.candidate_layout.as_str()
    );
    println!("candidate_count: {}", options.config.candidate_count);
    println!(
        "candidate_select_keys: {:?}",
        options.config.candidate_select_keys
    );
    println!(
        "candidate_page_next_key: {:?}",
        options.config.candidate_page_next_key
    );
    println!(
        "candidate_page_previous_key: {:?}",
        options.config.candidate_page_previous_key
    );
    println!(
        "english_commit_key: {:?}",
        options.config.english_commit_key
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
    println!(
        "candidate_layout: {}",
        runtime.options.config.candidate_layout.as_str()
    );
    println!(
        "candidate_count: {}",
        runtime.options.config.candidate_count
    );
    println!(
        "candidate_select_keys: {:?}",
        runtime.options.config.candidate_select_keys
    );
    println!(
        "candidate_page_next_key: {:?}",
        runtime.options.config.candidate_page_next_key
    );
    println!(
        "candidate_page_previous_key: {:?}",
        runtime.options.config.candidate_page_previous_key
    );
    println!(
        "english_commit_key: {:?}",
        runtime.options.config.english_commit_key
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
extern "C" fn mac_event_callback(event: mac::InputEvent) -> i32 {
    let Some(runtime) = LISTENER_RUNTIME.get() else {
        return 0;
    };

    let runtime = runtime.lock();
    let Ok(mut runtime) = runtime else {
        eprintln!("listener runtime lock is poisoned");
        return 0;
    };

    match runtime.handle_event(event) {
        Ok(consume) => i32::from(consume),
        Err(error) => {
            eprintln!("listener error: {error:#}");
            0
        }
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
    let mut pending_input = String::new();

    for ch in body.chars() {
        let status = session.process_key(KeyEvent::new(ch as u32, 0));
        if matches!(status, KeyStatus::Pass) {
            output.push_str(&pending_input);
            pending_input.clear();
            output.push(ch);
        } else {
            pending_input.push(ch);
        }

        if let Some(commit) = session.commit() {
            output.push_str(commit.text());
            pending_input.clear();
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
    } else {
        output.push_str(&pending_input);
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
            buffer_visible: Vec::new(),
            config,
            max_buffer_chars,
            mode: CaptureMode::Idle,
            committed_output_chars: 0,
            hidden_prefix_backspaces_remaining: 0,
            candidate_page: 0,
            last_conversion: None,
        }
    }

    fn push_text(&mut self, text: &str) -> Option<CaptureAction> {
        self.push_text_with_visibility(text, true)
    }

    fn push_text_with_visibility(&mut self, text: &str, visible: bool) -> Option<CaptureAction> {
        let mut action = None;

        for ch in text.chars() {
            if ch.is_control() {
                continue;
            }

            if let Some(next_action) = self.push_char(ch, visible) {
                action = Some(next_action);
            }
        }

        action
    }

    fn append_text_without_actions(&mut self, text: &str, visible: bool) {
        self.last_conversion = None;
        self.candidate_page = 0;
        for ch in text.chars().filter(|ch| !ch.is_control()) {
            self.buffer.push(ch);
            self.buffer_visible.push(visible);
            self.trim_buffer();
        }
    }

    fn restore_after_failed_rewrite(&mut self, previous: CaptureState, text: &str, visible: bool) {
        *self = previous;
        self.append_text_without_actions(text, visible);
    }

    fn will_rewrite_after_text(&self, text: &str, visible: bool) -> bool {
        let mut next = self.clone();
        if visible {
            next.push_text(text).is_some()
        } else {
            next.push_text_with_visibility(text, false).is_some()
        }
    }

    fn is_active(&self) -> bool {
        self.mode == CaptureMode::Active
    }

    fn active_exit_backspace_count(&self) -> usize {
        self.config.trigger_prefix.chars().count().max(1)
    }

    fn should_consume_backspace(&self) -> bool {
        if !self.is_active() {
            return false;
        }

        if let Some(conversion) = &self.last_conversion {
            return conversion.inserted_text.is_empty();
        }

        if !self.buffer.is_empty() {
            return self.buffer_visible.last() == Some(&false);
        }

        self.committed_output_chars == 0 && self.hidden_prefix_backspaces_remaining > 0
    }

    fn backspace_affects_visible_host(&self) -> bool {
        if !self.is_active() || self.last_conversion.is_some() {
            return false;
        }

        if !self.buffer.is_empty() {
            return self.buffer_visible.last() == Some(&true);
        }

        self.committed_output_chars > 0
    }

    fn push_char(&mut self, ch: char, visible: bool) -> Option<CaptureAction> {
        self.last_conversion = None;
        self.buffer.push(ch);
        self.buffer_visible.push(visible);
        self.trim_buffer();

        match self.mode {
            CaptureMode::Idle => {
                if self.buffer.ends_with(&self.config.trigger_prefix) {
                    let trigger_text = self.config.trigger_prefix.clone();
                    let delete_chars = self.visible_tail_chars(trigger_text.chars().count());
                    self.buffer.clear();
                    self.buffer_visible.clear();
                    self.mode = CaptureMode::Active;
                    self.committed_output_chars = 0;
                    self.hidden_prefix_backspaces_remaining = self.active_exit_backspace_count();
                    self.candidate_page = 0;
                    self.last_conversion = None;
                    return Some(CaptureAction::StartSession(StartSessionAction {
                        trigger_text,
                        delete_chars,
                    }));
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

                self.candidate_page = 0;

                None
            }
        }
    }

    fn backspace(&mut self) {
        self.last_conversion = None;
        self.candidate_page = 0;
        if self.buffer.pop().is_some() {
            self.buffer_visible.pop();
            return;
        }

        if self.committed_output_chars > 0 {
            self.committed_output_chars -= 1;
            self.mode = CaptureMode::Active;
            return;
        }

        if self.hidden_prefix_backspaces_remaining > 0 {
            self.hidden_prefix_backspaces_remaining -= 1;
            self.mode = if self.hidden_prefix_backspaces_remaining > 0 {
                CaptureMode::Active
            } else {
                CaptureMode::Idle
            };
            return;
        }
        self.mode = CaptureMode::Idle;
    }

    fn delete_previous_word(&mut self) -> DeletePreviousWordOutcome {
        self.last_conversion = None;
        self.candidate_page = 0;
        let mut outcome = DeletePreviousWordOutcome {
            removed_chars: 0,
            removed_visible_chars: 0,
        };

        while self.buffer.chars().last().is_some_and(char::is_whitespace) {
            self.buffer.pop();
            let visible = self.buffer_visible.pop().unwrap_or(false);
            outcome.removed_chars += 1;
            outcome.removed_visible_chars += visible as usize;
        }

        while self.buffer.chars().last().is_some_and(is_pinyin_char) {
            self.buffer.pop();
            let visible = self.buffer_visible.pop().unwrap_or(false);
            outcome.removed_chars += 1;
            outcome.removed_visible_chars += visible as usize;
        }

        outcome
    }

    fn clear(&mut self) {
        self.buffer.clear();
        self.buffer_visible.clear();
        self.mode = CaptureMode::Idle;
        self.committed_output_chars = 0;
        self.hidden_prefix_backspaces_remaining = 0;
        self.candidate_page = 0;
        self.last_conversion = None;
    }

    fn record_conversion(
        &mut self,
        original_text: String,
        inserted_text: String,
        quote_state_before: QuoteState,
    ) {
        self.committed_output_chars = self
            .committed_output_chars
            .saturating_add(inserted_text.chars().count());
        self.last_conversion = (original_text != inserted_text).then_some(ReversibleConversion {
            original_text,
            inserted_text,
            quote_state_before,
        });
    }

    fn record_committed_literal(&mut self, text: &str) {
        self.committed_output_chars = self
            .committed_output_chars
            .saturating_add(text.chars().count());
        self.last_conversion = None;
    }

    fn restore_last_conversion(&mut self, host_backspace_applied: bool) -> Option<RestoreAction> {
        let conversion = self.last_conversion.take()?;
        let inserted_chars = conversion.inserted_text.chars().count();
        let delete_remaining_chars = inserted_chars.saturating_sub(host_backspace_applied as usize);

        self.committed_output_chars = self.committed_output_chars.saturating_sub(inserted_chars);
        self.buffer = conversion.original_text.clone();
        self.buffer_visible = vec![true; conversion.original_text.chars().count()];
        self.mode = CaptureMode::Active;
        self.candidate_page = 0;
        let replacement_text = conversion.original_text.clone();

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
            if !self.buffer_visible.is_empty() {
                self.buffer_visible.remove(0);
            }
        }
    }

    fn try_end_session(&mut self) -> Option<CaptureAction> {
        if !self.buffer.ends_with(&self.config.trigger_suffix) {
            return None;
        }

        let suffix_start = self.buffer.len() - self.config.trigger_suffix.len();
        let typed_text = self.buffer.clone();
        let body = self.active_body_from(&self.buffer[..suffix_start]);
        let delete_chars = self.current_segment_delete_chars();
        self.buffer.clear();
        self.buffer_visible.clear();
        self.mode = CaptureMode::Idle;
        self.committed_output_chars = 0;
        self.hidden_prefix_backspaces_remaining = 0;
        self.candidate_page = 0;
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
            let invisible_text = self
                .buffer
                .chars()
                .zip(self.buffer_visible.iter().copied())
                .filter_map(|(ch, visible)| (!visible).then_some(ch))
                .collect::<String>();
            self.committed_output_chars = self
                .committed_output_chars
                .saturating_add(self.buffer.chars().count());
            self.buffer.clear();
            self.buffer_visible.clear();
            self.candidate_page = 0;
            return (!invisible_text.is_empty()).then_some(CaptureAction::InsertLiteral(
                InsertLiteralAction {
                    text: invisible_text,
                },
            ));
        }

        let delete_chars = self.current_segment_delete_chars();
        self.buffer.clear();
        self.buffer_visible.clear();
        self.mode = CaptureMode::Active;
        self.candidate_page = 0;
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
        value.to_owned()
    }

    fn active_preview_body(&self) -> String {
        if self.mode == CaptureMode::Active {
            self.active_body_from(&self.buffer)
        } else {
            String::new()
        }
    }

    fn pending_pinyin(&self) -> Option<String> {
        if !self.is_active()
            || self.buffer.is_empty()
            || !self.buffer.chars().all(is_pinyin_char)
            || !self.buffer.chars().any(|ch| ch.is_ascii_alphabetic())
        {
            return None;
        }

        Some(self.buffer.clone())
    }

    fn take_pending_commit(&mut self, replacement_text: String) -> Option<PendingCommitAction> {
        let original_text = self.pending_pinyin()?;
        let delete_chars = self.current_segment_delete_chars();
        self.buffer.clear();
        self.buffer_visible.clear();
        self.candidate_page = 0;
        self.last_conversion = None;

        Some(PendingCommitAction {
            original_text,
            replacement_text,
            delete_chars,
        })
    }

    fn take_candidate_selection(
        &mut self,
        selection: CandidateSelection,
    ) -> Option<PendingCandidateAction> {
        let original_text = self.pending_pinyin()?;
        let delete_chars = self.current_segment_delete_chars();

        let (selected_text, remaining_pinyin, replacement_text) = match selection {
            CandidateSelection::Complete { text } => {
                if text.is_empty() {
                    return None;
                }
                (text.clone(), None, text)
            }
            CandidateSelection::Partial {
                selected_text,
                remaining_pinyin,
            } => {
                let original_chars = original_text.chars().count();
                let remaining_chars = remaining_pinyin.chars().count();
                if selected_text.is_empty()
                    || remaining_pinyin.is_empty()
                    || remaining_chars >= original_chars
                    || !original_text.ends_with(&remaining_pinyin)
                    || !remaining_pinyin.chars().all(is_pinyin_char)
                    || !remaining_pinyin.chars().any(|ch| ch.is_ascii_alphabetic())
                {
                    return None;
                }

                let replacement_text = format!("{selected_text}{remaining_pinyin}");
                (selected_text, Some(remaining_pinyin), replacement_text)
            }
        };

        match &remaining_pinyin {
            Some(remaining) => {
                self.buffer.clone_from(remaining);
                self.buffer_visible = vec![true; remaining.chars().count()];
            }
            None => {
                self.buffer.clear();
                self.buffer_visible.clear();
            }
        }
        self.candidate_page = 0;
        self.last_conversion = None;

        Some(PendingCandidateAction {
            original_text,
            selected_text,
            remaining_pinyin,
            replacement_text,
            delete_chars,
        })
    }

    fn record_partial_candidate(&mut self, selected_text: &str) {
        self.committed_output_chars = self
            .committed_output_chars
            .saturating_add(selected_text.chars().count());
        self.last_conversion = None;
    }

    fn set_candidate_page(&mut self, page_no: usize) {
        self.candidate_page = page_no;
    }

    fn is_commit_separator(&self, ch: char) -> bool {
        !is_pinyin_char(ch)
            && !self.config.trigger_prefix.contains(ch)
            && !self.config.trigger_suffix.contains(ch)
    }

    fn visible_buffer_chars(&self) -> usize {
        self.buffer_visible
            .iter()
            .copied()
            .filter(|visible| *visible)
            .count()
    }

    fn visible_tail_chars(&self, tail_chars: usize) -> usize {
        self.buffer_visible
            .iter()
            .rev()
            .take(tail_chars)
            .copied()
            .filter(|visible| *visible)
            .count()
    }

    fn current_segment_delete_chars(&self) -> usize {
        self.visible_buffer_chars()
    }
}

#[cfg(target_os = "macos")]
impl ListenerRuntime {
    fn handle_event(&mut self, event: mac::InputEvent) -> Result<bool> {
        if self.options.log_events {
            self.log_event(&event);
        }

        if event.status != mac::STATUS_PRESSED {
            return Ok(false);
        }

        if event.event_type == mac::EVENT_MOUSE {
            if event.is_rewrite_active() {
                if self.options.log_events {
                    println!("[listener] ignored mouse event during rewrite transaction");
                }
                return Ok(false);
            }
            self.clear_capture_context("mouse event");
            return Ok(false);
        }

        if event.event_type == mac::EVENT_CONTEXT {
            if event.is_rewrite_active() {
                if self.options.log_events {
                    println!("[listener] ignored context event during rewrite transaction");
                }
                return Ok(false);
            }
            let context_reason = event.text();
            if context_reason.is_empty() {
                self.clear_capture_context("context changed");
            } else {
                self.clear_capture_context(&format!("context changed: {context_reason}"));
            }
            return Ok(false);
        }

        if event.event_type != mac::EVENT_KEYBOARD {
            return Ok(false);
        }

        let input_source = event_input_source_fingerprint(&event);
        if self.skip_if_input_source_is_not_system(&input_source) {
            return Ok(false);
        }

        if self.clear_if_session_input_source_changed(&input_source) {
            return Ok(false);
        }

        if event.has_command_modifier() && matches!(event.key_code, mac::KEY_TAB | mac::KEY_GRAVE) {
            self.clear_capture_context("window switch shortcut");
            return Ok(false);
        }

        if event.has_text_modifier() {
            return self.handle_modified_key(event);
        }

        match event.key_code {
            mac::KEY_BACKSPACE => {
                let consume = self.capture.should_consume_backspace();
                let host_backspace_applied = !consume && !event.is_buffered_replay();
                let replay_needs_host_delete =
                    event.is_buffered_replay() && self.capture.backspace_affects_visible_host();
                if let Some(action) = self.capture.restore_last_conversion(host_backspace_applied) {
                    self.handle_restore(action)?;
                } else {
                    let was_active = self.capture.is_active();
                    let buffer_chars = self.capture.buffer.chars().count();
                    let buffer_tail = buffer_tail(&self.capture.buffer, 80);
                    self.capture.backspace();
                    if replay_needs_host_delete {
                        self.perform_rewrite(1, "")?;
                    }
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
                    if self.capture.is_active() {
                        self.refresh_candidate_panel_best_effort("backspace");
                    } else {
                        mac::hide_candidate_panel();
                    }
                }
                return Ok(consume);
            }
            mac::KEY_ENTER
            | mac::KEY_RETURN
            | mac::KEY_ESCAPE
            | mac::KEY_ARROW_LEFT
            | mac::KEY_ARROW_RIGHT
            | mac::KEY_ARROW_DOWN
            | mac::KEY_ARROW_UP => {
                self.clear_capture_context(&format!("key {}", event.key_code));
                return Ok(false);
            }
            _ => {}
        }

        let text = event.text();
        if text.is_empty() {
            if self.options.log_events {
                println!("[listener] key {} has empty text", event.key_code);
            }
            return Ok(false);
        }

        if let Some(consume) = self.handle_candidate_interaction(event.key_code, &text)? {
            return Ok(consume);
        }

        let was_active = self.capture.is_active();
        let consume_commit_space =
            is_pending_pinyin_commit_space(&self.capture, event.key_code, &text);
        let visible = !event.is_buffered_replay() && !consume_commit_space;
        let recovery_visible = visible || consume_commit_space;
        let will_rewrite = self.capture.will_rewrite_after_text(&text, visible);
        let rewrite_recovery = will_rewrite.then(|| {
            (
                self.capture.clone(),
                self.quote_state,
                self.session_input_source.clone(),
            )
        });
        let mut rewrite_transaction = will_rewrite.then(RewriteTransactionGuard::begin);
        let action = self.capture.push_text_with_visibility(&text, visible);
        let handled_action = action.is_some();
        self.record_session_input_source_if_opened(was_active, &input_source);

        if let Some(action) = action {
            if let Err(error) = self.handle_action(action) {
                if let Some((capture, quote_state, session_input_source)) = rewrite_recovery {
                    self.capture
                        .restore_after_failed_rewrite(capture, &text, recovery_visible);
                    self.quote_state = quote_state;
                    self.session_input_source = session_input_source;
                    let delete_chars = self.capture.visible_buffer_chars();
                    let replacement_text = self.capture.buffer.clone();
                    if delete_chars > 0 || !replacement_text.is_empty() {
                        if let Err(recovery_error) =
                            self.perform_rewrite(delete_chars, &replacement_text)
                        {
                            return Err(error.context(format!(
                                "raw rewrite recovery also failed: {recovery_error:#}"
                            )));
                        }
                        self.capture.buffer_visible =
                            vec![true; self.capture.buffer.chars().count()];
                        if let Some(transaction) = rewrite_transaction.as_mut() {
                            transaction.mark_committed();
                        }
                    }
                    self.refresh_candidate_panel_best_effort("failed rewrite recovery");
                }
                return Err(error);
            }
            if let Some(transaction) = rewrite_transaction.as_mut() {
                transaction.mark_committed();
            }
        } else if self.capture.is_active() {
            self.refresh_candidate_panel()?;
        } else if self.options.log_events {
            println!(
                "[listener] pushed text={text:?} buffer_chars={} buffer_tail={:?}",
                self.capture.buffer.chars().count(),
                buffer_tail(&self.capture.buffer, 80)
            );
        }

        Ok(consume_commit_space && handled_action)
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
        mac::hide_candidate_panel();
        if was_active {
            println!(
                "[listener] active session cleared reason={} buffer_chars={} buffer_tail={:?}",
                reason, buffer_chars, buffer_tail
            );
        } else if self.options.log_events {
            println!("[listener] {reason} -> clear inactive buffer");
        }
    }

    fn handle_modified_key(&mut self, event: mac::InputEvent) -> Result<bool> {
        if event.has_control_modifier() && event.key_code == mac::KEY_W {
            if self.capture.buffer.is_empty() {
                self.clear_capture_context("control-w with no pending raw text");
                return Ok(false);
            }

            let outcome = self.capture.delete_previous_word();
            match outcome.disposition() {
                DeletePreviousWordDisposition::ClearAndPassThrough => {
                    self.clear_capture_context("control-w had mixed or unmapped pending raw text");
                    return Ok(false);
                }
                DeletePreviousWordDisposition::Consume
                | DeletePreviousWordDisposition::PassThrough => {}
            }
            if self.capture.is_active() {
                self.refresh_candidate_panel_best_effort("control-w");
            }
            if self.options.log_events {
                println!(
                    "[listener] ctrl+w -> delete previous word buffer_chars={} removed_chars={} removed_visible_chars={}",
                    self.capture.buffer.chars().count(),
                    outcome.removed_chars,
                    outcome.removed_visible_chars
                );
            }
            return Ok(outcome.disposition() == DeletePreviousWordDisposition::Consume);
        }

        if event.key_code == mac::KEY_BACKSPACE
            || (event.has_control_modifier() && event.key_code == mac::KEY_H)
        {
            self.clear_capture_context("unmodeled modified deletion");
            return Ok(false);
        }

        if event.has_control_modifier() && event.key_code == mac::KEY_C {
            self.clear_capture_context("control-c shortcut");
            return Ok(false);
        }

        if event.has_command_modifier()
            && matches!(
                event.key_code,
                mac::KEY_A | mac::KEY_C | mac::KEY_V | mac::KEY_X | mac::KEY_Z
            )
        {
            self.clear_capture_context(&format!("command shortcut key {}", event.key_code));
            return Ok(false);
        }

        if self.options.log_events {
            println!(
                "[listener] modified key {} ignored for capture buffer",
                event.key_code
            );
        }
        Ok(false)
    }

    fn handle_candidate_interaction(&mut self, key_code: u32, text: &str) -> Result<Option<bool>> {
        let mut chars = text.chars();
        let Some(key) = chars.next() else {
            return Ok(None);
        };
        if chars.next().is_some() {
            return Ok(None);
        }

        let Some(pending) = self.capture.pending_pinyin() else {
            return Ok(None);
        };

        let Some(interaction) =
            candidate_interaction_for_event(&self.options.config, key_code, key)
        else {
            return Ok(None);
        };
        let is_space_selection = key_code == mac::KEY_SPACE && key == ' ';

        if interaction == CandidateInteractionKey::EnglishCommit {
            return match self.commit_pending_text(pending, "english") {
                Ok(()) => Ok(Some(true)),
                Err(error) => {
                    eprintln!(
                        "listener English commit error; falling back to literal key: {error:#}"
                    );
                    Ok(None)
                }
            };
        }

        let current = match preview_candidates(
            &self.options,
            &pending,
            self.options.config.candidate_count,
            self.capture.candidate_page,
        ) {
            Ok(preview) => preview,
            Err(error) => {
                eprintln!("listener candidate interaction preview error: {error:#}");
                return Ok(None);
            }
        };
        if current.candidates.is_empty() {
            if is_space_selection {
                return Ok(None);
            }
            let replacement = format!("{pending}{key}");
            return match self.commit_pending_literal_text(replacement, "no-candidate literal") {
                Ok(()) => Ok(Some(true)),
                Err(error) => {
                    eprintln!(
                        "listener no-candidate literal commit error; falling back to ordinary key: {error:#}"
                    );
                    Ok(None)
                }
            };
        }

        if let CandidateInteractionKey::Select(index) = interaction {
            if index >= current.candidates.len() {
                return Ok(None);
            }
            let selected = match select_candidate(&self.options, &pending, current.page_no, index) {
                Ok(Some(selected)) => selected,
                Ok(None) => return Ok(None),
                Err(error) => {
                    eprintln!(
                        "listener candidate selection error; falling back to literal key: {error:#}"
                    );
                    return Ok(None);
                }
            };
            let kind = format!("candidate page={} index={index}", current.page_no);
            return match self.commit_candidate_selection(selected, &kind) {
                Ok(()) => Ok(Some(true)),
                Err(error) => {
                    eprintln!(
                        "listener candidate commit error; falling back to literal key: {error:#}"
                    );
                    Ok(None)
                }
            };
        }

        let target_page = match interaction {
            CandidateInteractionKey::NextPage => current.page_no.saturating_add(1),
            CandidateInteractionKey::PreviousPage => current.page_no.saturating_sub(1),
            CandidateInteractionKey::Select(_) | CandidateInteractionKey::EnglishCommit => {
                unreachable!("selection and English commit returned above")
            }
        };
        if (interaction == CandidateInteractionKey::NextPage && current.is_last_page)
            || (interaction == CandidateInteractionKey::PreviousPage && current.page_no == 0)
        {
            if let Err(error) = self.show_candidate_preview(&current) {
                eprintln!("listener candidate boundary preview error: {error:#}");
            }
            return Ok(Some(true));
        }

        let target = match preview_candidates(
            &self.options,
            &pending,
            self.options.config.candidate_count,
            target_page,
        ) {
            Ok(target) => target,
            Err(error) => {
                eprintln!("listener candidate page error; falling back to literal key: {error:#}");
                return Ok(None);
            }
        };
        self.capture.set_candidate_page(target.page_no);
        println!(
            "[listener] candidate page {} -> {}",
            current.page_no, target.page_no
        );
        if let Err(error) = self.show_candidate_preview(&target) {
            eprintln!("listener candidate page preview error: {error:#}");
        }
        Ok(Some(true))
    }

    fn commit_candidate_selection(
        &mut self,
        selection: CandidateSelection,
        kind: &str,
    ) -> Result<()> {
        let previous = self.capture.clone();
        let action = self
            .capture
            .take_candidate_selection(selection)
            .context("candidate selection no longer matches the pending pinyin")?;
        let quote_state_before = self.quote_state;

        if let Err(error) = self.perform_rewrite(action.delete_chars, &action.replacement_text) {
            self.capture = previous;
            return Err(error);
        }

        println!(
            "[listener] {kind} commit delete_chars={} raw={:?} selected={:?} remaining={:?} output={:?}",
            action.delete_chars,
            action.original_text,
            action.selected_text,
            action.remaining_pinyin,
            action.replacement_text
        );
        if action.remaining_pinyin.is_some() {
            self.capture.record_partial_candidate(&action.selected_text);
        } else {
            self.capture.record_conversion(
                action.original_text,
                action.replacement_text,
                quote_state_before,
            );
        }
        self.refresh_candidate_panel_best_effort(kind);
        Ok(())
    }

    fn commit_pending_text(&mut self, replacement_text: String, kind: &str) -> Result<()> {
        let previous = self.capture.clone();
        let Some(action) = self.capture.take_pending_commit(replacement_text) else {
            return Ok(());
        };
        let quote_state_before = self.quote_state;

        if let Err(error) = self.perform_rewrite(action.delete_chars, &action.replacement_text) {
            self.capture = previous;
            return Err(error);
        }

        println!(
            "[listener] {kind} commit delete_chars={} raw={:?} output={:?}",
            action.delete_chars, action.original_text, action.replacement_text
        );
        self.capture.record_conversion(
            action.original_text,
            action.replacement_text,
            quote_state_before,
        );
        self.refresh_candidate_panel_best_effort(kind);
        Ok(())
    }

    fn commit_pending_literal_text(&mut self, replacement_text: String, kind: &str) -> Result<()> {
        let previous = self.capture.clone();
        let Some(action) = self.capture.take_pending_commit(replacement_text) else {
            return Ok(());
        };

        if let Err(error) = self.perform_rewrite(action.delete_chars, &action.replacement_text) {
            self.capture = previous;
            return Err(error);
        }

        println!(
            "[listener] {kind} delete_chars={} raw={:?} output={:?}",
            action.delete_chars, action.original_text, action.replacement_text
        );
        self.capture
            .record_committed_literal(&action.replacement_text);
        self.refresh_candidate_panel_best_effort(kind);
        Ok(())
    }

    fn handle_action(&mut self, action: CaptureAction) -> Result<()> {
        match action {
            CaptureAction::StartSession(action) => self.handle_start_session(action),
            CaptureAction::Convert(action) => self.handle_conversion(action),
            CaptureAction::InsertLiteral(action) => self.handle_literal_insertion(action),
            CaptureAction::EndSession(action) => {
                self.quote_state = QuoteState::default();
                self.session_input_source = None;
                let delete_count = action.delete_chars;
                println!(
                    "[listener] session ended delete_chars={} typed={:?}",
                    delete_count, action.typed_text
                );
                self.perform_rewrite(delete_count, &action.replacement_text)?;
                mac::hide_candidate_panel();
                Ok(())
            }
        }
    }

    fn handle_start_session(&mut self, action: StartSessionAction) -> Result<()> {
        println!(
            "[listener] active session marker hidden delete_chars={} trigger={:?}",
            action.delete_chars, action.trigger_text
        );
        self.perform_rewrite(action.delete_chars, "")?;
        self.refresh_candidate_panel_best_effort("session start");
        Ok(())
    }

    fn handle_literal_insertion(&mut self, action: InsertLiteralAction) -> Result<()> {
        self.perform_rewrite(0, &action.text)?;
        self.refresh_candidate_panel_best_effort("buffered literal replay");
        Ok(())
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
        let injected_output = output.output.clone();
        println!(
            "[listener] injecting delete_chars={} output_chars={}",
            delete_count,
            injected_output.chars().count()
        );

        self.perform_rewrite(delete_count, &injected_output)?;
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
        if action.stay_active {
            self.refresh_candidate_panel_best_effort("conversion");
        } else {
            mac::hide_candidate_panel();
        }

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
        self.perform_rewrite(action.delete_remaining_chars, &action.replacement_text)?;
        self.refresh_candidate_panel_best_effort("conversion restore");
        Ok(())
    }

    fn perform_rewrite(&self, delete_chars: usize, replacement_text: &str) -> Result<()> {
        if self.options.log_events {
            println!(
                "[listener] rewrite operation queued delete_chars={} replacement_chars={} delay_ms={}",
                delete_chars,
                replacement_text.chars().count(),
                self.options.inject_delay_ms
            );
        }

        let mut transaction = RewriteTransactionGuard::begin();
        mac::commit_rewrite_transaction(
            delete_chars,
            replacement_text,
            self.options.inject_delay_ms,
        )?;
        transaction.mark_committed();
        Ok(())
    }

    fn refresh_candidate_panel_best_effort(&self, context: &str) {
        if let Err(error) = self.refresh_candidate_panel() {
            eprintln!("listener candidate panel error after {context}: {error:#}");
        }
    }

    fn refresh_candidate_panel(&self) -> Result<()> {
        if !self.capture.is_active() {
            mac::hide_candidate_panel();
            return Ok(());
        }

        let preview = preview_candidates(
            &self.options,
            &self.capture.active_preview_body(),
            self.options.config.candidate_count,
            self.capture.candidate_page,
        )?;
        self.show_candidate_preview(&preview)
    }

    fn show_candidate_preview(&self, preview: &CandidatePreview) -> Result<()> {
        let candidates = self
            .options
            .config
            .candidate_select_keys
            .chars()
            .zip(preview.candidates.iter())
            .map(|(key, candidate)| format!("{key}. {candidate}"))
            .collect::<Vec<_>>();
        mac::update_candidate_panel(
            &preview.preedit,
            &candidates,
            self.options.config.candidate_layout,
        )?;
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

#[cfg(target_os = "macos")]
fn candidate_interaction_for_event(
    config: &AppConfig,
    key_code: u32,
    key: char,
) -> Option<CandidateInteractionKey> {
    if key_code == mac::KEY_SPACE && key == ' ' {
        Some(CandidateInteractionKey::Select(0))
    } else {
        config.candidate_interaction_key(key)
    }
}

#[cfg(target_os = "macos")]
fn is_pending_pinyin_commit_space(capture: &CaptureState, key_code: u32, text: &str) -> bool {
    key_code == mac::KEY_SPACE && text == " " && capture.pending_pinyin().is_some()
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

fn preview_candidates(
    options: &Options,
    body: &str,
    candidate_count: usize,
    requested_page: usize,
) -> Result<CandidatePreview> {
    let Some((mut session, raw)) = open_candidate_session(options, body, requested_page)? else {
        return Ok(CandidatePreview {
            preedit: "Rime active".to_owned(),
            candidates: Vec::new(),
            page_no: 0,
            is_last_page: true,
        });
    };

    let mut preedit = raw.clone();
    let mut candidates = Vec::new();
    let mut page_no = 0;
    let mut is_last_page = true;
    if let Some(context) = session.context() {
        let composition = context.composition();
        if let Some(value) = composition.preedit {
            preedit = value.to_owned();
        }
        let menu = context.menu();
        page_no = menu.page_no;
        is_last_page = menu.is_last_page;
        candidates = menu
            .candidates
            .iter()
            .take(candidate_count)
            .map(|candidate| candidate.text.to_owned())
            .collect();
    }

    session.close().context("failed to close Rime session")?;
    Ok(CandidatePreview {
        preedit,
        candidates,
        page_no,
        is_last_page,
    })
}

fn open_candidate_session(
    options: &Options,
    body: &str,
    requested_page: usize,
) -> Result<Option<(Session, String)>> {
    let raw = pending_pinyin_for_preview(body);
    if raw.is_empty() {
        return Ok(None);
    }

    let normalized = normalize_pinyin_run(&raw);
    if !normalized.chars().any(|ch| ch.is_ascii_alphabetic()) {
        return Ok(None);
    }

    let mut session = create_selected_session(&options.schema)?;
    for ch in normalized.chars() {
        let status = session.process_key(KeyEvent::new(ch as u32, 0));
        if matches!(status, KeyStatus::Pass) {
            session.close().context("failed to close Rime session")?;
            return Ok(None);
        }
    }

    for _ in 0..requested_page {
        if !rime_direct::change_page(session.session_id, false)? {
            break;
        }
    }

    Ok(Some((session, raw)))
}

fn select_candidate(
    options: &Options,
    body: &str,
    page_no: usize,
    index: usize,
) -> Result<Option<CandidateSelection>> {
    let Some((mut session, raw)) = open_candidate_session(options, body, page_no)? else {
        return Ok(None);
    };

    let outcome = (|| -> Result<Option<CandidateSelection>> {
        let selected_text = session.context().and_then(|context| {
            context
                .menu()
                .candidates
                .get(index)
                .map(|candidate| candidate.text.to_owned())
        });
        let Some(selected_text) = selected_text else {
            return Ok(None);
        };

        if !rime_direct::select_candidate_on_current_page(session.session_id, index)? {
            return Ok(None);
        }

        if let Some(commit) = session.commit() {
            return Ok(Some(CandidateSelection::Complete {
                text: commit.text().to_owned(),
            }));
        }

        let context = session
            .context()
            .context("Rime returned neither a commit nor a post-selection context")?;
        let composition = context.composition();
        let preedit = composition
            .preedit
            .context("Rime partial selection has no preedit")?;
        let remaining_preedit = preedit.get(composition.sel_start..).with_context(|| {
            format!(
                "Rime partial selection boundary {} is invalid for preedit {preedit:?}",
                composition.sel_start
            )
        })?;
        let remaining_pinyin = remaining_raw_pinyin(&raw, remaining_preedit).with_context(|| {
            format!(
                "Rime partial selection made no valid progress: raw={raw:?} preedit={preedit:?} boundary={}",
                composition.sel_start
            )
        })?;
        if context.menu().candidates.is_empty() {
            bail!("Rime partial selection left no candidates for {remaining_pinyin:?}")
        }

        Ok(Some(CandidateSelection::Partial {
            selected_text,
            remaining_pinyin,
        }))
    })();

    let close_result = session.close().context("failed to close Rime session");
    let outcome = outcome?;
    close_result?;
    Ok(outcome)
}

fn remaining_raw_pinyin(original: &str, remaining_preedit: &str) -> Option<String> {
    let remaining_letters = remaining_preedit
        .chars()
        .filter(|ch| ch.is_ascii_alphabetic())
        .map(|ch| ch.to_ascii_lowercase())
        .collect::<String>();
    let original_letters = original
        .chars()
        .filter(|ch| ch.is_ascii_alphabetic())
        .map(|ch| ch.to_ascii_lowercase())
        .collect::<String>();
    if remaining_letters.is_empty() || remaining_letters == original_letters {
        return None;
    }

    original.char_indices().find_map(|(index, _)| {
        let suffix = original[index..].trim_start_matches('\'');
        let suffix_letters = suffix
            .chars()
            .filter(|ch| ch.is_ascii_alphabetic())
            .map(|ch| ch.to_ascii_lowercase())
            .collect::<String>();
        (suffix_letters == remaining_letters && !suffix.is_empty()).then(|| suffix.to_owned())
    })
}

fn pending_pinyin_for_preview(body: &str) -> String {
    tokenize_body(body)
        .into_iter()
        .rev()
        .find_map(|token| match token {
            Token::Pinyin(value) if value.chars().any(|ch| ch.is_ascii_alphabetic()) => Some(value),
            _ => None,
        })
        .unwrap_or_default()
}

fn convert_pinyin_run(options: &Options, raw: &str, normalized: &str) -> Result<ConvertedSegment> {
    let mut session = create_selected_session(&options.schema)?;

    for ch in normalized.chars() {
        let status = session.process_key(KeyEvent::new(ch as u32, 0));
        if matches!(status, KeyStatus::Pass) {
            session.close().context("failed to close Rime session")?;
            return Ok(raw_fallback_segment(raw, normalized, raw));
        }
    }

    let segment = if let Some(context) = session.context() {
        let composition = context.composition();
        let preedit = composition.preedit.unwrap_or(raw).to_owned();
        match context.menu().candidates.first() {
            Some(first) => ConvertedSegment {
                raw: raw.to_owned(),
                normalized: normalized.to_owned(),
                preedit,
                first: first.text.to_owned(),
            },
            None => raw_fallback_segment(raw, normalized, &preedit),
        }
    } else {
        raw_fallback_segment(raw, normalized, raw)
    };

    session.close().context("failed to close Rime session")?;
    Ok(segment)
}

fn raw_fallback_segment(raw: &str, normalized: &str, preedit: &str) -> ConvertedSegment {
    ConvertedSegment {
        raw: raw.to_owned(),
        normalized: normalized.to_owned(),
        preedit: preedit.to_owned(),
        first: raw.to_owned(),
    }
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
        .filter(|ch| ch.is_ascii_alphabetic() || *ch == '\'')
        .map(|ch| ch.to_ascii_lowercase())
        .collect()
}

fn is_pinyin_char(ch: char) -> bool {
    ch.is_ascii_alphabetic() || ch == '\''
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
        ":" => "：".to_owned(),
        ";" => "；".to_owned(),
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
    println!(
        "candidate_layout: {}",
        options.config.candidate_layout.as_str()
    );
    println!("candidate_count: {}", options.config.candidate_count);
    println!(
        "candidate_select_keys: {:?}",
        options.config.candidate_select_keys
    );
    println!(
        "candidate_page_next_key: {:?}",
        options.config.candidate_page_next_key
    );
    println!(
        "candidate_page_previous_key: {:?}",
        options.config.candidate_page_previous_key
    );
    println!(
        "english_commit_key: {:?}",
        options.config.english_commit_key
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
--candidate-layout <LAYOUT> Candidate layout: horizontal or vertical [env: RIME_POC_CANDIDATE_LAYOUT] [default: horizontal]\n  \
--candidate-count <N>    Candidate count shown in the UI, 1-10 [env: RIME_POC_CANDIDATE_COUNT] [default: 5]\n  \
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    static REWRITE_ABORT_COUNT: AtomicUsize = AtomicUsize::new(0);

    fn count_rewrite_abort() {
        REWRITE_ABORT_COUNT.fetch_add(1, Ordering::SeqCst);
    }

    fn test_capture_state() -> CaptureState {
        CaptureState::new(AppConfig::default(), 128)
    }

    #[test]
    fn extracts_body_with_configured_triggers() {
        let config = AppConfig {
            trigger_prefix: "[[".to_owned(),
            trigger_suffix: "]]".to_owned(),
            ..AppConfig::default()
        };

        let body = extract_body("[[woyaoceshi]]", &config, false).unwrap();

        assert_eq!(body, "woyaoceshi");
    }

    #[test]
    fn rejects_reserved_trigger_chars() {
        let config = AppConfig {
            trigger_prefix: ";;".to_owned(),
            trigger_suffix: "?".to_owned(),
            ..AppConfig::default()
        };

        let error = validate_config(&config).unwrap_err().to_string();

        assert!(error.contains("trigger_suffix"));

        let config = AppConfig {
            trigger_prefix: "!".to_owned(),
            trigger_suffix: ";;".to_owned(),
            ..AppConfig::default()
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

        assert_eq!(
            capture.push_text(";;"),
            Some(CaptureAction::StartSession(StartSessionAction {
                trigger_text: ";;".to_owned(),
                delete_chars: 2,
            }))
        );

        assert_eq!(capture.mode, CaptureMode::Active);
        assert_eq!(capture.committed_output_chars, 0);
        assert_eq!(capture.hidden_prefix_backspaces_remaining, 2);
        assert!(capture.buffer.is_empty());
    }

    #[test]
    fn entering_active_state_isolates_previous_idle_buffer() {
        let mut capture = test_capture_state();

        assert_eq!(
            capture.push_text("old text ;;"),
            Some(CaptureAction::StartSession(StartSessionAction {
                trigger_text: ";;".to_owned(),
                delete_chars: 2,
            }))
        );

        assert_eq!(capture.mode, CaptureMode::Active);
        assert!(capture.buffer.is_empty());

        let action = capture.push_text("ceshi ");

        assert_eq!(
            action,
            Some(CaptureAction::Convert(ConversionAction {
                typed_text: "ceshi ".to_owned(),
                body: "ceshi".to_owned(),
                restore_text: "ceshi".to_owned(),
                delete_chars: "ceshi ".chars().count(),
                stay_active: true,
            }))
        );
    }

    #[test]
    fn space_triggers_incremental_conversion_without_restoring_space() {
        let mut capture = test_capture_state();
        capture.push_text(";;");
        capture.push_text("woyaoceshi");

        assert!(capture.will_rewrite_after_text(" ", false));
        let action = capture.push_text_with_visibility(" ", false);

        assert_eq!(
            action,
            Some(CaptureAction::Convert(ConversionAction {
                typed_text: "woyaoceshi ".to_owned(),
                body: "woyaoceshi".to_owned(),
                restore_text: "woyaoceshi".to_owned(),
                delete_chars: "woyaoceshi".chars().count(),
                stay_active: true,
            }))
        );
        assert_eq!(capture.mode, CaptureMode::Active);
        assert_eq!(capture.committed_output_chars, 0);
        assert_eq!(capture.hidden_prefix_backspaces_remaining, 2);
        assert!(capture.buffer.is_empty());
    }

    #[test]
    fn buffered_replay_conversion_does_not_delete_host_text() {
        let mut capture = test_capture_state();
        capture.push_text(";;");

        let action = capture.push_text_with_visibility("woyaoceshi ", false);

        assert_eq!(
            action,
            Some(CaptureAction::Convert(ConversionAction {
                typed_text: "woyaoceshi ".to_owned(),
                body: "woyaoceshi".to_owned(),
                restore_text: "woyaoceshi".to_owned(),
                delete_chars: 0,
                stay_active: true,
            }))
        );
        assert_eq!(capture.mode, CaptureMode::Active);
        assert!(capture.buffer.is_empty());
        assert!(capture.buffer_visible.is_empty());
    }

    #[test]
    fn punctuation_triggers_incremental_conversion_and_keeps_separator() {
        let mut capture = test_capture_state();
        capture.push_text(";;");

        let action = capture.push_text("woyaoceshi,");

        assert_eq!(
            action,
            Some(CaptureAction::Convert(ConversionAction {
                typed_text: "woyaoceshi,".to_owned(),
                body: "woyaoceshi,".to_owned(),
                restore_text: "woyaoceshi,".to_owned(),
                delete_chars: "woyaoceshi,".chars().count(),
                stay_active: true,
            }))
        );
    }

    #[test]
    fn punctuation_separator_is_committed_as_chinese_punctuation() {
        let mut quote_state = QuoteState::default();

        assert_eq!(map_separator(",", &mut quote_state), "，");
        assert_eq!(map_separator(".", &mut quote_state), "。");
        assert_eq!(map_separator(":", &mut quote_state), "：");
        assert_eq!(map_separator(";", &mut quote_state), "；");

        let mut capture = test_capture_state();
        capture.push_text(";;");
        let action = capture.push_text("woyaoceshi ");
        assert!(matches!(
            action,
            Some(CaptureAction::Convert(ConversionAction { body, .. })) if body == "woyaoceshi"
        ));
    }

    #[test]
    fn arbitrary_non_pinyin_separator_triggers_incremental_conversion() {
        let mut capture = test_capture_state();
        capture.push_text(";;");

        let action = capture.push_text("woyaoceshi:");

        assert_eq!(
            action,
            Some(CaptureAction::Convert(ConversionAction {
                typed_text: "woyaoceshi:".to_owned(),
                body: "woyaoceshi:".to_owned(),
                restore_text: "woyaoceshi:".to_owned(),
                delete_chars: "woyaoceshi:".chars().count(),
                stay_active: true,
            }))
        );
    }

    #[test]
    fn trigger_characters_do_not_trigger_incremental_conversion() {
        let mut capture = test_capture_state();
        capture.push_text(";;");

        assert!(!capture.will_rewrite_after_text("woyaoceshi;", true));
        let action = capture.push_text("woyaoceshi;");

        assert_eq!(action, None);
        assert_eq!(capture.mode, CaptureMode::Active);
        assert_eq!(capture.buffer, "woyaoceshi;");
    }

    #[test]
    fn non_pinyin_literals_do_not_enter_an_empty_active_buffer() {
        let mut capture = test_capture_state();
        capture.push_text(";;");

        for literal in ["1", "=", "-", "`", "!"] {
            assert_eq!(capture.push_text(literal), None);
            assert!(
                capture.buffer.is_empty(),
                "literal {literal:?} must stay outside the candidate buffer"
            );
        }

        assert!(!is_pinyin_char('1'));
        assert!(is_pinyin_char('n'));
        assert!(is_pinyin_char('\''));
    }

    #[test]
    fn shifted_punctuation_converts_pending_pinyin() {
        let mut capture = test_capture_state();
        capture.push_text(";;ni");

        let action = capture.push_text("!");

        assert!(matches!(
            action,
            Some(CaptureAction::Convert(ConversionAction { body, .. })) if body == "ni!"
        ));
    }

    #[test]
    fn candidate_commit_clears_pending_text_and_resets_page() {
        let mut capture = test_capture_state();
        capture.push_text(";;ni");
        capture.set_candidate_page(2);

        let action = capture.take_pending_commit("你".to_owned());

        assert_eq!(
            action,
            Some(PendingCommitAction {
                original_text: "ni".to_owned(),
                replacement_text: "你".to_owned(),
                delete_chars: 2,
            })
        );
        assert!(capture.buffer.is_empty());
        assert_eq!(capture.candidate_page, 0);
        assert!(capture.is_active());
    }

    #[test]
    fn partial_candidate_selection_preserves_remaining_pinyin() {
        let mut capture = test_capture_state();
        capture.push_text(";;woshizhongguoren");
        capture.set_candidate_page(2);

        let action = capture
            .take_candidate_selection(CandidateSelection::Partial {
                selected_text: "我是".to_owned(),
                remaining_pinyin: "zhongguoren".to_owned(),
            })
            .unwrap();

        assert_eq!(
            action,
            PendingCandidateAction {
                original_text: "woshizhongguoren".to_owned(),
                selected_text: "我是".to_owned(),
                remaining_pinyin: Some("zhongguoren".to_owned()),
                replacement_text: "我是zhongguoren".to_owned(),
                delete_chars: "woshizhongguoren".chars().count(),
            }
        );
        assert_eq!(capture.buffer, "zhongguoren");
        assert_eq!(capture.buffer_visible, vec![true; "zhongguoren".len()]);
        assert_eq!(capture.candidate_page, 0);
        assert!(capture.is_active());
        assert_eq!(capture.committed_output_chars, 0);

        capture.record_partial_candidate(&action.selected_text);
        assert_eq!(capture.committed_output_chars, 2);
        assert_eq!(capture.last_conversion, None);

        capture.backspace();
        assert_eq!(capture.buffer, "zhongguore");
        assert_eq!(capture.committed_output_chars, 2);
    }

    #[test]
    fn repeated_partial_candidate_selection_finishes_the_remaining_pinyin() {
        let mut capture = test_capture_state();
        capture.push_text(";;woshizhongguoren");

        let first = capture
            .take_candidate_selection(CandidateSelection::Partial {
                selected_text: "我是".to_owned(),
                remaining_pinyin: "zhongguoren".to_owned(),
            })
            .unwrap();
        capture.record_partial_candidate(&first.selected_text);

        let second = capture
            .take_candidate_selection(CandidateSelection::Partial {
                selected_text: "中国".to_owned(),
                remaining_pinyin: "ren".to_owned(),
            })
            .unwrap();
        assert_eq!(second.original_text, "zhongguoren");
        assert_eq!(second.replacement_text, "中国ren");
        assert_eq!(second.delete_chars, "zhongguoren".chars().count());
        capture.record_partial_candidate(&second.selected_text);

        let final_action = capture
            .take_candidate_selection(CandidateSelection::Complete {
                text: "人".to_owned(),
            })
            .unwrap();
        assert_eq!(final_action.original_text, "ren");
        assert_eq!(final_action.replacement_text, "人");
        assert_eq!(final_action.remaining_pinyin, None);
        assert!(capture.buffer.is_empty());
        capture.record_conversion(
            final_action.original_text,
            final_action.replacement_text,
            QuoteState::default(),
        );

        assert_eq!(capture.committed_output_chars, 5);
        assert!(capture.is_active());
    }

    #[test]
    fn candidate_selection_suffix_preserves_internal_apostrophe() {
        assert_eq!(
            remaining_raw_pinyin("woshizhongguoren", "zhong guo ren"),
            Some("zhongguoren".to_owned())
        );
        assert_eq!(
            remaining_raw_pinyin("woxi'anren", "xi an ren"),
            Some("xi'anren".to_owned())
        );
        assert_eq!(remaining_raw_pinyin("ni", "ni"), None);
        assert_eq!(remaining_raw_pinyin("ni", ""), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn space_selects_first_candidate_on_current_page() {
        let config = AppConfig::default();

        assert_eq!(
            candidate_interaction_for_event(&config, mac::KEY_SPACE, ' '),
            Some(CandidateInteractionKey::Select(0))
        );
        assert_eq!(
            candidate_interaction_for_event(&config, 18, '1'),
            Some(CandidateInteractionKey::Select(0))
        );
        assert_eq!(
            candidate_interaction_for_event(
                &config,
                27,
                DEFAULT_CANDIDATE_PAGE_NEXT_KEY.chars().next().unwrap()
            ),
            Some(CandidateInteractionKey::NextPage)
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn space_commit_is_invisible_and_deletes_only_pending_pinyin() {
        let mut capture = test_capture_state();
        capture.push_text(";;ni");

        assert!(is_pending_pinyin_commit_space(
            &capture,
            mac::KEY_SPACE,
            " "
        ));
        let action = capture.push_text_with_visibility(" ", false);

        assert!(matches!(
            action,
            Some(CaptureAction::Convert(ConversionAction {
                typed_text,
                body,
                delete_chars: 2,
                stay_active: true,
                ..
            })) if typed_text == "ni " && body == "ni"
        ));

        let mut empty = test_capture_state();
        empty.push_text(";;");
        assert!(!is_pending_pinyin_commit_space(&empty, mac::KEY_SPACE, " "));
    }

    #[test]
    fn english_commit_keeps_raw_pending_text() {
        let mut capture = test_capture_state();
        capture.push_text(";;hello");

        let action = capture.take_pending_commit("hello".to_owned()).unwrap();
        capture.record_conversion(
            action.original_text,
            action.replacement_text,
            QuoteState::default(),
        );

        assert_eq!(capture.committed_output_chars, 5);
        assert_eq!(capture.last_conversion, None);
        assert!(capture.is_active());
    }

    #[test]
    fn invalid_pinyin_followed_by_digit_commits_as_raw_literal_text() {
        let mut capture = test_capture_state();
        capture.push_text(";;vke");

        let action = capture.push_text("1");

        assert!(matches!(
            action,
            Some(CaptureAction::Convert(ConversionAction { body, .. })) if body == "vke1"
        ));
        assert!(capture.buffer.is_empty());
    }

    #[test]
    fn no_candidate_control_key_can_be_recorded_as_raw_literal_output() {
        let mut capture = test_capture_state();
        capture.push_text(";;vke");

        let action = capture.take_pending_commit("vke-".to_owned()).unwrap();
        capture.record_committed_literal(&action.replacement_text);

        assert_eq!(action.delete_chars, 3);
        assert_eq!(capture.committed_output_chars, 4);
        assert_eq!(capture.last_conversion, None);
        assert!(capture.buffer.is_empty());
        assert!(capture.is_active());
    }

    #[test]
    fn buffered_literal_outside_pending_pinyin_is_reinjected() {
        let mut capture = test_capture_state();
        capture.push_text(";;");

        let action = capture.push_text_with_visibility("1", false);

        assert_eq!(
            action,
            Some(CaptureAction::InsertLiteral(InsertLiteralAction {
                text: "1".to_owned(),
            }))
        );
        assert!(capture.buffer.is_empty());
        assert_eq!(capture.committed_output_chars, 1);
    }

    #[test]
    fn visible_literal_deletion_preserves_hidden_prefix_budget() {
        let mut capture = test_capture_state();
        capture.push_text(";;");

        assert_eq!(capture.push_text("1"), None);
        assert_eq!(capture.committed_output_chars, 1);
        assert_eq!(capture.hidden_prefix_backspaces_remaining, 2);

        capture.backspace();

        assert!(capture.is_active());
        assert_eq!(capture.committed_output_chars, 0);
        assert_eq!(capture.hidden_prefix_backspaces_remaining, 2);
        assert!(capture.should_consume_backspace());
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
        assert_eq!(capture.restore_last_conversion(true), None);
    }

    #[test]
    fn delete_previous_word_keeps_active_state() {
        let mut capture = test_capture_state();
        capture.buffer = "woyao ceshi".to_owned();
        capture.buffer_visible = vec![true; capture.buffer.chars().count()];
        capture.mode = CaptureMode::Active;

        let first = capture.delete_previous_word();

        assert_eq!(capture.mode, CaptureMode::Active);
        assert_eq!(capture.buffer, "woyao ");
        assert_eq!(first.removed_chars, 5);
        assert_eq!(first.removed_visible_chars, 5);

        let second = capture.delete_previous_word();

        assert_eq!(capture.mode, CaptureMode::Active);
        assert!(capture.buffer.is_empty());
        assert_eq!(second.removed_chars, 6);
        assert_eq!(second.removed_visible_chars, 6);
    }

    #[test]
    fn backspace_consumes_invisible_buffered_replay_text() {
        let mut capture = test_capture_state();
        capture.push_text(";;");
        capture.append_text_without_actions("ab", false);

        assert!(capture.should_consume_backspace());
        assert!(!capture.backspace_affects_visible_host());
        capture.backspace();
        assert_eq!(capture.buffer, "a");
        assert!(capture.should_consume_backspace());

        capture.backspace();
        assert!(capture.buffer.is_empty());
        assert!(capture.should_consume_backspace());
        assert_eq!(capture.hidden_prefix_backspaces_remaining, 2);
    }

    #[test]
    fn replayed_backspace_requests_host_deletion_for_visible_state() {
        let mut capture = test_capture_state();
        capture.push_text(";;visible");
        assert!(capture.backspace_affects_visible_host());

        capture.buffer.clear();
        capture.buffer_visible.clear();
        capture.committed_output_chars = 2;
        assert!(capture.backspace_affects_visible_host());

        capture.committed_output_chars = 0;
        assert!(!capture.backspace_affects_visible_host());
        assert!(capture.should_consume_backspace());
    }

    #[test]
    fn deleting_an_invisible_previous_word_requests_event_consumption() {
        let mut capture = test_capture_state();
        capture.push_text(";;");
        capture.append_text_without_actions("buffered ", false);

        let outcome = capture.delete_previous_word();

        assert_eq!(outcome.removed_chars, "buffered ".chars().count());
        assert_eq!(outcome.removed_visible_chars, 0);
        assert!(capture.buffer.is_empty());
        assert!(capture.is_active());
    }

    #[test]
    fn mixed_visibility_previous_word_clears_instead_of_guessing_host_deletion() {
        let outcome = DeletePreviousWordOutcome {
            removed_chars: 5,
            removed_visible_chars: 2,
        };

        assert_eq!(
            outcome.disposition(),
            DeletePreviousWordDisposition::ClearAndPassThrough
        );
    }

    #[test]
    fn suffix_still_converts_pending_body_and_ends_session() {
        let mut capture = test_capture_state();

        let action = capture.push_text(";;woyaoceshi;;");

        assert_eq!(
            action,
            Some(CaptureAction::Convert(ConversionAction {
                typed_text: "woyaoceshi;;".to_owned(),
                body: "woyaoceshi".to_owned(),
                restore_text: "woyaoceshi".to_owned(),
                delete_chars: "woyaoceshi;;".chars().count(),
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
                delete_chars: ";;".chars().count(),
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
                delete_chars: "ceshi;;".chars().count(),
                stay_active: false,
            }))
        );
        assert_eq!(capture.mode, CaptureMode::Idle);
    }

    #[test]
    fn backspace_after_conversion_restores_original_text() {
        let mut capture = test_capture_state();
        let quote_state_before = QuoteState { next_is_open: true };
        capture.mode = CaptureMode::Active;
        capture.record_conversion(
            "woyaoceshi".to_owned(),
            "我要测试".to_owned(),
            quote_state_before,
        );

        let restore = capture.restore_last_conversion(true);

        assert_eq!(
            restore,
            Some(RestoreAction {
                original_text: "woyaoceshi".to_owned(),
                replacement_text: "woyaoceshi".to_owned(),
                delete_remaining_chars: 3,
                quote_state_before,
            })
        );
        assert_eq!(capture.mode, CaptureMode::Active);
        assert_eq!(capture.buffer, "woyaoceshi");
        assert_eq!(capture.committed_output_chars, 0);
    }

    #[test]
    fn buffered_replay_backspace_restores_all_inserted_characters() {
        let mut capture = test_capture_state();
        capture.mode = CaptureMode::Active;
        capture.record_conversion("ceshi".to_owned(), "测试".to_owned(), QuoteState::default());

        let restore = capture.restore_last_conversion(false).unwrap();

        assert_eq!(restore.delete_remaining_chars, 2);
        assert_eq!(restore.replacement_text, "ceshi");
    }

    #[test]
    fn empty_conversion_output_consumes_backspace_before_restore() {
        let mut capture = test_capture_state();
        capture.mode = CaptureMode::Active;
        capture.record_conversion("vke".to_owned(), String::new(), QuoteState::default());

        assert!(capture.should_consume_backspace());
        let restore = capture.restore_last_conversion(false).unwrap();

        assert_eq!(restore.delete_remaining_chars, 0);
        assert_eq!(restore.replacement_text, "vke");
    }

    #[test]
    fn deleting_multiple_committed_segments_preserves_hidden_prefix_budget() {
        let mut capture = test_capture_state();
        capture.push_text(";;");

        capture.push_text("woyao ");
        capture.record_conversion("woyao".to_owned(), "我要".to_owned(), QuoteState::default());
        capture.push_text("ceshi ");
        capture.record_conversion("ceshi".to_owned(), "测试".to_owned(), QuoteState::default());

        let restored = capture.restore_last_conversion(true).unwrap();
        assert_eq!(restored.replacement_text, "ceshi");
        assert_eq!(capture.committed_output_chars, 2);
        assert_eq!(capture.hidden_prefix_backspaces_remaining, 2);

        for _ in 0.."ceshi".chars().count() {
            capture.backspace();
        }
        assert!(capture.is_active());
        assert_eq!(capture.committed_output_chars, 2);

        capture.backspace();
        assert!(capture.is_active());
        capture.backspace();
        assert!(
            capture.is_active(),
            "deleting the first segment must not consume the hidden prefix budget"
        );
        assert_eq!(capture.committed_output_chars, 0);
        assert_eq!(capture.hidden_prefix_backspaces_remaining, 2);
        assert!(capture.should_consume_backspace());

        capture.backspace();
        assert!(capture.is_active());
        assert_eq!(capture.hidden_prefix_backspaces_remaining, 1);
        capture.backspace();
        assert!(!capture.is_active());
    }

    #[test]
    fn retyping_does_not_reset_partially_consumed_hidden_prefix_budget() {
        let mut capture = test_capture_state();
        capture.push_text(";;");
        capture.backspace();
        assert_eq!(capture.hidden_prefix_backspaces_remaining, 1);

        capture.push_text("ni");
        capture.backspace();
        capture.backspace();

        assert!(capture.is_active());
        assert_eq!(capture.hidden_prefix_backspaces_remaining, 1);
        capture.backspace();
        assert!(!capture.is_active());
    }

    #[test]
    fn failed_rewrite_restores_raw_buffer_and_visibility() {
        let mut capture = test_capture_state();
        capture.push_text(";;vke");
        let previous = capture.clone();

        assert!(matches!(
            capture.push_text_with_visibility(" ", false),
            Some(CaptureAction::Convert(ConversionAction { .. }))
        ));
        assert!(capture.buffer.is_empty());

        capture.restore_after_failed_rewrite(previous, " ", false);

        assert_eq!(capture.mode, CaptureMode::Active);
        assert_eq!(capture.buffer, "vke ");
        assert_eq!(capture.buffer_visible, vec![true, true, true, false]);
        assert_eq!(capture.hidden_prefix_backspaces_remaining, 2);
    }

    #[test]
    fn rewrite_transaction_guard_aborts_only_when_uncommitted() {
        REWRITE_ABORT_COUNT.store(0, Ordering::SeqCst);

        {
            let _transaction = RewriteTransactionGuard::new(count_rewrite_abort);
        }
        assert_eq!(REWRITE_ABORT_COUNT.load(Ordering::SeqCst), 1);

        {
            let mut transaction = RewriteTransactionGuard::new(count_rewrite_abort);
            transaction.mark_committed();
        }
        assert_eq!(REWRITE_ABORT_COUNT.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn backspace_to_empty_raw_buffer_keeps_hidden_marker_session() {
        let mut capture = test_capture_state();
        capture.push_text(";;");
        capture.buffer = "bug".to_owned();
        capture.buffer_visible = vec![true; 3];

        capture.backspace();
        capture.backspace();
        capture.backspace();

        assert_eq!(capture.mode, CaptureMode::Active);
        assert_eq!(capture.hidden_prefix_backspaces_remaining, 2);
        assert!(capture.buffer.is_empty());

        let action = capture.push_text("le ");

        assert_eq!(
            action,
            Some(CaptureAction::Convert(ConversionAction {
                typed_text: "le ".to_owned(),
                body: "le".to_owned(),
                restore_text: "le".to_owned(),
                delete_chars: "le ".chars().count(),
                stay_active: true,
            }))
        );
        assert_eq!(capture.mode, CaptureMode::Active);
    }

    #[test]
    fn backspace_with_empty_raw_buffer_consumes_trigger_length_before_exit() {
        let mut capture = test_capture_state();
        capture.mode = CaptureMode::Active;
        capture.hidden_prefix_backspaces_remaining = capture.active_exit_backspace_count();

        capture.backspace();

        assert_eq!(capture.mode, CaptureMode::Active);
        assert_eq!(capture.hidden_prefix_backspaces_remaining, 1);

        capture.backspace();

        assert_eq!(capture.mode, CaptureMode::Idle);
        assert_eq!(capture.hidden_prefix_backspaces_remaining, 0);
    }

    #[test]
    fn backspace_after_deleting_all_marker_chars_exits_active_state() {
        let mut capture = test_capture_state();
        capture.hidden_prefix_backspaces_remaining = capture.config.trigger_prefix.chars().count();
        capture.mode = CaptureMode::Active;

        capture.backspace();

        assert_eq!(capture.mode, CaptureMode::Active);
        assert_eq!(capture.hidden_prefix_backspaces_remaining, 1);

        capture.backspace();

        assert_eq!(capture.mode, CaptureMode::Idle);
        assert_eq!(capture.hidden_prefix_backspaces_remaining, 0);
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
    fn raw_fallback_segment_preserves_invalid_pinyin() {
        let segment = raw_fallback_segment("vke", "vke", "vke");

        assert_eq!(segment.raw, "vke");
        assert_eq!(segment.normalized, "vke");
        assert_eq!(segment.preedit, "vke");
        assert_eq!(segment.first, "vke");

        let mut capture = test_capture_state();
        capture.mode = CaptureMode::Active;
        capture.record_conversion("vke".to_owned(), "vke".to_owned(), QuoteState::default());
        assert_eq!(capture.committed_output_chars, 3);
        assert_eq!(capture.last_conversion, None);
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

    #[test]
    fn parses_candidate_layouts() {
        assert_eq!(
            CandidateLayout::parse("horizontal").unwrap(),
            CandidateLayout::Horizontal
        );
        assert_eq!(
            CandidateLayout::parse("vertical").unwrap(),
            CandidateLayout::Vertical
        );
        assert!(CandidateLayout::parse("diagonal").is_err());
    }

    #[test]
    fn validates_candidate_count_range() {
        assert!(validate_runtime_options(128, 0, DEFAULT_CANDIDATE_COUNT).is_ok());
        assert!(validate_runtime_options(128, 0, 0).is_err());
        assert!(validate_runtime_options(128, 0, MAX_CANDIDATE_COUNT + 1).is_err());
    }

    #[test]
    fn validates_candidate_interaction_keys() {
        assert!(validate_config(&AppConfig::default()).is_ok());

        let config = AppConfig {
            candidate_select_keys: "112".to_owned(),
            ..AppConfig::default()
        };
        assert!(validate_config(&config)
            .unwrap_err()
            .to_string()
            .contains("duplicate"));

        let config = AppConfig {
            candidate_select_keys: "12x".to_owned(),
            ..AppConfig::default()
        };
        assert!(validate_config(&config)
            .unwrap_err()
            .to_string()
            .contains("ASCII digits"));

        let config = AppConfig {
            english_commit_key: "1".to_owned(),
            ..AppConfig::default()
        };
        assert!(validate_config(&config)
            .unwrap_err()
            .to_string()
            .contains("candidate_select_keys"));

        let config = AppConfig {
            candidate_page_next_key: DEFAULT_CANDIDATE_PAGE_PREVIOUS_KEY.to_owned(),
            ..AppConfig::default()
        };
        assert!(validate_config(&config)
            .unwrap_err()
            .to_string()
            .contains("must be distinct"));

        let config = AppConfig {
            candidate_count: DEFAULT_CANDIDATE_SELECT_KEYS.chars().count() + 1,
            ..AppConfig::default()
        };
        assert!(validate_config(&config)
            .unwrap_err()
            .to_string()
            .contains("capacity"));

        let config = AppConfig {
            candidate_page_next_key: String::new(),
            ..AppConfig::default()
        };
        assert!(validate_config(&config)
            .unwrap_err()
            .to_string()
            .contains("exactly one character"));

        let config = AppConfig {
            english_commit_key: "ab".to_owned(),
            ..AppConfig::default()
        };
        assert!(validate_config(&config)
            .unwrap_err()
            .to_string()
            .contains("exactly one printable character"));

        let config = AppConfig {
            candidate_page_next_key: "a".to_owned(),
            ..AppConfig::default()
        };
        assert!(validate_config(&config)
            .unwrap_err()
            .to_string()
            .contains("pinyin"));

        let config = AppConfig {
            english_commit_key: ";".to_owned(),
            ..AppConfig::default()
        };
        assert!(validate_config(&config)
            .unwrap_err()
            .to_string()
            .contains("trigger character"));
    }

    #[test]
    fn loads_and_classifies_custom_candidate_interaction_keys() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = env::temp_dir().join(format!(
            "rime-poc-custom-interaction-{}-{unique}.toml",
            std::process::id()
        ));
        fs::write(
            &path,
            r#"
trigger_prefix = ";;"
trigger_suffix = ";;"
candidate_count = 5
candidate_select_keys = "0987654321"
candidate_page_next_key = "]"
candidate_page_previous_key = "["
english_commit_key = "\\"
"#,
        )
        .unwrap();

        let (config, loaded_path) = load_config(Some(path.clone())).unwrap();
        let _ = fs::remove_file(&path);

        assert_eq!(loaded_path.as_deref(), Some(path.as_path()));
        assert!(validate_config(&config).is_ok());
        assert_eq!(
            config.candidate_interaction_key('0'),
            Some(CandidateInteractionKey::Select(0))
        );
        assert_eq!(
            config.candidate_interaction_key('1'),
            Some(CandidateInteractionKey::Select(9))
        );
        assert_eq!(
            config.candidate_interaction_key(']'),
            Some(CandidateInteractionKey::NextPage)
        );
        assert_eq!(
            config.candidate_interaction_key('['),
            Some(CandidateInteractionKey::PreviousPage)
        );
        assert_eq!(
            config.candidate_interaction_key('\\'),
            Some(CandidateInteractionKey::EnglishCommit)
        );
    }

    #[test]
    #[ignore = "requires the bundled Rime data and native librime"]
    fn rime_candidate_selection_and_paging_integration() {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let shared_data_dir = manifest_dir.join("data/shared");
        let user_data_dir =
            env::temp_dir().join(format!("rime-poc-candidate-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&user_data_dir);
        fs::create_dir_all(&user_data_dir).unwrap();
        fs::copy(
            manifest_dir.join("data/user/default.custom.yaml"),
            user_data_dir.join("default.custom.yaml"),
        )
        .unwrap();

        let mut traits = Traits::new();
        traits
            .set_shared_data_dir(path_to_str(&shared_data_dir).unwrap())
            .set_user_data_dir(path_to_str(&user_data_dir).unwrap())
            .set_distribution_name("rime-poc-test")
            .set_distribution_code_name("rime-poc-test")
            .set_distribution_version(env!("CARGO_PKG_VERSION"))
            .set_app_name("rime-poc-test")
            .set_min_log_level(2);
        setup(&mut traits);
        initialize(&mut traits);

        let result = (|| -> Result<()> {
            if !matches!(full_deploy_and_wait(), DeployResult::Success) {
                bail!("Rime deployment failed")
            }
            let options = Options {
                shared_data_dir,
                user_data_dir: user_data_dir.clone(),
                schema: "luna_pinyin_simp".to_owned(),
                config_path: None,
                config: AppConfig::default(),
                body_mode: false,
                doctor: false,
                listen: false,
                max_buffer_chars: DEFAULT_MAX_BUFFER_CHARS,
                inject_delay_ms: DEFAULT_INJECT_DELAY_MS,
                log_events: false,
                input: String::new(),
            };

            let first_page = preview_candidates(&options, "shi", 5, 0)?;
            anyhow::ensure!(!first_page.candidates.is_empty(), "first page is empty");
            anyhow::ensure!(first_page.page_no == 0, "first page number is not zero");

            let second_page = preview_candidates(&options, "shi", 5, 1)?;
            anyhow::ensure!(second_page.page_no == 1, "failed to move to page 1");
            anyhow::ensure!(!second_page.candidates.is_empty(), "second page is empty");
            anyhow::ensure!(
                first_page.candidates != second_page.candidates,
                "page navigation did not change candidates"
            );
            let expected_space_selected = second_page
                .candidates
                .first()
                .context("second page has no first candidate")?
                .clone();
            let space_selected = select_candidate(&options, "shi", 1, 0)?;
            match space_selected {
                Some(CandidateSelection::Complete { text }) => anyhow::ensure!(
                    text == expected_space_selected,
                    "Space selection did not commit the displayed page candidate: expected={expected_space_selected:?} actual={text:?}"
                ),
                Some(CandidateSelection::Partial {
                    selected_text,
                    remaining_pinyin,
                }) => {
                    anyhow::ensure!(
                        selected_text == expected_space_selected,
                        "Space partial selection did not choose the displayed page candidate: expected={expected_space_selected:?} actual={selected_text:?}"
                    );
                    anyhow::ensure!(
                        !remaining_pinyin.is_empty(),
                        "Space partial selection lost the unconsumed pinyin"
                    );
                    let remaining_preview =
                        preview_candidates(&options, &remaining_pinyin, 5, 0)?;
                    anyhow::ensure!(
                        !remaining_preview.candidates.is_empty(),
                        "Space partial selection remainder produced no candidates"
                    );
                }
                None => bail!("Space selection returned no candidate"),
            }
            let expected_selected = second_page
                .candidates
                .get(1)
                .context("second page has fewer than two candidates")?
                .clone();

            let selected = select_candidate(&options, "shi", 1, 1)?;
            anyhow::ensure!(
                matches!(
                    selected,
                    Some(CandidateSelection::Complete { ref text })
                        if text == &expected_selected
                ),
                "candidate selection did not commit the displayed page candidate: expected={expected_selected:?} actual={selected:?}"
            );

            let multi_syllable = "woshizhongguoren";
            let multi_preview = preview_candidates(&options, multi_syllable, 10, 0)?;
            let partial_index = multi_preview
                .candidates
                .iter()
                .position(|candidate| candidate == "我是")
                .context("multi-syllable first page has no partial candidate 我是")?;
            let partial = select_candidate(&options, multi_syllable, 0, partial_index)?;
            let remaining = match partial {
                Some(CandidateSelection::Partial {
                    selected_text,
                    remaining_pinyin,
                }) => {
                    anyhow::ensure!(selected_text == "我是", "unexpected partial selection");
                    anyhow::ensure!(
                        remaining_pinyin == "zhongguoren",
                        "unexpected partial remainder: {remaining_pinyin:?}"
                    );
                    remaining_pinyin
                }
                other => bail!("expected partial candidate selection, got {other:?}"),
            };

            let remaining_preview = preview_candidates(&options, &remaining, 5, 0)?;
            anyhow::ensure!(
                !remaining_preview.candidates.is_empty(),
                "remaining pinyin produced no candidates"
            );
            let final_selection = select_candidate(&options, &remaining, 0, 0)?;
            anyhow::ensure!(
                matches!(
                    final_selection,
                    Some(CandidateSelection::Complete { ref text }) if !text.is_empty()
                ),
                "remaining pinyin did not finish with a complete selection: {final_selection:?}"
            );
            Ok(())
        })();

        finalize();
        let _ = fs::remove_dir_all(&user_data_dir);
        result.unwrap();
    }
}
