mod api;
mod auth;
mod browser_login;
mod captcha;
mod cli;
mod commands;
mod config;
mod download;
mod errors;
mod guard;
mod output;

use clap::Parser;

use api::SunoClient;
use api::types::{ControlSliders, GenerateRequest, SetMetadataRequest};
use auth::AuthState;
use cli::*;
use commands::generation::{generation_recovery, handle_generation, preview_request};
use errors::CliError;
use output::OutputFormat;

async fn client() -> Result<SunoClient, CliError> {
    let auth = AuthState::load()?;
    SunoClient::new_with_refresh(auth).await
}

/// Flag > config (`default_model`, itself env-overridable) > compiled default.
fn resolve_model(
    flag: Option<ModelVersion>,
    cfg: &config::AppConfig,
) -> Result<ModelVersion, CliError> {
    match flag {
        Some(m) => Ok(m),
        None => {
            <ModelVersion as clap::ValueEnum>::from_str(&cfg.default_model, true).map_err(|_| {
                CliError::Config(format!(
                    "config default_model '{}' is not a valid --model value — \
                     fix it with `suno config set default_model v6`",
                    cfg.default_model
                ))
            })
        }
    }
}

/// Read a caller-supplied input file (e.g. --lyrics-file). A missing or
/// unreadable path is bad input (exit 3), not a retryable io_error (exit 1):
/// retrying the same wrong path fails identically.
fn read_input_file(path: &str) -> Result<String, CliError> {
    std::fs::read_to_string(path)
        .map_err(|e| CliError::InvalidInput(format!("cannot read input file '{path}': {e}")))
}

/// Credit protection shared by every command that sends lyrics into the
/// v2-web `prompt` field (generate, extend): an unfilled `suno write`
/// scaffold would be sung verbatim at real credit cost. Refuse it unless
/// --allow-placeholders is supplied.
fn reject_unfilled_scaffold(
    lyrics: Option<&str>,
    allow_placeholders: bool,
) -> Result<(), CliError> {
    let Some(text) = lyrics else { return Ok(()) };
    let lines = commands::write::placeholder_lines(text);
    if lines.is_empty() || allow_placeholders {
        return Ok(());
    }
    let numbers: Vec<String> = lines.iter().map(|n| n.to_string()).collect();
    Err(CliError::InvalidInput(format!(
        "lyrics contain {} unresolved scaffold placeholder(s) at line(s) {} — fill the <...> spans before generating (or pass --allow-placeholders to send them as written)",
        lines.len(),
        numbers.join(", ")
    )))
}

fn build_tags(tags: Option<&str>, vocal: Option<&VocalGender>) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    if let Some(t) = tags {
        parts.push(t);
    }
    match vocal {
        Some(VocalGender::Male) => parts.push("male vocals"),
        Some(VocalGender::Female) => parts.push("female vocals"),
        None => {}
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(", "))
    }
}

/// Build a control_sliders block when any slider flag is set.
/// Returns None when none is provided so we don't pollute the request.
fn build_control_sliders(
    weirdness: Option<f64>,
    style_influence: Option<f64>,
    audio_influence: Option<f64>,
) -> Option<ControlSliders> {
    if weirdness.is_none() && style_influence.is_none() && audio_influence.is_none() {
        return None;
    }
    Some(ControlSliders {
        // Normalize 0-100 → 0.0-1.0
        weirdness_constraint: weirdness.map(|w| (w / 100.0).clamp(0.0, 1.0)),
        style_weight: style_influence.map(|s| (s / 100.0).clamp(0.0, 1.0)),
        audio_weight: audio_influence.map(|a| (a / 100.0).clamp(0.0, 1.0)),
        aug_creativity: None,
    })
}

/// Resolve the captcha token for v2-web creation and reference tasks.
/// Returns the token to attach
/// to the request body, or `None` when no captcha is needed.
///
/// An explicit --token wins (assumed hCaptcha-solved); --no-captcha means
/// never solve. Otherwise preflight `/api/c/check` — most accounts are above
/// Suno's trust threshold and need no captcha, so we skip the Chrome solver
/// entirely. A preflight failure falls back to solving (never a hard gate).
async fn resolve_captcha(
    c: &SunoClient,
    token: Option<String>,
    provider: Option<u8>,
    no_captcha: bool,
    headless: bool,
    no_browser: bool,
    quiet: bool,
) -> Result<(Option<String>, Option<u8>), CliError> {
    if token.is_some() && provider.is_some() {
        return Ok((token, provider));
    }
    if no_captcha && token.is_none() {
        return Ok((None, None));
    }
    let check = c.captcha_check("generation").await?;
    let provider = match provider {
        Some(p) => p,
        None => u8::try_from(check.captcha_version.unwrap_or(1)).map_err(|_| {
            CliError::Config("unknown Suno captcha provider; update the CLI".into())
        })?,
    };
    if !matches!(provider, 1 | 2) {
        return Err(CliError::Config(format!(
            "unknown Suno captcha provider {provider}; update the CLI"
        )));
    }
    if token.is_some() {
        return Ok((token, Some(provider)));
    }
    let forced = std::env::var("SUNO_FORCE_CAPTCHA").is_ok_and(|v| v == "1");
    if !check.required && !forced {
        return Ok((None, None));
    }
    if no_browser {
        return Err(CliError::Api {
            code: "captcha_required",
            message: format!(
                "Suno requires captcha provider {provider}; supply --token and --token-provider {provider}, or omit --no-browser"
            ),
        });
    }
    if !quiet {
        eprintln!("Solving Suno captcha (provider {provider})...");
    }
    let (solved, actual_provider) =
        captcha::solve(&AuthState::load()?, headless, provider, quiet).await?;
    Ok((Some(solved), Some(actual_provider)))
}

#[derive(Clone, Copy)]
struct DispatchContext {
    fmt: OutputFormat,
    quiet: bool,
    headless: bool,
    no_browser: bool,
}

