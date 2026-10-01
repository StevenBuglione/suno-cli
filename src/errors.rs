use thiserror::Error;

#[derive(Error, Debug)]
pub enum CliError {
    #[error("{source}")]
    Diagnostic {
        source: Box<CliError>,
        details: serde_json::Value,
    },

    #[error("API error: {message}")]
    Api { code: &'static str, message: String },

    #[error("Authentication required — run `suno auth` first")]
    AuthMissing,

    #[error("JWT expired or rejected by Suno")]
    AuthExpired,

    #[error("Rate limited by Suno — wait and retry")]
    RateLimited,

    #[error("Rate limited by Suno; retry after {seconds} seconds")]
    RateLimitedFor { seconds: u64, suggestion: String },

    #[error("Generation failed: {0}")]
    GenerationFailed(String),

    #[error("{source}")]
    Recovery {
        source: Box<CliError>,
        suggestion: String,
    },

    #[error("Generation is still pending: {0}")]
    GenerationPending(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Invalid input: {0}")]
    InvalidInput(String),

    #[error("Download failed: {0}")]
    Download(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Self-update failed: {0}")]
    Update(String),

    #[error(transparent)]
    Http(#[from] reqwest::Error),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl CliError {
    pub fn details(&self) -> Option<serde_json::Value> {
        match self {
            Self::Diagnostic { details, .. } => Some(details.clone()),
            Self::Recovery { source, .. } => source.details(),
            Self::RateLimitedFor { seconds, .. } => {
                Some(serde_json::json!({"retry_after_secs":seconds}))
            }
            _ => None,
        }
    }

    pub fn retry_delay(&self) -> Option<std::time::Duration> {
        match self {
            Self::RateLimitedFor { seconds, .. } => Some(std::time::Duration::from_secs(*seconds)),
            _ => None,
        }
    }

    pub fn retryable_read(&self) -> bool {
        match self {
            Self::RateLimited | Self::RateLimitedFor { .. } => true,
            Self::Http(e) => e.is_timeout() || e.is_connect() || e.is_body(),
            Self::Api { code, .. } => *code == "api_unavailable",
            _ => false,
        }
    }

    /// Framework exit-code contract: 0 success, 1 transient (retry),
    /// 2 config/auth (fix setup), 3 bad input (fix arguments, don't blind
    /// retry), 4 rate limited. Auth errors are 2 because the fix is a setup
    /// action (`suno auth --login`), not a retry; not-found is 3 because the
    /// fix is a different ID. Code 5 no longer exists (breaking vs 0.5.x).
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Recovery { source, .. } | Self::Diagnostic { source, .. } => source.exit_code(),
            Self::GenerationPending(_) => 1,
            Self::Config(_) | Self::AuthMissing | Self::AuthExpired => 2,
            Self::Api {
                code: "captcha_required",
                ..
            } => 2,
            Self::InvalidInput(_) | Self::NotFound(_) => 3,
            Self::RateLimited | Self::RateLimitedFor { .. } => 4,
            // Moderation / content-policy rejections are bad input (3) —
            // retrying the same prompt fails identically. Everything else
            // that failed server-side is worth a retry (1).
            Self::GenerationFailed(msg) => {
                if msg.to_ascii_lowercase().contains("moderat") {
                    3
                } else {
                    1
                }
            }
            Self::Api { .. }
            | Self::Http(_)
            | Self::Download(_)
            | Self::Update(_)
            | Self::Io(_)
            | Self::Json(_) => 1,
        }
    }

    pub fn error_code(&self) -> &'static str {
        match self {
            Self::Recovery { source, .. } | Self::Diagnostic { source, .. } => source.error_code(),
            Self::GenerationPending(_) => "generation_pending",
            Self::Api { code, .. } => code,
            Self::AuthMissing => "auth_missing",
            Self::AuthExpired => "auth_expired",
            Self::RateLimited | Self::RateLimitedFor { .. } => "rate_limited",
            Self::Config(_) => "config_error",
            Self::InvalidInput(_) => "invalid_input",
            Self::GenerationFailed(_) => "generation_failed",
            Self::Download(_) => "download_error",
            Self::NotFound(_) => "not_found",
            Self::Http(_) => "http_error",
            Self::Io(_) => "io_error",
            Self::Json(_) => "json_error",
            Self::Update(_) => "update_error",
        }
    }

    pub fn suggestion(&self) -> &str {
        match self {
            Self::Recovery { suggestion, .. } => suggestion,
            Self::Diagnostic { source, .. } => source.suggestion(),
            Self::GenerationPending(_) => {
                "Resume with `suno status <ids> --wait`; do not submit another generation"
            }
            Self::AuthMissing => "Run `suno auth --login` to authenticate",
            Self::AuthExpired => {
                "Run `suno auth --refresh`; if that fails, run `suno auth --login`"
            }
            Self::RateLimited => "Wait 30-60 seconds and retry",
            Self::RateLimitedFor { suggestion, .. } => suggestion,
            Self::Config(_) => "Check `suno doctor` for configuration issues",
            Self::InvalidInput(_) => "Check arguments with `suno <command> --help`",
            Self::NotFound(_) => "Verify the ID exists with `suno list` or `suno search`",
            Self::Download(_) => {
                "Check that the clip has finished generating with `suno status <id>`"
            }
            Self::GenerationFailed(_) => "Check `suno credits` for remaining balance",
            Self::Api {
                code: "captcha_required",
                ..
            } => {
                "Supply --token, or omit --no-browser to allow the captcha solver (use --headless for invisible Chrome)"
            }
            Self::Api { code, .. } if *code == "schema_drift" => {
                "Suno changed their API or rolled out hCaptcha enforcement. Try (1) `suno auth --refresh` to mint a fresh JWT, (2) `suno update` to pull the latest fix, (3) supply an hCaptcha solution via `--token <solved>`, or (4) see https://github.com/paperfoot/suno-cli/issues for the current status"
            }
            Self::Api { .. } | Self::Http(_) => "Check your network connection and retry",
            Self::Io(_) => "Check file permissions and disk space",
            Self::Json(_) => {
                "This may indicate an API change — run `suno update` for the latest fix"
            }
            Self::Update(_) => {
                "Check your network connection or download the binary directly from GitHub Releases"
            }
        }
    }
}