async fn run_auth(mut args: AuthArgs, ctx: DispatchContext) -> Result<(), CliError> {
    let fmt = ctx.fmt;
    if args.cookie_stdin {
        args.cookie = Some(commands::auth_input::read_secret()?);
    }
    if args.jwt_stdin {
        args.jwt = Some(commands::auth_input::read_secret()?);
    }
    if ctx.no_browser && args.login {
        return Err(CliError::Config(
            "headless authentication uses --cookie-stdin, --jwt-stdin, or a stored session".into(),
        ));
    }
    if args.logout {
        AuthState::delete()?;
        match fmt {
            OutputFormat::Json => {
                output::json::success(serde_json::json!({ "authenticated": false }))?
            }
            OutputFormat::Table => {
                eprintln!("Logged out; removed stored Suno authentication")
            }
        }
        return Ok(());
    }

    let mut state = match AuthState::load() {
        Ok(s) => s,
        Err(CliError::AuthMissing) => AuthState::default(),
        Err(e) => return Err(e),
    };

    let has_explicit_auth_input = args.login
        || args.browser_login
        || args.refresh
        || args.jwt.is_some()
        || args.cookie.is_some();
    let should_login = args.login
        || args.browser_login
        || (!has_explicit_auth_input && state.jwt.is_none() && state.clerk_client_cookie.is_none());

    if ctx.no_browser && should_login {
        return Err(CliError::AuthMissing);
    }
    if ctx.headless && args.browser_login {
        return Err(CliError::InvalidInput(
            "--browser-login is interactive; use --cookie-stdin or --jwt-stdin with --headless"
                .into(),
        ));
    }
    if args.refresh {
        // Force-refresh the JWT via the stored Clerk session cookie.
        // Useful when the API rejects the current JWT mid-session
        // before the CLI's own staleness check fires.
        let cookie = state.clerk_client_cookie.clone().ok_or_else(|| {
            CliError::Config(
                "no Clerk session cookie stored — run `suno auth --login` first".into(),
            )
        })?;
        let http = auth::http_client()?;
        if !ctx.quiet && !matches!(fmt, OutputFormat::Json) {
            eprintln!("Refreshing JWT via Clerk session cookie...");
        }
        let (session_id, jwt) = if let Some(session_id) = state.session_id.clone() {
            (
                session_id.clone(),
                auth::clerk_refresh_jwt(&http, &cookie, &session_id).await?,
            )
        } else {
            auth::clerk_token_exchange(&http, &cookie).await?
        };
        state.session_id = Some(session_id);
        state.jwt = Some(jwt);
        state.save()?;
        if !ctx.quiet && !matches!(fmt, OutputFormat::Json) {
            eprintln!("JWT refreshed successfully");
        }
    } else if should_login {
        // Automatic: extract cookies from browser
        if !ctx.quiet && !matches!(fmt, OutputFormat::Json) {
            eprintln!("Extracting Suno session from your browser...");
        }
        let browser_auth = if args.browser_login {
            Box::pin(browser_login::login(args.login_timeout)).await?
        } else {
            auth::extract_browser_auth()?
        };

        let http = auth::http_client()?;
        if !ctx.quiet && !matches!(fmt, OutputFormat::Json) {
            eprintln!("Exchanging for access token via Clerk...");
        }
        let (session_id, jwt) =
            auth::clerk_token_exchange(&http, &browser_auth.clerk_client_cookie).await?;

        state.cookie = Some(browser_auth.cookie_header);
        state.clerk_client_cookie = Some(browser_auth.clerk_client_cookie);
        state.session_id = Some(session_id);
        state.jwt = Some(jwt);
        state.device_id = browser_auth
            .device_id
            .or(state.device_id)
            .or_else(|| Some(uuid::Uuid::new_v4().to_string()));
    } else if let Some(cookie) = args.cookie.as_deref() {
        // Manual: user provides a full Cookie header or raw Clerk __client value.
        let browser_auth = auth::normalize_cookie_input(cookie)?;
        let http = auth::http_client()?;
        if !ctx.quiet && !matches!(fmt, OutputFormat::Json) {
            eprintln!("Exchanging cookie for access token...");
        }
        let (session_id, jwt) =
            auth::clerk_token_exchange(&http, &browser_auth.clerk_client_cookie).await?;

        state.cookie = Some(browser_auth.cookie_header);
        state.clerk_client_cookie = Some(browser_auth.clerk_client_cookie);
        state.session_id = Some(session_id);
        state.jwt = Some(jwt);
        state.device_id = browser_auth
            .device_id
            .or(state.device_id)
            .or_else(|| Some(uuid::Uuid::new_v4().to_string()));
    } else if let Some(jwt) = args.jwt.clone() {
        // Legacy: direct JWT paste (expires in ~1 hour)
        state.jwt = Some(jwt);
        if state.device_id.is_none() {
            state.device_id = Some(uuid::Uuid::new_v4().to_string());
        }
    } else if !ctx.quiet && !matches!(fmt, OutputFormat::Json) {
        eprintln!("Checking existing authentication...");
    }

    if let Some(device) = args.device.as_ref() {
        state.device_id = Some(device.clone());
    }

    // Verify
    let should_save_after_verify = args.refresh
        || should_login
        || args.cookie.is_some()
        || args.jwt.is_some()
        || args.device.is_some();
    let client = SunoClient::new_with_refresh(state.clone()).await?;
    let info = client.billing_info().await?;
    if should_save_after_verify {
        state.save()?;
    }
    match fmt {
        OutputFormat::Json => output::json::success(serde_json::json!({
            "authenticated": true,
            "plan": info.plan.name,
            "credits": info.total_credits_left,
        }))?,
        OutputFormat::Table => eprintln!(
            "Authenticated! Plan: {}, Credits: {}",
            info.plan.name, info.total_credits_left
        ),
    }

    Ok(())
}

async fn run_cover(args: CoverArgs, ctx: DispatchContext) -> Result<(), CliError> {
    let fmt = ctx.fmt;
    let cfg = config::AppConfig::load()?;
    let model = resolve_model(args.model, &cfg)?;
    api::cover::validate_range(args.start, args.end, None)?;
    let lyrics = match (&args.lyrics, &args.lyrics_file) {
        (Some(text), _) => Some(text.clone()),
        (_, Some(path)) => Some(read_input_file(path)?),
        _ => None,
    };
    reject_unfilled_scaffold(lyrics.as_deref(), args.allow_placeholders)?;
    let (source, connection) = if let Some(path) = args.source_file.as_deref() {
        let value: serde_json::Value =
            serde_json::from_str(&read_input_file(path)?).map_err(|_| {
                CliError::InvalidInput("source file must contain valid clip JSON".into())
            })?;
        let clip: api::types::Clip =
            serde_json::from_value(value.get("data").cloned().unwrap_or(value)).map_err(|_| {
                CliError::InvalidInput(
                    "source file must contain a clip or `suno info` envelope".into(),
                )
            })?;
        if clip.id != args.clip_id {
            return Err(CliError::InvalidInput(
                "source-file clip ID does not match the requested song".into(),
            ));
        }
        (clip, None)
    } else {
        let c = client().await?;
        let source = c
            .get_existing_clips(std::slice::from_ref(&args.clip_id))
            .await?
            .remove(0);
        (source, Some(c))
    };
    let mut req = api::cover::request(
        &source,
        model.to_api_key(),
        api::cover::CoverOptions {
            title: args.title,
            tags: args.tags,
            lyrics,
            exclude: args.exclude,
            instrumental: args.instrumental,
            persona: args.persona,
            vocal: args.vocal.map(|v| match v {
                VocalGender::Male => "male".into(),
                VocalGender::Female => "female".into(),
            }),
            start: args.start,
            end: args.end,
            sliders: build_control_sliders(
                args.weirdness,
                args.style_influence,
                args.audio_influence,
            ),
            request_id: args.request_id,
        },
    )?;
    reject_unfilled_scaffold(Some(&req.prompt), args.allow_placeholders)?;
    if args.dry_run {
        preview_request(&req, fmt)?;
        return Ok(());
    }
    cfg.validate()?;
    let mut guard = guard::DuplicateGuard::new(&config::data_dir(), "cover");
    guard.acquire(args.force)?;
    let c = connection.expect("live cover has a client");
    if let Some(ids) = commands::jobs::existing(&req)? {
        let clips = c
            .get_existing_clips(&ids)
            .await
            .map_err(|e| generation_recovery(e, &ids))?;
        handle_generation(
            &c,
            clips,
            args.wait,
            args.download.as_deref(),
            fmt,
            ctx.quiet,
            &cfg,
        )
        .await?;
        return Ok(());
    }
    c.validate_generation(&req).await?;
    (req.token, req.token_provider) = resolve_captcha(
        &c,
        args.token,
        args.token_provider,
        args.no_captcha,
        ctx.headless,
        ctx.no_browser,
        ctx.quiet,
    )
    .await?;
    let clips = c.generate(&req).await?;
    handle_generation(
        &c,
        clips,
        args.wait,
        args.download.as_deref(),
        fmt,
        ctx.quiet,
        &cfg,
    )
    .await?;

    Ok(())
}

async fn run_speed(args: SpeedArgs, ctx: DispatchContext) -> Result<(), CliError> {
    let fmt = ctx.fmt;
    let cfg = config::AppConfig::load()?;
    let mut body = api::edit::speed_request(
        &args.edit.clip_id,
        args.multiplier,
        args.keep_pitch,
        args.edit.title.as_deref().unwrap_or("Speed edit"),
    )?;
    if args.edit.dry_run {
        output::json::success(
            serde_json::json!({"dry_run":true,"submitted":false,"endpoint":"/api/clips/adjust-speed/","request":body}),
        )?;
        return Ok(());
    }
    cfg.validate()?;
    let c = client().await?;
    if args.edit.title.is_none() {
        let source = c
            .get_existing_clips(std::slice::from_ref(&args.edit.clip_id))
            .await?
            .remove(0);
        body["title"] = format!("{} ({}x)", source.title, args.multiplier).into();
    }
    let clip = c.transform("speed", &body).await?;
    handle_generation(
        &c,
        vec![clip],
        args.edit.wait,
        args.edit.download.as_deref(),
        fmt,
        ctx.quiet,
        &cfg,
    )
    .await?;

    Ok(())
}

async fn run_reverse(args: EditArgs, ctx: DispatchContext) -> Result<(), CliError> {
    let fmt = ctx.fmt;
    let cfg = config::AppConfig::load()?;
    let mut body = serde_json::json!({"clip_id":args.clip_id,"title":args.title.as_deref().unwrap_or("Reversed")});
    if args.dry_run {
        output::json::success(
            serde_json::json!({"dry_run":true,"submitted":false,"endpoint":"/api/clips/reverse-clip/","request":body}),
        )?;
        return Ok(());
    }
    cfg.validate()?;
    let c = client().await?;
    if args.title.is_none() {
        let source = c
            .get_existing_clips(std::slice::from_ref(&args.clip_id))
            .await?
            .remove(0);
        body["title"] = format!("{} (Reversed)", source.title).into();
    }
    let clip = c.transform("reverse", &body).await?;
    handle_generation(
        &c,
        vec![clip],
        args.wait,
        args.download.as_deref(),
        fmt,
        ctx.quiet,
        &cfg,
    )
    .await?;

    Ok(())
}

async fn run_editstatus(args: EditStatusArgs, ctx: DispatchContext) -> Result<(), CliError> {
    let fmt = ctx.fmt;
    let cfg = config::AppConfig::load()?;
    cfg.validate()?;
    let c = client().await?;
    if args.wait || args.download.is_some() {
        let clips = c
            .poll_edit_action(&args.id, cfg.poll_timeout_secs, cfg.poll_interval_secs)
            .await?;
        handle_generation(
            &c,
            clips,
            false,
            args.download.as_deref(),
            fmt,
            ctx.quiet,
            &cfg,
        )
        .await?;
    } else {
        output::json::success(c.edit_action(&args.id).await?)?;
    }

    Ok(())
}

async fn run_remix(args: RemixArgs, operation: &str, ctx: DispatchContext) -> Result<(), CliError> {
    let cfg = config::AppConfig::load()?;
    let model = resolve_model(args.generate.model.clone(), &cfg)?;
    if (args.start.is_some() || args.end.is_some()) && operation != "replace" {
        return Err(CliError::InvalidInput(
            "--start and --end are supported by replace; use cover for reference ranges".into(),
        ));
    }
    api::cover::validate_range(args.start, args.end, None)?;
    let explicit_lyrics = match (&args.generate.lyrics, &args.generate.lyrics_file) {
        (Some(text), _) => Some(text.clone()),
        (_, Some(path)) => Some(read_input_file(path)?),
        _ => None,
    };
    reject_unfilled_scaffold(explicit_lyrics.as_deref(), args.generate.allow_placeholders)?;
    let (source, connection) = if let Some(path) = args.source_file.as_deref() {
        let value: serde_json::Value = serde_json::from_str(&read_input_file(path)?)
            .map_err(|_| CliError::InvalidInput("invalid source clip JSON".into()))?;
        let clip: api::types::Clip =
            serde_json::from_value(value.get("data").cloned().unwrap_or(value)).map_err(|_| {
                CliError::InvalidInput(
                    "source file must contain a clip or `suno info` envelope".into(),
                )
            })?;
        if clip.id != args.clip_id {
            return Err(CliError::InvalidInput(
                "source-file clip ID does not match the song".into(),
            ));
        }
        (clip, None)
    } else {
        let c = client().await?;
        let source = c
            .get_existing_clips(std::slice::from_ref(&args.clip_id))
            .await?
            .remove(0);
        (source, Some(c))
    };
    let a = args.generate;
    if (a.variety.is_some() || a.mumble || a.duration.is_some()) && !model.is_v6_family() {
        return Err(CliError::InvalidInput(
            "duration, variety and mumble require v6".into(),
        ));
    }
    let mut req = GenerateRequest::new(model.to_api_key(), "custom");
    req.title = a.title.unwrap_or_else(|| source.title.clone());
    req.tags = build_tags(
        a.tags.as_deref().or(source.metadata.tags.as_deref()),
        a.vocal.as_ref(),
    )
    .unwrap_or_default();
    req.prompt =
        explicit_lyrics.unwrap_or_else(|| source.metadata.prompt.clone().unwrap_or_default());
    req.negative_tags = a.exclude.unwrap_or_default();
    req.make_instrumental = a.instrumental || req.prompt.trim().is_empty();
    if a.instrumental {
        req.prompt.clear();
    }
    req.persona_id = a.persona;
    req.duration = a.duration;
    req.metadata.is_mumble = a.mumble;
    req.metadata.is_max_mode = a.max_mode;
    req.metadata.vocal_gender = a.vocal.map(|v| match v {
        VocalGender::Male => "male".into(),
        VocalGender::Female => "female".into(),
    });
    req.metadata.control_sliders =
        build_control_sliders(a.weirdness, a.style_influence, a.audio_influence);
    if let Some(variety) = a.variety {
        req.metadata
            .control_sliders
            .get_or_insert(ControlSliders {
                weirdness_constraint: None,
                style_weight: None,
                audio_weight: None,
                aug_creativity: None,
            })
            .aug_creativity = Some(variety);
    }
    if let Some(id) = a.request_id {
        req.transaction_uuid = id.to_string();
    }
    let mut req = api::remix::request(&source, req, operation, args.start, args.end)?;
    reject_unfilled_scaffold(Some(&req.prompt), a.allow_placeholders)?;
    if a.dry_run {
        preview_request(&req, ctx.fmt)?;
        return Ok(());
    }
    cfg.validate()?;
    let mut guard = guard::DuplicateGuard::new(&config::data_dir(), operation);
    guard.acquire(a.force)?;
    let c = connection.expect("live remix has a client");
    if let Some(ids) = commands::jobs::existing(&req)? {
        let clips = c
            .get_existing_clips(&ids)
            .await
            .map_err(|e| generation_recovery(e, &ids))?;
        handle_generation(
            &c,
            clips,
            a.wait,
            a.download.as_deref(),
            ctx.fmt,
            ctx.quiet,
            &cfg,
        )
        .await?;
        return Ok(());
    }
    c.validate_generation(&req).await?;
    (req.token, req.token_provider) = resolve_captcha(
        &c,
        a.token,
        a.token_provider,
        a.no_captcha,
        ctx.headless,
        ctx.no_browser,
        ctx.quiet,
    )
    .await?;
    let clips = c.generate(&req).await?;
    handle_generation(
        &c,
        clips,
        a.wait,
        a.download.as_deref(),
        ctx.fmt,
        ctx.quiet,
        &cfg,
    )
    .await?;
    Ok(())
}

async fn run_generate(args: GenerateArgs, ctx: DispatchContext) -> Result<(), CliError> {
    let fmt = ctx.fmt;
    let cfg = config::AppConfig::load()?;
    let model = resolve_model(args.model, &cfg)?;
    let lyrics = match (&args.lyrics, &args.lyrics_file) {
        (Some(l), _) => Some(l.clone()),
        (_, Some(path)) => Some(read_input_file(path)?),
        _ => None,
    };
    reject_unfilled_scaffold(lyrics.as_deref(), args.allow_placeholders)?;
    let tags = build_tags(args.tags.as_deref(), args.vocal.as_ref());
    let mut control_sliders =
        build_control_sliders(args.weirdness, args.style_influence, args.audio_influence);

    if (args.variety.is_some() || args.mumble) && !model.is_v6_family() {
        return Err(CliError::InvalidInput(
            "--variety and --mumble require a v6 model".into(),
        ));
    }
    if let Some(variety) = args.variety {
        control_sliders
            .get_or_insert(ControlSliders {
                weirdness_constraint: None,
                style_weight: None,
                audio_weight: None,
                aug_creativity: None,
            })
            .aug_creativity = Some(variety);
    }
    if args.duration.is_some() && !model.is_v6_family() {
        return Err(CliError::InvalidInput(
            "--duration requires v6 custom generation".into(),
        ));
    }

    // Build the new v2-web request shape. Persona generation routes
    // through the same endpoint with persona_id set; the legacy
    // task="vox" field no longer exists in the v2-web schema.
    let mut req = GenerateRequest::new(model.to_api_key(), "custom");
    req.prompt = lyrics.unwrap_or_default();
    req.title = args.title.unwrap_or_default();
    req.tags = tags.unwrap_or_default();
    req.negative_tags = args.exclude.unwrap_or_default();
    req.make_instrumental = args.instrumental;
    req.persona_id = args.persona.clone();
    req.metadata.control_sliders = control_sliders;
    req.metadata.is_max_mode = args.max_mode;
    req.metadata.is_mumble = args.mumble;
    req.metadata.vocal_gender = args.vocal.map(|v| match v {
        VocalGender::Male => "male".into(),
        VocalGender::Female => "female".into(),
    });
    req.duration = args.duration;
    if let Some(id) = args.request_id {
        req.transaction_uuid = id.to_string();
    }
    api::models::validate_offline(&req)?;
    if args.dry_run {
        preview_request(&req, fmt)?;
        return Ok(());
    }
    cfg.validate()?;
    let mut guard = guard::DuplicateGuard::new(&config::data_dir(), "generate");
    guard.acquire(args.force)?;
    let c = client().await?;
    if let Some(ids) = commands::jobs::existing(&req)? {
        let clips = if args.wait || args.download.is_some() {
            c.poll_clips(&ids, cfg.poll_timeout_secs, cfg.poll_interval_secs)
                .await
                .map_err(|e| generation_recovery(e, &ids))?
        } else {
            c.get_existing_clips(&ids)
                .await
                .map_err(|e| generation_recovery(e, &ids))?
        };
        handle_generation(
            &c,
            clips,
            args.wait,
            args.download.as_deref(),
            fmt,
            ctx.quiet,
            &cfg,
        )
        .await?;
        return Ok(());
    }
    c.validate_generation(&req).await?;
    (req.token, req.token_provider) = resolve_captcha(
        &c,
        args.token,
        args.token_provider,
        args.no_captcha,
        ctx.headless,
        ctx.no_browser,
        ctx.quiet,
    )
    .await?;

    if !ctx.quiet {
        let persona_note = if args.persona.is_some() {
            " with voice persona"
        } else {
            ""
        };
        eprintln!(
            "Submitting generation ({}{persona_note})...",
            model.display_name()
        );
    }
    let clips = c.generate(&req).await?;
    handle_generation(
        &c,
        clips,
        args.wait,
        args.download.as_deref(),
        fmt,
        ctx.quiet,
        &cfg,
    )
    .await?;
    Ok(())
}

async fn run_describe(args: DescribeArgs, ctx: DispatchContext) -> Result<(), CliError> {
    let fmt = ctx.fmt;
    let cfg = config::AppConfig::load()?;
    let model = resolve_model(args.model, &cfg)?;
    let tags = build_tags(args.tags.as_deref(), args.vocal.as_ref());
    let mut control_sliders = build_control_sliders(args.weirdness, args.style_influence, None);

    if (args.variety.is_some() || args.mumble) && !model.is_v6_family() {
        return Err(CliError::InvalidInput(
            "--variety and --mumble require a v6 model".into(),
        ));
    }
    if let Some(variety) = args.variety {
        control_sliders
            .get_or_insert(ControlSliders {
                weirdness_constraint: None,
                style_weight: None,
                audio_weight: None,
                aug_creativity: None,
            })
            .aug_creativity = Some(variety);
    }

    // Simple mode sends the description in gpt_description_prompt;
    // prompt remains empty for server-generated lyrics.
    let mut req = GenerateRequest::new(model.to_api_key(), "simple");
    req.gpt_description_prompt = Some(args.prompt);
    req.tags = tags.unwrap_or_default();
    req.make_instrumental = args.instrumental;
    req.persona_id = args.persona.clone();
    req.metadata.control_sliders = control_sliders;
    req.metadata.is_max_mode = args.max_mode;
    req.metadata.is_mumble = args.mumble;
    req.metadata.vocal_gender = args.vocal.map(|v| match v {
        VocalGender::Male => "male".into(),
        VocalGender::Female => "female".into(),
    });
    if let Some(id) = args.request_id {
        req.transaction_uuid = id.to_string();
    }
    api::models::validate_offline(&req)?;
    if args.dry_run {
        preview_request(&req, fmt)?;
        return Ok(());
    }
    cfg.validate()?;
    let mut guard = guard::DuplicateGuard::new(&config::data_dir(), "describe");
    guard.acquire(args.force)?;
    let c = client().await?;
    if let Some(ids) = commands::jobs::existing(&req)? {
        let clips = if args.wait || args.download.is_some() {
            c.poll_clips(&ids, cfg.poll_timeout_secs, cfg.poll_interval_secs)
                .await
                .map_err(|e| generation_recovery(e, &ids))?
        } else {
            c.get_existing_clips(&ids)
                .await
                .map_err(|e| generation_recovery(e, &ids))?
        };
        handle_generation(
            &c,
            clips,
            args.wait,
            args.download.as_deref(),
            fmt,
            ctx.quiet,
            &cfg,
        )
        .await?;
        return Ok(());
    }
    c.validate_generation(&req).await?;
    (req.token, req.token_provider) = resolve_captcha(
        &c,
        args.token,
        args.token_provider,
        args.no_captcha,
        ctx.headless,
        ctx.no_browser,
        ctx.quiet,
    )
    .await?;

    if !ctx.quiet {
        eprintln!("Submitting description ({})...", model.display_name());
    }
    let clips = c.generate(&req).await?;
    handle_generation(
        &c,
        clips,
        args.wait,
        args.download.as_deref(),
        fmt,
        ctx.quiet,
        &cfg,
    )
    .await?;
    Ok(())
}

async fn run_extend(args: ExtendArgs, ctx: DispatchContext) -> Result<(), CliError> {
    let fmt = ctx.fmt;
    let cfg = config::AppConfig::load()?;
    let model = resolve_model(args.model, &cfg)?;
    api::cover::validate_range(Some(args.at), None, None)?;
    let lyrics = match (&args.lyrics, &args.lyrics_file) {
        (Some(text), _) => Some(text.clone()),
        (_, Some(path)) => Some(read_input_file(path)?),
        _ => None,
    };
    reject_unfilled_scaffold(lyrics.as_deref(), args.allow_placeholders)?;
    let mut req = GenerateRequest::new(model.to_api_key(), "custom");
    req.task = Some("extend".into());
    req.metadata.is_remix = true;
    req.prompt = lyrics.unwrap_or_default();
    req.tags = args.tags.unwrap_or_default();
    req.continue_clip_id = Some(args.clip_id.clone());
    req.continue_at = Some(args.at);
    if let Some(id) = args.request_id {
        req.transaction_uuid = id.to_string();
    }
    api::models::validate_offline(&req)?;
    if args.dry_run {
        preview_request(&req, fmt)?;
        return Ok(());
    }
    cfg.validate()?;
    let mut guard = guard::DuplicateGuard::new(&config::data_dir(), "extend");
    guard.acquire(args.force)?;
    let c = client().await?;
    if let Some(ids) = commands::jobs::existing(&req)? {
        let clips = c
            .get_existing_clips(&ids)
            .await
            .map_err(|e| generation_recovery(e, &ids))?;
        handle_generation(
            &c,
            clips,
            args.wait,
            args.download.as_deref(),
            fmt,
            ctx.quiet,
            &cfg,
        )
        .await?;
        return Ok(());
    }
    let source = c
        .get_existing_clips(std::slice::from_ref(&args.clip_id))
        .await?
        .remove(0);
    api::cover::validate_range(Some(args.at), None, source.metadata.duration)?;
    c.validate_generation(&req).await?;
    (req.token, req.token_provider) = resolve_captcha(
        &c,
        args.token,
        args.token_provider,
        args.no_captcha,
        ctx.headless,
        ctx.no_browser,
        ctx.quiet,
    )
    .await?;
    let clips = c.generate(&req).await?;
    handle_generation(
        &c,
        clips,
        args.wait,
        args.download.as_deref(),
        fmt,
        ctx.quiet,
        &cfg,
    )
    .await?;
    Ok(())
}

async fn run_remaster(args: RemasterArgs, ctx: DispatchContext) -> Result<(), CliError> {
    let fmt = ctx.fmt;
    let cfg = config::AppConfig::load()?;
    let req = api::remaster::request(
        &args.clip_id,
        args.model.to_api_key(),
        args.variation.map(|v| v.as_str()),
        args.style_profile.map(|v| v.as_str()),
    )?;
    if args.dry_run {
        output::json::success(
            serde_json::json!({"dry_run":true,"endpoint":"/api/generate/upsample","request":req}),
        )?;
        return Ok(());
    }
    cfg.validate()?;
    let mut guard = guard::DuplicateGuard::new(&config::data_dir(), "remaster");
    guard.acquire(args.force)?;
    let c = client().await?;
    let clips = c
        .remaster(
            &args.clip_id,
            args.model.to_api_key(),
            args.variation.map(|v| v.as_str()),
            args.style_profile.map(|v| v.as_str()),
        )
        .await?;
    handle_generation(
        &c,
        clips,
        args.wait,
        args.download.as_deref(),
        fmt,
        ctx.quiet,
        &cfg,
    )
    .await?;
    Ok(())
}

async fn run_download(args: DownloadArgs, ctx: DispatchContext) -> Result<(), CliError> {
    let fmt = ctx.fmt;
    let cfg = config::AppConfig::load()?;
    let out_dir = args.output.unwrap_or(cfg.output_dir);
    let c = client().await?;
    let clips = c.get_clips(&args.ids).await?;
    let format = if args.video {
        DownloadFormat::Mp4
    } else {
        args.format
    };
    let source = c.download_source(args.source, format).await?;
    if clips.is_empty() {
        return Err(CliError::NotFound(format!("clip: {}", args.ids.join(", "))));
    }

    // IDs the feed didn't return don't exist (or aren't yours) —
    // report them per-item instead of failing the whole batch.
    let mut failed: Vec<serde_json::Value> = args
        .ids
        .iter()
        .filter(|id| !clips.iter().any(|c| &&c.id == id))
        .map(|id| serde_json::json!({ "id": id, "error": "not found" }))
        .collect();

    let mut paths = Vec::new();
    for clip in &clips {
        // Download + lyric-embed per clip; one bad clip (still
        // streaming, deleted mid-batch) must not sink the rest.
        let result: Result<String, CliError> = async {
            let path =
                download::download_clip(&c, clip, &out_dir, format, source, ctx.quiet).await?;
            if format == DownloadFormat::Mp3 {
                let plain_lyrics = clip.metadata.prompt.as_deref();
                let aligned = c.aligned_lyrics(&clip.id).await.ok();
                download::embed_lyrics_in_mp3(
                    &path,
                    &clip.title,
                    plain_lyrics,
                    aligned.as_deref(),
                )?;
                if !ctx.quiet {
                    eprintln!("Embedded lyrics into {path}");
                }
            }
            Ok(path)
        }
        .await;

        match result {
            Ok(path) => {
                if !ctx.quiet {
                    eprintln!("Downloaded: {path}");
                }
                paths.push(path);
            }
            Err(e) => {
                if !ctx.quiet && !matches!(fmt, OutputFormat::Json) {
                    eprintln!("Failed: {} — {e}", clip.id);
                }
                failed.push(serde_json::json!({
                    "id": clip.id,
                    "error": e.to_string(),
                }));
            }
        }
    }

    if !failed.is_empty() {
        let ids: Vec<String> = failed
            .iter()
            .filter_map(|v| v["id"].as_str().map(str::to_owned))
            .collect();
        let mut argv = vec!["suno".to_string(), "download".to_string()];
        argv.extend(ids);
        argv.extend([
            "--format".to_string(),
            format.extension().to_string(),
            "--output".to_string(),
            out_dir,
        ]);
        return Err(CliError::Diagnostic {
            source: Box::new(CliError::Download(format!(
                "{} download(s) failed; completed files are listed in details.downloaded",
                failed.len()
            ))),
            details: serde_json::json!({"downloaded": paths, "failed": failed, "next_action":{"argv":argv}}),
        });
    }
    if matches!(fmt, OutputFormat::Json) {
        output::json::success(serde_json::json!({"downloaded":paths,"failed":failed}))?;
    }
    Ok(())
}

async fn run_crop(args: CropArgs, ctx: DispatchContext, edit_remove: bool) -> Result<(), CliError> {
    let fmt = ctx.fmt;
    let remove = edit_remove;
    let cfg = config::AppConfig::load()?;
    let mut body = api::edit::crop_request(
        args.start,
        args.end,
        remove,
        args.edit
            .title
            .as_deref()
            .unwrap_or(if remove { "Cut" } else { "Crop" }),
    )?;
    if args.edit.dry_run {
        output::json::success(
            serde_json::json!({"dry_run":true,"submitted":false,"endpoint":format!("/api/edit/crop/{}/",args.edit.clip_id),"request":body}),
        )?;
        return Ok(());
    }
    cfg.validate()?;
    let c = client().await?;
    let source = c
        .get_existing_clips(std::slice::from_ref(&args.edit.clip_id))
        .await?
        .remove(0);
    api::cover::validate_range(Some(args.start), Some(args.end), source.metadata.duration)?;
    if args.edit.title.is_none() {
        body["title"] =
            format!("{} ({})", source.title, if remove { "Cut" } else { "Crop" }).into();
    }
    let id = c.crop(&args.edit.clip_id, &body).await?;
    if args.edit.wait || args.edit.download.is_some() {
        let clips = c
            .poll_edit_action(&id, cfg.poll_timeout_secs, cfg.poll_interval_secs)
            .await?;
        handle_generation(
            &c,
            clips,
            false,
            args.edit.download.as_deref(),
            fmt,
            ctx.quiet,
            &cfg,
        )
        .await?;
    } else {
        output::json::success(
            serde_json::json!({"action_clip_id":id,"status":"submitted","next_action":{"argv":["suno","edit-status",id,"--wait"]}}),
        )?;
    }
    Ok(())
}

async fn run(cli: Cli, fmt: OutputFormat) -> Result<(), CliError> {
    let ctx = DispatchContext {
        fmt,
        quiet: cli.quiet,
        headless: cli.headless,
        no_browser: cli.no_browser,
    };
    let edit_remove = matches!(&cli.command, Commands::Cut(_));
    match cli.command {
        Commands::Auth(args) => Box::pin(run_auth(args, ctx)).await?,

        Commands::Credits => {
            let info = client().await?.billing_info().await?;
            match fmt {
                OutputFormat::Json => output::json::success(&info)?,
                OutputFormat::Table => output::table::billing(&info),
            }
        }

        Commands::Models => {
            let info = client().await?.billing_info().await?;
            match fmt {
                OutputFormat::Json => output::json::success(&info.models)?,
                OutputFormat::Table => output::table::models(&info.models),
            }
        }

        Commands::List(args) => {
            let feed = client().await?.feed(args.cursor).await?;
            match fmt {
                // Object shape (not a bare clip array) so agents can page:
                // feed/v3 only accepts the opaque next_cursor token.
                OutputFormat::Json => {
                    let status = if feed.clips.is_empty() {
                        "no_results"
                    } else {
                        "success"
                    };
                    output::json::with_status(
                        status,
                        serde_json::json!({
                            "clips": feed.clips,
                            "next_cursor": feed.next_cursor,
                            "has_more": feed.has_more,
                        }),
                    )?;
                }
                OutputFormat::Table => {
                    output::table::clips(&feed.clips);
                    if let Some(cursor) = feed.next_cursor.as_deref()
                        && feed.has_more
                    {
                        eprintln!("\nNext page: suno list --cursor {cursor}");
                    }
                }
            }
        }

        Commands::Search(args) => {
            let feed = client().await?.search(&args.query).await?;
            match fmt {
                OutputFormat::Json => {
                    if feed.clips.is_empty() {
                        output::json::with_status("no_results", &feed.clips)?;
                    } else {
                        output::json::success(&feed.clips)?;
                    }
                }
                OutputFormat::Table => {
                    if feed.clips.is_empty() {
                        eprintln!("No clips matching \"{}\"", args.query);
                    } else {
                        output::table::clips(&feed.clips);
                    }
                }
            }
        }

        Commands::Lyrics(args) => {
            if !cli.quiet {
                eprintln!("Generating lyrics...");
            }
            let result = client().await?.generate_lyrics(&args.prompt).await?;
            match fmt {
                OutputFormat::Json => output::json::success(&result)?,
                OutputFormat::Table => output::table::lyrics(&result),
            }
        }

        Commands::Generate(args) => Box::pin(run_generate(args, ctx)).await?,

        Commands::Describe(args) => Box::pin(run_describe(args, ctx)).await?,

        Commands::Extend(args) => Box::pin(run_extend(args, ctx)).await?,

        Commands::Concat(args) => {
            let clip = client().await?.concat(&args.clip_id).await?;
            match fmt {
                OutputFormat::Json => output::json::success(&clip)?,
                OutputFormat::Table => output::table::clips(&[clip]),
            }
        }

        Commands::Cover(args) => Box::pin(run_cover(args, ctx)).await?,
        Commands::Reuse(args) => Box::pin(run_remix(args, "reuse", ctx)).await?,
        Commands::Replace(args) => Box::pin(run_remix(args, "replace", ctx)).await?,
        Commands::AddVocals(args) => Box::pin(run_remix(args, "add-vocals", ctx)).await?,
        Commands::AddInstrumental(args) => {
            Box::pin(run_remix(args, "add-instrumental", ctx)).await?
        }

        Commands::Crop(args) | Commands::Cut(args) => {
            Box::pin(run_crop(args, ctx, edit_remove)).await?
        }
        Commands::Speed(args) => Box::pin(run_speed(args, ctx)).await?,

        Commands::Reverse(args) => Box::pin(run_reverse(args, ctx)).await?,

        Commands::EditStatus(args) => Box::pin(run_editstatus(args, ctx)).await?,

        Commands::Remaster(args) => Box::pin(run_remaster(args, ctx)).await?,

        Commands::Stems(args) => {
            let clip = client().await?.stems(&args.clip_id).await?;
            match fmt {
                OutputFormat::Json => output::json::success(&clip)?,
                OutputFormat::Table => output::table::clips(&[clip]),
            }
        }

        Commands::Info(args) => {
            let Some(id) = args.id else {
                return commands::agent_info::run(args.command.as_deref());
            };
            let clips = client().await?.get_clips(std::slice::from_ref(&id)).await?;
            if clips.is_empty() {
                return Err(CliError::NotFound(format!("clip: {}", id)));
            }
            match fmt {
                OutputFormat::Json => output::json::success(&clips[0])?,
                OutputFormat::Table => output::table::clip_detail(&clips[0]),
            }
        }

        Commands::Persona(args) => {
            let persona = client().await?.get_persona(&args.id).await?;
            match fmt {
                OutputFormat::Json => output::json::success(&persona)?,
                OutputFormat::Table => output::table::persona(&persona),
            }
        }

        Commands::Jobs(args) => commands::jobs::run(args.limit, fmt)?,

        Commands::Status(args) => {
            let cfg = config::AppConfig::load()?;
            cfg.validate()?;
            let c = client().await?;
            let clips = if args.wait || args.download.is_some() {
                c.poll_clips(&args.ids, cfg.poll_timeout_secs, cfg.poll_interval_secs)
                    .await
                    .map_err(|e| generation_recovery(e, &args.ids))?
            } else {
                c.get_clips(&args.ids).await?
            };
            if clips.is_empty() {
                return Err(CliError::NotFound(format!(
                    "clips: {}",
                    args.ids.join(", ")
                )));
            }
            handle_generation(
                &c,
                clips,
                args.wait,
                args.download.as_deref(),
                fmt,
                cli.quiet,
                &cfg,
            )
            .await?;
        }

        Commands::Download(args) => Box::pin(run_download(args, ctx)).await?,

        Commands::Delete(args) => {
            if args.ids.is_empty() {
                return Err(CliError::InvalidInput("no clip IDs provided".into()));
            }
            // No interactive confirmation and no sleep-then-proceed: agents
            // can't answer prompts, and a timed auto-proceed deletes data the
            // caller never confirmed. Explicit -y or nothing. Restoring is
            // non-destructive (it undoes a trash), so it needs no -y.
            if !args.yes && !args.confirm && !args.restore {
                return Err(CliError::InvalidInput(format!(
                    "delete requires --confirm — re-run: suno delete {} --confirm",
                    args.ids.join(" ")
                )));
            }
            client()
                .await?
                .set_trashed(&args.ids, !args.restore)
                .await?;
            let verb = if args.restore { "restored" } else { "deleted" };
            match fmt {
                OutputFormat::Json => output::json::success(serde_json::json!({
                    verb: args.ids.len(),
                    "ids": args.ids,
                }))?,
                OutputFormat::Table => {
                    if args.restore {
                        eprintln!("Restored {} clip(s) from trash", args.ids.len());
                    } else {
                        eprintln!("Deleted {} clip(s)", args.ids.len());
                    }
                }
            }
        }

        Commands::Set(args) => {
            let lyrics = match (&args.lyrics, &args.lyrics_file) {
                (Some(l), _) => Some(l.clone()),
                (_, Some(path)) => Some(read_input_file(path)?),
                _ => None,
            };
            let req = SetMetadataRequest {
                title: args.title.clone(),
                lyrics,
                caption: args.caption.clone(),
                remove_image_cover: if args.remove_cover { Some(true) } else { None },
                remove_video_cover: None,
            };
            client().await?.set_metadata(&args.id, &req).await?;
            let mut changes = Vec::new();
            if args.title.is_some() {
                changes.push("title");
            }
            if args.lyrics.is_some() || args.lyrics_file.is_some() {
                changes.push("lyrics");
            }
            if args.caption.is_some() {
                changes.push("caption");
            }
            if args.remove_cover {
                changes.push("cover");
            }
            match fmt {
                OutputFormat::Json => output::json::success(serde_json::json!({
                    "id": args.id,
                    "updated": changes,
                }))?,
                OutputFormat::Table => eprintln!("Updated: {}", changes.join(", ")),
            }
        }

        Commands::Publish(args) => {
            let c = client().await?;
            let is_public = !args.private;
            for id in &args.ids {
                c.set_visibility(id, is_public).await?;
            }
            let state = if is_public { "public" } else { "private" };
            match fmt {
                OutputFormat::Json => output::json::success(serde_json::json!({
                    "published": args.ids,
                    "visibility": state,
                }))?,
                OutputFormat::Table => eprintln!("Set {} clip(s) to {state}", args.ids.len()),
            }
        }

        Commands::TimedLyrics(args) => {
            let words = client().await?.aligned_lyrics(&args.id).await?;
            // Empty stdout with exit 0 is correct (instrumentals have no
            // alignment) but reads like a silent failure — say why on stderr.
            if words.iter().all(|w| !w.success) && !cli.quiet {
                eprintln!("No timed lyrics available (instrumental or not yet aligned)");
            }
            if args.lrc {
                // LRC format: [mm:ss.xx] word
                for w in &words {
                    if !w.success {
                        continue;
                    }
                    let mins = (w.start_s / 60.0) as u32;
                    let secs = w.start_s % 60.0;
                    println!("[{:02}:{:05.2}] {}", mins, secs, w.word);
                }
            } else {
                match fmt {
                    OutputFormat::Json => output::json::success(&words)?,
                    OutputFormat::Table => {
                        for w in &words {
                            if w.success {
                                println!("{:>6.2}s - {:>6.2}s  {}", w.start_s, w.end_s, w.word);
                            }
                        }
                    }
                }
            }
        }

        Commands::Config(args) => match args.action {
            ConfigAction::Show => {
                let cfg = config::AppConfig::load()?;
                match fmt {
                    OutputFormat::Json => output::json::success(&cfg)?,
                    OutputFormat::Table => {
                        println!("{}", serde_json::to_string_pretty(&cfg)?)
                    }
                }
            }
            ConfigAction::Set { key, value } => {
                let path = config::AppConfig::set_value(&key, &value)?;
                match fmt {
                    OutputFormat::Json => output::json::success(serde_json::json!({
                        "updated": { "key": key, "value": value },
                        "path": path.display().to_string(),
                    }))?,
                    OutputFormat::Table => {
                        eprintln!("Set {key} = {value} in {}", path.display())
                    }
                }
            }
            ConfigAction::Path => {
                let path = config::config_path();
                match fmt {
                    OutputFormat::Json => output::json::success(serde_json::json!({
                        "path": path.display().to_string(),
                    }))?,
                    OutputFormat::Table => println!("{}", path.display()),
                }
            }
            // Parse + semantic validation of the merged effective config; the
            // auth/Chrome/network health checks this used to half-do live in
            // `doctor` now.
            ConfigAction::Check => {
                let path = config::config_path();
                config::AppConfig::load()?.validate()?;
                match fmt {
                    OutputFormat::Json => output::json::success(serde_json::json!({
                        "valid": true,
                        "path": path.display().to_string(),
                        "exists": path.exists(),
                    }))?,
                    OutputFormat::Table => eprintln!(
                        "Config OK ({}{})",
                        path.display(),
                        if path.exists() {
                            ""
                        } else {
                            " — not present, defaults apply"
                        }
                    ),
                }
            }
        },

        Commands::Doctor => commands::doctor::run(fmt, cli.quiet).await?,

        Commands::Skill(args) => match args.action {
            SkillAction::Install => commands::skill::install(fmt, cli.quiet, false)?,
            SkillAction::Status => commands::skill::status(fmt)?,
        },

        // Hidden back-compat for pre-0.6 scripts; `skill install` is the
        // real command.
        Commands::InstallSkill(args) => {
            if args.print {
                print!("{}", commands::skill::SKILL_CONTENT);
            } else if let Some(path) = args.path.as_deref() {
                commands::skill::install_to_path(path, fmt, cli.quiet, args.force)?;
            } else {
                commands::skill::install(fmt, cli.quiet, args.force)?;
            }
        }

        Commands::Update(args) => {
            // The updater uses blocking reqwest, which refuses to run on the
            // async runtime thread (panics under debug_assertions) — hop to a
            // blocking thread.
            let (check, force, quiet) = (args.check, args.force, cli.quiet);
            tokio::task::spawn_blocking(move || commands::update::run(check, force, fmt, quiet))
                .await
                .map_err(|e| CliError::Update(format!("update task failed: {e}")))??
        }

        Commands::Contract { code } => commands::contract::run(fmt, code)?,

        Commands::Prompt(args) => commands::prompt::run(args, fmt)?,
        Commands::Write(args) => commands::write::run(args, fmt, cli.quiet)?,

        Commands::Guide(args) => commands::guide::run(args.name, fmt, cli.quiet)?,

        Commands::AgentInfo(args) => commands::agent_info::run(args.command.as_deref())?,
    }

    Ok(())
}

#[tokio::main]
async fn main() {
    // Pre-scan argv for --json before clap runs so help, version, and parse
    // errors honor it too (clap hasn't populated the Cli struct on those
    // paths).
    let json_mode = std::env::args_os()
        .take_while(|a| a != "--")
        .any(|a| a == "--json")
        || !std::io::IsTerminal::is_terminal(&std::io::stdout());

    // try_parse so exit codes and envelopes stay ours, not clap's: help and
    // --version are data (exit 0, enveloped when piped), parse errors are bad
    // input (exit 3, JSON error envelope on stderr in JSON mode).
    let mut cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            if matches!(
                e.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) {
                if json_mode {
                    if let Err(error) = output::json::help(e.to_string().trim_end()) {
                        output::json::error(
                            error.error_code(),
                            &error.to_string(),
                            error.suggestion(),
                        );
                        std::process::exit(error.exit_code());
                    }
                    std::process::exit(0);
                }
                e.exit();
            }
            if json_mode {
                output::json::error(
                    "invalid_input",
                    e.to_string().trim_end(),
                    "Check arguments with `suno --help`",
                );
            } else {
                eprint!("{e}");
            }
            std::process::exit(3);
        }
    };

    // JSON stderr must be one parseable error envelope, never mixed progress.
    cli.quiet |= json_mode;
    let fmt = OutputFormat::detect(cli.json);
    // Race the command against Ctrl-C so the solver Chrome is torn down on
    // every exit path — it must never outlive the invocation.
    let result = tokio::select! {
        r = Box::pin(run(cli, fmt)) => r,
        _ = tokio::signal::ctrl_c() => {
            captcha::shutdown().await;
            eprintln!("Interrupted");
            std::process::exit(130);
        }
    };
    captcha::shutdown().await;
    if let Err(e) = result {
        if json_mode {
            output::json::error_details(
                e.error_code(),
                &e.to_string(),
                e.suggestion(),
                e.details(),
            );
        } else {
            eprintln!("Error [{}]: {}", e.error_code(), e);
            eprintln!("Hint: {}", e.suggestion());
        }
        std::process::exit(e.exit_code());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_sliders_normalize_and_clamp() {
        let s = build_control_sliders(Some(50.0), Some(150.0), Some(0.0)).unwrap();
        assert_eq!(s.weirdness_constraint, Some(0.5));
        // Out-of-range input clamps rather than sending >1.0 to the API.
        assert_eq!(s.style_weight, Some(1.0));
        assert_eq!(s.audio_weight, Some(0.0));

        // No flags → no block at all, so the request stays clean.
        assert!(build_control_sliders(None, None, None).is_none());

        // A lone --audio-influence still produces a block.
        let s = build_control_sliders(None, None, Some(65.0)).unwrap();
        assert_eq!(s.audio_weight, Some(0.65));
        assert_eq!(s.weirdness_constraint, None);
    }

    #[test]
    fn tags_merge_vocal_gender() {
        assert_eq!(
            build_tags(Some("pop"), Some(&VocalGender::Female)).as_deref(),
            Some("pop, female vocals")
        );
        assert_eq!(build_tags(None, None), None);
    }

    #[test]
    fn scaffold_preflight_covers_every_lyrics_path() {
        // The guard generate AND extend share: unresolved spans are refused...
        let scaffold = "[Verse]\n<4-6 lines — set the scene>\n";
        assert!(matches!(
            reject_unfilled_scaffold(Some(scaffold), false),
            Err(CliError::InvalidInput(_))
        ));
        // ...including spans split across lines...
        let split = "[Verse]\n<4-6 lines — set the scene:\nroad trips>\n";
        assert!(reject_unfilled_scaffold(Some(split), false).is_err());
        // ...while --allow-placeholders, filled lyrics, and no lyrics pass through.
        assert!(reject_unfilled_scaffold(Some(scaffold), true).is_ok());
        assert!(reject_unfilled_scaffold(Some("[Verse]\nwe rise\n"), false).is_ok());
        assert!(reject_unfilled_scaffold(None, false).is_ok());
    }
}
