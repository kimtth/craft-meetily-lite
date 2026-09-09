//! Official github-copilot-sdk 1.0.13 inference/runtime. No inference REST shim.
//! GitHub GET /user supplies identity metadata omitted by SDK token auth.
//! Installed gh is used only to read local credentials, never for inference.
//! Sources: https://github.com/github/copilot-sdk/blob/main/rust/README.md
//! https://docs.rs/github-copilot-sdk/1.0.13/github_copilot_sdk/
use super::schema::{validate_account, CopilotModel, CopilotStatus, MAX_INPUT_BYTES, MAX_OUTPUT_BYTES};
use github_copilot_sdk::{Client, ClientMode, ClientOptions, CliProgram, LogLevel, Transport};
use github_copilot_sdk::rpc::ModelsListRequest;
use github_copilot_sdk::types::{InfiniteSessionConfig, MemoryConfiguration, MessageOptions, SessionConfig, SessionId, SystemMessageConfig};
use serde_json::Value;
use std::{ffi::OsString, path::{Path, PathBuf}, process::Stdio, time::Duration};
use tempfile::TempDir;
use tokio::{io::AsyncReadExt, process::Command};
use tokio::time::timeout;
use uuid::Uuid;

const RPC_TIMEOUT: Duration = Duration::from_secs(30);
const TURN_TIMEOUT: Duration = Duration::from_secs(120);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);
const GH_TOKEN_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_TOKEN_BYTES: usize = 16 * 1024;
const MAX_ACCOUNTS_BYTES: usize = 16 * 1024;
const MAX_ACCOUNTS: usize = 100;
const TOKEN_ENV_NAMES: [&str; 3] = ["COPILOT_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"];
const SELECTED_AUTH_UNAVAILABLE: &str = "The selected GitHub CLI account credential is unavailable or invalid. Repair that account's local github.com login and retry; no other credential was used.";
const AUTH_IDENTITY: &str = "Copilot did not confirm the selected GitHub account identity. Request blocked; no other account was used. Refresh account status and repair the selected login.";
const AUTH_SETUP: &str = "No local GitHub credential is available. Set COPILOT_GITHUB_TOKEN, GH_TOKEN, or GITHUB_TOKEN before launching the app, or install GitHub CLI and run gh auth login --hostname github.com yourself, then retry. Copilot access is required; this app never opens a login browser.";
const AUTH_INVALID: &str = "A local GitHub credential is malformed. Replace or unset the highest-priority token variable (COPILOT_GITHUB_TOKEN, then GH_TOKEN, then GITHUB_TOKEN), or repair your github.com GitHub CLI login, and retry. Credential values are never included in diagnostics.";

// All credential reads happen inside the app, not development tools. Do not
// derive Debug, log subprocess output, or put the token in argv/options.env.
fn normalize_token(value: String) -> Result<Option<String>, &'static str> {
    if value.len() > MAX_TOKEN_BYTES { return Err(AUTH_INVALID); }
    let token = value.trim();
    if token.is_empty() { return Ok(None); }
    if !token.bytes().all(|byte| byte.is_ascii_graphic()) { return Err(AUTH_INVALID); }
    Ok(Some(token.to_owned()))
}

// Injectable lookup keeps ordinary tests independent of real credentials and
// avoids process-wide environment mutation/races between parallel tests.
fn environment_token(mut lookup: impl FnMut(&str) -> Option<OsString>) -> Result<Option<String>, &'static str> {
    for name in TOKEN_ENV_NAMES {
        if let Some(value) = lookup(name) {
            let value = value.into_string().map_err(|_| AUTH_INVALID)?;
            if let Some(token) = normalize_token(value)? { return Ok(Some(token)); }
        }
    }
    Ok(None)
}

fn gh_command(home: &Path) -> Command {
    // Use the executable, not a .cmd/.bat wrapper or a shell. Keep the user's
    // HOME/GH_CONFIG_DIR so gh can read its own local login/keychain. An explicit
    // host prevents GH_HOST or the repository cwd selecting a different account.
    let mut command = Command::new(if cfg!(windows) { "gh.exe" } else { "gh" });
    command.current_dir(home)
        .env("GH_PROMPT_DISABLED", "1")
        .env_remove("GH_DEBUG")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    // Explicit account selection deliberately bypasses ambient tokens; default
    // token lookup reaches gh only after blank/unset environment values.
    // Enumeration also reads stored accounts, not ambient token identities.
    for name in TOKEN_ENV_NAMES.into_iter().chain(["GH_ENTERPRISE_TOKEN", "GITHUB_ENTERPRISE_TOKEN", "COPILOT_SDK_AUTH_TOKEN"]) {
        command.env_remove(name);
    }
    #[cfg(windows)]
    command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    command
}

fn gh_token_command(home: &Path, account: Option<&str>) -> Result<Command, &'static str> {
    validate_account(account)?;
    let mut command = gh_command(home);
    command.args(["auth", "token", "--hostname", "github.com"]);
    if let Some(account) = account { command.args(["--user", account]); }
    Ok(command)
}

fn gh_accounts_command(home: &Path) -> Command {
    let mut command = gh_command(home);
    // Only the projected login array reaches stdout; never capture raw hosts,
    // tokens, stderr, or debug output. This command is not used by inference.
    command.args(["auth", "status", "--hostname", "github.com", "--json", "hosts", "--jq",
        "[.hosts[\"github.com\"][]?.login | select(type == \"string\")]"]);
    command
}

async fn bounded_output(mut command: Command, limit: usize) -> Result<Vec<u8>, &'static str> {
    let mut child = command.spawn().map_err(|_| AUTH_SETUP)?;
    let stdout = child.stdout.take().ok_or(AUTH_SETUP)?;
    let mut bytes = Vec::new();
    stdout.take((limit + 1) as u64).read_to_end(&mut bytes).await.map_err(|_| AUTH_SETUP)?;
    if bytes.len() > limit { return Err(AUTH_INVALID); }
    if !child.wait().await.map_err(|_| AUTH_SETUP)?.success() { return Err(AUTH_SETUP); }
    Ok(bytes)
}

fn parse_accounts(bytes: &[u8]) -> Vec<String> {
    if bytes.len() > MAX_ACCOUNTS_BYTES { return Vec::new(); }
    let Ok(mut accounts) = serde_json::from_slice::<Vec<String>>(bytes) else { return Vec::new(); };
    if accounts.len() > MAX_ACCOUNTS { return Vec::new(); }
    accounts.retain(|login| validate_account(Some(login)).is_ok());
    accounts.sort_by_key(|login| login.to_ascii_lowercase());
    accounts.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    accounts
}

// Called ONLY by the explicit copilot_status IPC, not startup or ask_copilot.
// Failure of this optional picker must not block credential/auth status.
pub(super) async fn local_accounts() -> Vec<String> {
    accounts_from_lookup(bounded_output(gh_accounts_command(&std::env::temp_dir()), MAX_ACCOUNTS_BYTES), GH_TOKEN_TIMEOUT).await
}

async fn accounts_from_lookup(
    lookup: impl std::future::Future<Output = Result<Vec<u8>, &'static str>>,
    budget: Duration,
) -> Vec<String> {
    match timeout(budget, lookup).await {
        Ok(Ok(bytes)) => parse_accounts(&bytes),
        _ => Vec::new(),
    }
}

struct Credential { token: String, source: String }

// Injected dependencies prove explicit choice bypasses even malformed ambient
// tokens, and errors never fall back to another account. Never derive Debug.
async fn resolve_credential<F, Fut>(account: Option<&str>, mut lookup: impl FnMut(&str) -> Option<OsString>, gh: F)
    -> Result<Credential, &'static str>
where
    F: FnOnce(Option<String>) -> Fut,
    Fut: std::future::Future<Output = Result<String, &'static str>>,
{
    validate_account(account)?;
    if let Some(account) = account {
        let token = gh(Some(account.to_owned())).await.map_err(|_| SELECTED_AUTH_UNAVAILABLE)?;
        return Ok(Credential { token, source: format!("gh:github.com:{account}") });
    }
    let mut source = String::new();
    if let Some(token) = environment_token(|name| {
        source = format!("env:{name}");
        lookup(name)
    })? { return Ok(Credential { token, source }); }
    Ok(Credential { token: gh(None).await?, source: "gh:github.com:active".into() })
}

async fn local_gh_token(home: &Path, account: Option<&str>) -> Result<String, &'static str> {
    let command = gh_token_command(home, account)?;
    // auth token is a local config/keychain read; no login, refresh or API call.
    // The timeout owns the child: cancellation/timeouts drop it and kill it.
    // Bound stdout as well as time; discard stderr without ever capturing logs.
    timeout(GH_TOKEN_TIMEOUT, async {
        let bytes = bounded_output(command, MAX_TOKEN_BYTES).await?;
        normalize_token(String::from_utf8(bytes).map_err(|_| AUTH_INVALID)?)?.ok_or(AUTH_SETUP)
    }).await.map_err(|_| "Local GitHub CLI credential lookup timed out after 5 seconds. Repair your gh login or set COPILOT_GITHUB_TOKEN before launching the app, then retry.")?
}

pub(super) struct Runtime {
    // Option lets close drop the client BEFORE removing its isolated home.
    client: Option<Client>,
    directory: Option<TempDir>,
    auth_unavailable: Option<&'static str>,
    // Same credential as ClientOptions, retained only in memory for account-
    // scoped discovery. Never derive Debug or log token-bearing RPC requests.
    token: Option<String>,
    credential_source: Option<String>,
    selected_account: Option<String>,
    token_login: tokio::sync::OnceCell<String>,
}

fn identity_login(bytes: &[u8]) -> Result<String, &'static str> {
    if bytes.len() > 64 * 1024 { return Err(AUTH_IDENTITY); }
    #[derive(serde::Deserialize)]
    struct Identity { login: String }
    let identity: Identity = serde_json::from_slice(bytes).map_err(|_| AUTH_IDENTITY)?;
    validate_account(Some(&identity.login)).map_err(|_| AUTH_IDENTITY)?;
    Ok(identity.login)
}

async fn github_token_login(token: &str) -> Result<String, &'static str> {
    timeout(RPC_TIMEOUT, async {
        let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none())
            .timeout(RPC_TIMEOUT).build().map_err(|_| AUTH_IDENTITY)?;
        let mut response = client.get("https://api.github.com/user")
            .header("User-Agent", "Meetly-Lite")
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .bearer_auth(token).send().await.map_err(|_| AUTH_IDENTITY)?;
        if !response.status().is_success() { return Err(AUTH_IDENTITY); }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| AUTH_IDENTITY)? {
            if bytes.len() + chunk.len() > 64 * 1024 { return Err(AUTH_IDENTITY); }
            bytes.extend_from_slice(&chunk);
        }
        identity_login(&bytes)
    }).await.map_err(|_| AUTH_IDENTITY)?
}

fn verified_identity(authenticated: bool, login: Option<&str>, selected: Option<&str>) -> Result<Option<String>, &'static str> {
    let login = login.filter(|login| validate_account(Some(login)).is_ok());
    if let Some(selected) = selected {
        validate_account(Some(selected))?;
        if !authenticated || !login.is_some_and(|login| login.eq_ignore_ascii_case(selected)) {
            return Err(AUTH_IDENTITY);
        }
    }
    if !authenticated { return Err("The selected local GitHub credential was not accepted. Replace or unset stale token variables (COPILOT_GITHUB_TOKEN takes precedence over GH_TOKEN and GITHUB_TOKEN), or run gh auth login --hostname github.com yourself, then retry. Verify that the account has Copilot access; this app never opens a login browser."); }
    Ok(login.map(str::to_owned))
}

fn model_list_request(token: &str) -> ModelsListRequest {
    ModelsListRequest { git_hub_token: Some(token.to_owned()), selection_id: None }
}

fn options(home: &Path, program: PathBuf, token: Option<String>, env_names: impl IntoIterator<Item = OsString>) -> ClientOptions {
    let mut options = ClientOptions::default();
    options.program = CliProgram::Path(program);
    options.transport = Transport::Stdio;
    options.mode = ClientMode::Empty;
    options.base_directory = Some(home.to_path_buf());
    options.working_directory = home.to_path_buf();
    options.log_level = Some(LogLevel::None);
    options.enable_remote_sessions = false;
    options.github_token = token;
    options.use_logged_in_user = Some(false);
    // A nonempty replacement list suppresses runtime bundled plugin discovery.
    options.builtin_plugin_directories = vec![home.join("empty-plugins")];
    // SDK 1.0.13 build_command injects auth/home/keytar, then options.env, THEN
    // env_remove. Never remove our explicitly controlled values: that would
    // undo isolation, telemetry suppression, or the SDK's token injection.
    options.env_remove = env_names.into_iter().filter(|key| {
        let name = key.to_string_lossy().to_ascii_uppercase();
        if matches!(name.as_str(), "COPILOT_HOME" | "COPILOT_DISABLE_KEYTAR"
            | "COPILOT_OTEL_ENABLED" | "OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT")
            || (name == "COPILOT_SDK_AUTH_TOKEN" && options.github_token.is_some()) {
            return false;
        }
        name.starts_with("COPILOT_") || name.starts_with("OTEL_")
            || name.starts_with("NODE_") || name.starts_with("GIT_CONFIG")
            || name == "GIT_DIR" || name == "GIT_WORK_TREE"
    }).collect();
    options.env_remove.extend(TOKEN_ENV_NAMES.into_iter().map(OsString::from));
    if options.github_token.is_none() { options.env_remove.push("COPILOT_SDK_AUTH_TOKEN".into()); }
    // These names are documented by SDK TelemetryConfig; no guessed flags.
    // Do not set TelemetryConfig: any populated config turns OTel ON.
    options.env = vec![
        (OsString::from("COPILOT_OTEL_ENABLED"), OsString::from("false")),
        (OsString::from("OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT"), OsString::from("false")),
    ];
    options
}

fn session_config(home: &Path, model: &str, instructions: &str) -> SessionConfig {
    // SDK structs are non_exhaustive: construct Default and assign public fields.
    let mut config = SessionConfig::default().deny_all_permissions();
    config.model = Some(model.into());
    config.session_id = Some(SessionId::new(Uuid::new_v4().to_string()));
    config.available_tools = Some(Vec::new());
    config.tools = Some(Vec::new());
    config.excluded_tools = Some(vec!["builtin:*".into(), "mcp:*".into(), "custom:*".into()]);
    config.streaming = Some(false);
    config.system_message = Some(SystemMessageConfig::new().with_mode("replace").with_content(instructions));
    config.working_directory = Some(home.into());
    config.config_directory = Some(home.into());
    config.additional_directories = Some(Vec::new());
    config.enable_config_discovery = Some(false);
    config.enable_on_demand_instruction_discovery = Some(false);
    config.skip_custom_instructions = Some(true);
    config.organization_custom_instructions = Some(String::new());
    config.instruction_directories = Some(Vec::new());
    config.skill_directories = Some(Vec::new());
    config.plugin_directories = Some(Vec::new());
    config.included_builtin_skills = Some(Vec::new());
    config.custom_agents = Some(Vec::new());
    config.custom_agents_local_only = Some(true);
    config.mcp_servers = Some(Default::default());
    config.enable_skills = Some(false);
    config.enable_mcp_apps = Some(false);
    config.enable_file_hooks = Some(false);
    config.enable_host_git_operations = Some(false);
    config.enable_file_change_tracking = Some(false);
    config.hooks = Some(false);
    config.memory = Some(MemoryConfiguration::disabled());
    config.skip_embedding_retrieval = Some(true);
    config.embedding_cache_storage = Some("in-memory".into());
    config.mcp_oauth_token_storage = Some("in-memory".into());
    config.enable_session_store = Some(false);
    config.enable_session_telemetry = Some(false);
    config.infinite_sessions = Some(InfiniteSessionConfig::new().with_enabled(false));
    config.request_extensions = Some(false);
    config.request_canvas_renderer = Some(false);
    config.include_sub_agent_streaming_events = Some(false);
    config.commands = Some(Vec::new());
    config.enable_experimental_mode = Some(false);
    config.coauthor_enabled = Some(false);
    config.manage_schedule_enabled = Some(false);
    config
}

impl Runtime {
    pub(super) async fn start(account: Option<&str>) -> Result<Self, String> {
        // Reject invalid selection before extraction, environment reads or gh.
        validate_account(account)?;
        // Extraction is SDK-managed, local only, and off the async executor.
        // Explicit path prevents COPILOT_CLI_PATH or a PATH shim from winning.
        let program = tokio::task::spawn_blocking(github_copilot_sdk::install_bundled_runtime)
            .await.map_err(|_| "Bundled Copilot runtime extraction failed")?
            .ok_or("Bundled Copilot runtime is missing for this platform; verify the packaged SDK build")?;
        let directory = tempfile::Builder::new().prefix("meetly-copilot-").tempdir()
            .map_err(|_| "Could not create isolated Copilot directory")?;
        std::fs::create_dir(directory.path().join("empty-plugins"))
            .map_err(|_| "Could not isolate Copilot plugins")?;
        let home = directory.path();
        let (token, credential_source, auth_unavailable) = match resolve_credential(
            account, |name| std::env::var_os(name),
            |selected| async move { local_gh_token(home, selected.as_deref()).await },
        ).await {
            Ok(credential) => (Some(credential.token), Some(credential.source), None),
            Err(detail) => (None, account.map(|login| format!("gh:github.com:{login}")), Some(detail)),
        };
        // Missing auth must not prevent the local runtime handshake. status()
        // reports actionable local setup guidance without attempting auth RPCs.
        let options = options(directory.path(), program, token.clone(), std::env::vars_os().map(|(key, _)| key));
        let client = timeout(RPC_TIMEOUT, Client::start(options))
            .await.map_err(|_| "Copilot runtime startup timed out")?
            .map_err(|_| "Copilot runtime could not start; verify bundled runtime and account setup")?;
        Ok(Self { client: Some(client), directory: Some(directory), auth_unavailable, token,
            credential_source, selected_account: account.map(str::to_owned), token_login: tokio::sync::OnceCell::new() })
    }

    async fn identity(&self) -> Result<Option<String>, &'static str> {
        if let Some(detail) = self.auth_unavailable { return Err(detail); }
        let client = self.client.as_ref().expect("runtime open");
        let auth = match timeout(RPC_TIMEOUT, client.get_auth_status()).await {
            Ok(Ok(auth)) => auth,
            _ => return Err("Copilot authentication status is unavailable or timed out"),
        };
        let mut login = auth.login;
        if auth.is_authenticated && login.is_none() {
            let token = self.token.as_deref().ok_or(AUTH_SETUP)?;
            match self.token_login.get_or_try_init(|| github_token_login(token)).await {
                Ok(value) => login = Some(value.clone()),
                Err(error) if self.selected_account.is_some() => return Err(error),
                Err(_) => {}, // Default auth can remain usable with unknown identity.
            }
        }
        verified_identity(auth.is_authenticated, login.as_deref(), self.selected_account.as_deref())
    }

    pub(super) async fn status(&self) -> CopilotStatus {
        let mut status = CopilotStatus::unavailable(AUTH_SETUP);
        status.credential_source = self.credential_source.clone();
        status.login = match self.identity().await {
            Ok(login) => login,
            Err(detail) => { status.detail = detail.into(); return status; }
        };
        let Some(token) = self.token.as_deref() else { return status; };
        status.authenticated = true;
        let client = self.client.as_ref().expect("runtime open");
        // Explicitly scope discovery to the same credential as inference, not
        // an ambient account. Pinned SDK 1.0.13 / bundled runtime 1.0.83.
        // This does not override server-side model_picker_enabled filtering.
        let models = match timeout(RPC_TIMEOUT, client.rpc().models().list_with_params(model_list_request(token))).await {
            Ok(Ok(result)) => result.models,
            _ => {
                status.detail = "Authenticated, but model discovery failed or timed out; verify Copilot entitlement and network access".into();
                return status;
            }
        };
        let mut models: Vec<_> = models.into_iter().map(|model| CopilotModel { id: model.id, name: model.name }).collect();
        models.sort_by(|a, b| a.id.cmp(&b.id));
        models.dedup_by(|a, b| a.id == b.id);
        status.detail = if models.is_empty() { "Authenticated, but no models are available".into() }
            else if models.iter().all(|model| model.id == "auto") {
                "This GitHub Copilot SDK connection currently offers only Auto. GitHub selects the model; manual model selection is unavailable here. Available models can differ from Copilot in VS Code or on the web. No inference was performed.".into()
            } else { "Authenticated. Models were queried for the same GitHub credential used for inference; this SDK surface may differ from other Copilot products. Entitlement and policy are enforced when invoked. No inference was performed by this status check.".into() };
        status.models = models;
        status
    }

    pub(super) async fn generate(&self, model: &str, instructions: &str, input: Value) -> Result<String, String> {
        if let Some(detail) = self.auth_unavailable { return Err(detail.into()); }
        // Every selected-account turn (including chunks/retries) rechecks SDK
        // identity BEFORE creating a session or sending any meeting content.
        // Default behavior is unchanged: ask_copilot already checks status.
        if self.selected_account.is_some() { self.identity().await?; }
        let prompt = serde_json::to_string(&input).map_err(|_| "Could not encode assistant request")?;
        if prompt.len() + instructions.len() > MAX_INPUT_BYTES {
            return Err("Assistant context exceeds the 96000-byte budget; nothing was silently truncated".into());
        }
        let client = self.client.as_ref().expect("runtime open");
        let home = self.directory.as_ref().expect("runtime directory").path();
        let session = timeout(RPC_TIMEOUT, client.create_session(session_config(home, model, instructions)))
            .await.map_err(|_| "Copilot session creation timed out")?
            .map_err(|_| "Copilot session unavailable; check authentication, model entitlement and isolation support")?;
        // SDK waiter tracks final assistant.message and session.idle, cleans its
        // slot on cancellation, and reports session.error instead of partial success.
        let result = timeout(TURN_TIMEOUT, session.send_and_wait(
            MessageOptions::new(prompt).with_wait_timeout(TURN_TIMEOUT),
        )).await;
        let output = match result {
            Ok(Ok(Some(event))) => event.data.get("content").and_then(Value::as_str)
                .filter(|text| !text.trim().is_empty() && text.len() <= MAX_OUTPUT_BYTES)
                .map(str::to_owned).ok_or_else(|| "Copilot returned empty or oversized final content".to_string()),
            Ok(Ok(None)) => Err("Copilot returned no final answer".into()),
            Ok(Err(_)) => Err("Copilot inference failed; check account, model availability and network, then retry".into()),
            Err(_) => Err("Copilot inference timed out; no partial response was accepted".into()),
        };
        if output.is_err() { let _ = timeout(CLEANUP_TIMEOUT, session.abort()).await; }
        let id = session.id().clone();
        let detached = timeout(CLEANUP_TIMEOUT, session.disconnect()).await;
        drop(session);
        let deleted = timeout(CLEANUP_TIMEOUT, client.delete_session(&id)).await;
        if !matches!(detached, Ok(Ok(()))) || !matches!(deleted, Ok(Ok(()))) {
            client.force_stop();
            return Err("Copilot session cleanup failed; request stopped and no assistant data was saved".into());
        }
        output
    }

    pub(super) async fn close(mut self) -> Result<(), String> {
        if let Some(client) = self.client.as_ref() {
            if !matches!(timeout(CLEANUP_TIMEOUT, client.stop()).await, Ok(Ok(()))) { client.force_stop(); }
        }
        self.client.take();
        // Report failure instead of claiming successful private cleanup. TempDir
        // Drop retries if Windows still holds files briefly after process exit.
        if let Some(directory) = self.directory.as_ref() {
            std::fs::remove_dir_all(directory.path())
                .map_err(|_| "Copilot temporary data cleanup failed; no assistant result was saved. A meetly-copilot-* directory may remain in the OS temp directory".to_string())?;
        }
        Ok(())
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        if let Some(client) = self.client.take() { client.force_stop(); }
        // TempDir then drops AFTER the owned child has been force-stopped.
        // OS crash/power loss may leave a temp directory; see handoff limitations.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_metadata_is_bounded_validated_and_not_an_auth_response_dump() {
        assert_eq!(identity_login(br#"{"login":"synthetic_managed","other":"ignored"}"#).unwrap(), "synthetic_managed");
        for value in [br#"{"login":"bad\nlogin"}"#.as_slice(), br#"{"token":"not-identity"}"#, b"invalid"] {
            assert_eq!(identity_login(value), Err(AUTH_IDENTITY));
        }
        assert_eq!(identity_login(&vec![b' '; 65537]), Err(AUTH_IDENTITY));
    }

    #[tokio::test]
    async fn explicit_account_bypasses_environment_and_never_falls_back() {
        let credential = resolve_credential(Some("synthetic_managed"),
            |_| panic!("Explicit selection must not read token environment"),
            |account| {
                assert_eq!(account.as_deref(), Some("synthetic_managed"));
                std::future::ready(Ok("synthetic-selected-token".into()))
            }).await.unwrap_or_else(|_| panic!("Synthetic resolution failed"));
        assert_eq!(credential.token, "synthetic-selected-token");
        assert_eq!(credential.source, "gh:github.com:synthetic_managed");
        for error in [AUTH_SETUP, AUTH_INVALID, "PRIVATE SYNTHETIC STDERR"] {
            let mut calls = 0;
            let result = resolve_credential(Some("synthetic_managed"),
                |_| panic!("Selected-account failure must never read environment"),
                |account| {
                    calls += 1;
                    assert_eq!(account.as_deref(), Some("synthetic_managed"));
                    std::future::ready(Err(error))
                }).await;
            assert!(matches!(result, Err(SELECTED_AUTH_UNAVAILABLE)));
            assert_eq!(calls, 1);
        }
    }

    #[tokio::test]
    async fn invalid_account_rejected_before_any_credential_lookup_or_runtime_start() {
        for invalid in ["", " ", "-flag", "--user=other", "a b", "a\nb", "a\0b", "../other",
            "x;whoami", "x&y", "한글", "ａｂｃ", "a@b", "_managed", "a-", "a_"] {
            let result = resolve_credential(Some(invalid),
                |_| panic!("Invalid selection must not read environment"),
                |_| { panic!("Invalid selection must not run gh");
                    #[allow(unreachable_code)] std::future::ready(Err(AUTH_SETUP)) }).await;
            assert!(result.is_err());
            assert!(gh_token_command(Path::new("isolated"), Some(invalid)).is_err());
            assert!(Runtime::start(Some(invalid)).await.is_err());
        }
        assert!(validate_account(Some(&"a".repeat(101))).is_err());
        for valid in ["a", "synthetic-user", "synthetic_managed", &"a".repeat(100)] {
            assert!(validate_account(Some(valid)).is_ok());
        }
        assert!(validate_account(None).is_ok());
    }

    #[tokio::test]
    async fn default_resolution_keeps_precedence_and_reports_actual_source() {
        for (index, expected) in TOKEN_ENV_NAMES.into_iter().enumerate() {
            let credential = resolve_credential(None, |name| {
                let position = TOKEN_ENV_NAMES.iter().position(|candidate| *candidate == name).unwrap();
                assert!(position <= index, "No lower-priority credential should be read");
                Some(if position == index { "synthetic-token" } else { " \r\n" }.into())
            }, |_| { panic!("Environment token must bypass gh");
                #[allow(unreachable_code)] std::future::ready(Err(AUTH_SETUP)) }).await
                .unwrap_or_else(|_| panic!("Synthetic resolution failed"));
            assert_eq!(credential.source, format!("env:{expected}"));
            assert_eq!(credential.token, "synthetic-token");
        }
        let credential = resolve_credential(None, |_| None, |account| {
            assert!(account.is_none());
            std::future::ready(Ok("synthetic-active".into()))
        }).await.unwrap_or_else(|_| panic!("Synthetic resolution failed"));
        assert_eq!(credential.source, "gh:github.com:active");
        assert_eq!(credential.token, "synthetic-active");
        let result = resolve_credential(None, |_| Some("invalid token".into()),
            |_| { panic!("Malformed environment must not fall back to gh");
                #[allow(unreachable_code)] std::future::ready(Err(AUTH_SETUP)) }).await;
        assert!(matches!(result, Err(AUTH_INVALID)));
        assert!(matches!(resolve_credential(None, |_| None,
            |_| std::future::ready(Err(AUTH_SETUP))).await, Err(AUTH_SETUP)));
    }

    #[test]
    fn selected_account_commands_are_scoped_and_enumeration_projects_only_names() {
        let token = gh_token_command(Path::new("isolated"), Some("synthetic_managed")).unwrap();
        assert_eq!(token.as_std().get_args().collect::<Vec<_>>(),
            ["auth", "token", "--hostname", "github.com", "--user", "synthetic_managed"]);
        let accounts = gh_accounts_command(Path::new("isolated"));
        assert_eq!(accounts.as_std().get_args().collect::<Vec<_>>(),
            ["auth", "status", "--hostname", "github.com", "--json", "hosts", "--jq",
             "[.hosts[\"github.com\"][]?.login | select(type == \"string\")]"]);
        for command in [token, accounts] {
            let command = command.as_std();
            assert_eq!(command.get_program(), if cfg!(windows) { "gh.exe" } else { "gh" });
            let env: std::collections::BTreeMap<_, _> = command.get_envs().collect();
            for name in TOKEN_ENV_NAMES.into_iter().chain(["GH_DEBUG", "GH_ENTERPRISE_TOKEN", "GITHUB_ENTERPRISE_TOKEN", "COPILOT_SDK_AUTH_TOKEN"]) {
                assert_eq!(env.get(std::ffi::OsStr::new(name)), Some(&None));
            }
            assert!(!command.get_args().any(|arg| arg == "switch" || arg == "login" || arg == "logout"));
        }
    }

    #[test]
    fn account_enumeration_is_bounded_sanitized_and_case_insensitively_deduplicated() {
        assert_eq!(parse_accounts(br#"["z-user","Synthetic_managed","synthetic_managed","a","bad login","--flag","","private\ntext"]"#),
            ["a", "Synthetic_managed", "z-user"]);
        for malformed in [b"not json".as_slice(), br#"{"hosts":{"github.com":[]}}"#, br#"[null,42]"#, &[0xff]] {
            assert!(parse_accounts(malformed).is_empty());
        }
        assert!(parse_accounts(&vec![b' '; MAX_ACCOUNTS_BYTES + 1]).is_empty());
        let many = serde_json::to_vec(&vec!["synthetic"; MAX_ACCOUNTS + 1]).unwrap();
        assert!(parse_accounts(&many).is_empty());
    }

    #[tokio::test]
    async fn optional_account_lookup_errors_and_timeout_leave_credential_status_unchanged() {
        for lookup in [Err(AUTH_SETUP), Err(AUTH_INVALID), Err("PRIVATE SYNTHETIC ERROR"),
            Ok(b"not json".to_vec())] {
            let mut status = CopilotStatus { authenticated: true, detail: "Synthetic status".into(),
                models: vec![CopilotModel { id: "auto".into(), name: "Auto".into() }],
                login: Some("synthetic".into()), credential_source: Some("env:GH_TOKEN".into()),
                accounts: Vec::new() };
            let before = serde_json::to_value(&status).unwrap();
            status.accounts = accounts_from_lookup(std::future::ready(lookup), GH_TOKEN_TIMEOUT).await;
            assert_eq!(serde_json::to_value(&status).unwrap(), before);
        }
        // Pending future has no process, environment access or network traffic.
        let accounts = accounts_from_lookup(std::future::pending(), Duration::from_millis(1)).await;
        assert!(accounts.is_empty());
        assert_eq!(accounts_from_lookup(std::future::ready(Ok(br#"["synthetic"]"#.to_vec())),
            GH_TOKEN_TIMEOUT).await, ["synthetic"]);
    }

    #[test]
    fn explicit_identity_requires_authenticated_safe_matching_sdk_login() {
        for (authenticated, login) in [(false, Some("synthetic_managed")), (true, None),
            (true, Some("")), (true, Some("different")), (true, Some("private\nresponse"))] {
            assert!(matches!(verified_identity(authenticated, login, Some("synthetic_managed")), Err(AUTH_IDENTITY)));
        }
        assert_eq!(verified_identity(true, Some("Synthetic_managed"), Some("synthetic_managed")).unwrap(),
            Some("Synthetic_managed".into()));
        // Missing identity did not block default auth previously; preserve it.
        assert_eq!(verified_identity(true, None, None).unwrap(), None);
        assert_eq!(verified_identity(true, Some("bad\nresponse"), None).unwrap(), None);
        assert_eq!(verified_identity(true, Some("synthetic-active"), None).unwrap(), Some("synthetic-active".into()));
        assert!(verified_identity(false, Some("synthetic-active"), None).is_err());
    }

    #[tokio::test]
    async fn selected_credential_failure_preserves_source_but_never_authenticates_or_generates() {
        let runtime = Runtime { client: None, directory: None, auth_unavailable: Some(SELECTED_AUTH_UNAVAILABLE),
            token: None, credential_source: Some("gh:github.com:synthetic_managed".into()),
            selected_account: Some("synthetic_managed".into()), token_login: tokio::sync::OnceCell::new() };
        let status = runtime.status().await;
        assert!(!status.authenticated);
        assert_eq!(status.login, None);
        assert_eq!(status.credential_source.as_deref(), Some("gh:github.com:synthetic_managed"));
        assert!(status.accounts.is_empty());
        assert!(status.models.is_empty());
        assert_eq!(runtime.generate("synthetic", "synthetic", Value::Null).await.unwrap_err(), SELECTED_AUTH_UNAVAILABLE);
        runtime.close().await.unwrap();
    }

    #[test]
    fn token_environment_precedence_and_blank_fallback_are_synthetic() {
        for (values, expected) in [
            ([Some(" synthetic-copilot "), Some("synthetic-gh"), Some("synthetic-github")], Some("synthetic-copilot")),
            ([Some(" \r\n"), Some("synthetic-gh"), Some("synthetic-github")], Some("synthetic-gh")),
            ([None, Some(""), Some("synthetic-github")], Some("synthetic-github")),
            ([None, None, None], None),
        ] {
            let selected = environment_token(|name| {
                TOKEN_ENV_NAMES.iter().position(|candidate| *candidate == name)
                    .and_then(|index| values[index].map(OsString::from))
            }).expect("synthetic environment should parse");
            assert_eq!(selected.as_deref(), expected);
        }
        // Once selected, do not even read lower-priority credentials.
        let selected = environment_token(|name| {
            assert_eq!(name, "COPILOT_GITHUB_TOKEN");
            Some("synthetic-first".into())
        }).unwrap();
        assert_eq!(selected.as_deref(), Some("synthetic-first"));
    }

    #[test]
    fn malformed_local_credentials_fail_closed_without_echoing_values() {
        for invalid in ["synthetic token", "synthetic\nsecond-line", "synthetic\0token", "synthetic-한글"] {
            let result = environment_token(|name| {
                assert_eq!(name, "COPILOT_GITHUB_TOKEN");
                Some(invalid.into())
            });
            assert!(matches!(result, Err(AUTH_INVALID)));
        }
        assert!(matches!(normalize_token("x".repeat(MAX_TOKEN_BYTES + 1)), Err(AUTH_INVALID)));
        assert_eq!(normalize_token("synthetic-gh\r\n".into()).unwrap().as_deref(), Some("synthetic-gh"));
    }

    #[test]
    fn gh_lookup_is_a_local_noninteractive_executable_command() {
        // Inspect only a command built with synthetic paths; never spawn gh.
        let command = gh_token_command(Path::new("isolated"), None).unwrap();
        let command = command.as_std();
        assert_eq!(command.get_program(), if cfg!(windows) { "gh.exe" } else { "gh" });
        assert_eq!(command.get_args().collect::<Vec<_>>(), ["auth", "token", "--hostname", "github.com"]);
        assert_eq!(command.get_current_dir(), Some(Path::new("isolated")));
        let env: std::collections::BTreeMap<_, _> = command.get_envs().collect();
        assert_eq!(env.get(std::ffi::OsStr::new("GH_PROMPT_DISABLED")), Some(&Some(std::ffi::OsStr::new("1"))));
        for name in TOKEN_ENV_NAMES.into_iter().chain(["GH_DEBUG"]) {
            assert_eq!(env.get(std::ffi::OsStr::new(name)), Some(&None));
        }
        for name in ["HOME", "USERPROFILE", "GH_CONFIG_DIR"] {
            assert!(!env.contains_key(std::ffi::OsStr::new(name)));
        }
    }

    #[test]
    fn sdk_managed_environment_survives_last_applied_removals() {
        let controlled = ["COPILOT_HOME", "COPILOT_DISABLE_KEYTAR", "COPILOT_OTEL_ENABLED",
            "OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT"];
        let stripped = ["COPILOT_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN", "COPILOT_CLI_PATH",
            "COPILOT_SDK_DEFAULT_CONNECTION", "COPILOT_CUSTOM_INSTRUCTIONS_DIRS",
            "OTEL_EXPORTER_OTLP_ENDPOINT", "NODE_OPTIONS", "GIT_CONFIG_COUNT", "GIT_DIR", "GIT_WORK_TREE"];
        for token in [None, Some("synthetic-token".to_string())] {
            let has_token = token.is_some();
            let names = controlled.into_iter().chain(stripped).chain(["COPILOT_SDK_AUTH_TOKEN", "PATH", "HOME", "GH_CONFIG_DIR"])
                .map(OsString::from);
            let options = options(Path::new("isolated"), "bundled-runtime".into(), token, names);
            let removes = |name: &str| options.env_remove.iter().any(|key| key == name);
            for name in controlled { assert!(!removes(name), "SDK-controlled name was removed: {name}"); }
            for name in stripped { assert!(removes(name), "Ambient override was retained: {name}"); }
            assert_eq!(removes("COPILOT_SDK_AUTH_TOKEN"), !has_token);
            assert_eq!(options.github_token.is_some(), has_token);
            assert_eq!(options.use_logged_in_user, Some(false));
            assert_eq!(options.working_directory, PathBuf::from("isolated"));
            assert_eq!(options.builtin_plugin_directories, vec![PathBuf::from("isolated").join("empty-plugins")]);
            assert!(options.telemetry.is_none());
            assert!(options.on_github_telemetry.is_none());
            assert_eq!(options.log_level, Some(LogLevel::None));
            assert_eq!(options.env, vec![
                (OsString::from("COPILOT_OTEL_ENABLED"), OsString::from("false")),
                (OsString::from("OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT"), OsString::from("false")),
            ]);
            // SDK alone transports the token: neither argv nor the generic env
            // vector (whose Debug implementation exposes values) contains it.
            assert!(options.extra_args.is_empty());
            assert!(options.prefix_args.is_empty());
        }
    }

    #[test]
    fn client_is_stdio_empty_mode_without_ambient_program_override() {
        let options = options(Path::new("isolated"), PathBuf::from("bundled-runtime"), None, Vec::new());
        assert!(matches!(options.transport, Transport::Stdio));
        assert_eq!(options.mode, ClientMode::Empty);
        assert!(matches!(options.program, CliProgram::Path(_)));
        assert!(!options.enable_remote_sessions);
        assert!(options.extra_args.is_empty());
        assert!(options.telemetry.is_none());
        assert_eq!(options.base_directory, Some(PathBuf::from("isolated")));
    }
    #[test]
    fn session_has_no_tools_or_ambient_context() {
        let config = session_config(Path::new("isolated"), "model", "instructions");
        assert_eq!(config.available_tools, Some(Vec::new()));
        assert_eq!(config.enable_config_discovery, Some(false));
        assert_eq!(config.enable_on_demand_instruction_discovery, Some(false));
        assert_eq!(config.working_directory, Some(PathBuf::from("isolated")));
        assert_eq!(config.config_directory, Some(PathBuf::from("isolated")));
        assert_eq!(config.additional_directories, Some(Vec::new()));
        assert_eq!(config.instruction_directories, Some(Vec::new()));
        assert_eq!(config.skill_directories, Some(Vec::new()));
        assert_eq!(config.plugin_directories, Some(Vec::new()));
        assert_eq!(config.enable_skills, Some(false));
        assert_eq!(config.enable_session_store, Some(false));
        assert_eq!(config.enable_session_telemetry, Some(false));
        assert_eq!(config.enable_file_hooks, Some(false));
        assert_eq!(config.enable_host_git_operations, Some(false));
        assert_eq!(config.hooks, Some(false));
        assert_eq!(config.skip_custom_instructions, Some(true));
        assert_eq!(config.infinite_sessions.unwrap().enabled, Some(false));
        assert_eq!(config.system_message.unwrap().mode.as_deref(), Some("replace"));
        assert!(config.custom_agents.unwrap().is_empty());
        assert!(config.mcp_servers.unwrap().is_empty());
    }

    #[tokio::test]
    async fn missing_local_credentials_return_guidance_without_any_client() {
        // No process, actual environment, RPC or network is involved.
        let runtime = Runtime { client: None, directory: None, auth_unavailable: Some(AUTH_SETUP), token: None,
            credential_source: None, selected_account: None, token_login: tokio::sync::OnceCell::new() };
        let status = runtime.status().await;
        assert!(!status.authenticated);
        assert!(status.models.is_empty());
        assert_eq!(status.detail, AUTH_SETUP);
        assert!(matches!(runtime.generate("synthetic-model", "synthetic-instructions", Value::Null).await,
            Err(detail) if detail == AUTH_SETUP));
        assert!(runtime.close().await.is_ok());
    }

    #[test]
    fn model_discovery_is_explicitly_scoped_to_the_selected_credential() {
        let request = model_list_request("synthetic-model-token");
        let wire = serde_json::to_value(request).unwrap();
        assert_eq!(wire, serde_json::json!({"gitHubToken":"synthetic-model-token"}));
        let options = options(Path::new("isolated"), "bundled-runtime".into(), Some("synthetic-model-token".into()), Vec::new());
        assert_eq!(options.github_token.as_deref(), wire["gitHubToken"].as_str());
        assert_eq!(options.mode, ClientMode::Empty);
        assert_eq!(options.use_logged_in_user, Some(false));
    }

    /// Explicit opt-in only: contacts GitHub for account/model metadata, never
    /// creates a session or sends a transcript/prompt. Logs IDs, not auth data.
    #[tokio::test]
    #[ignore = "NETWORK METADATA ONLY: requires user approval to check GitHub authentication and model catalog"]
    async fn approved_account_model_catalog_metadata_only() {
        let account = std::env::var("MEETLY_TEST_GITHUB_ACCOUNT").ok();
        let runtime = Runtime::start(account.as_deref()).await.unwrap_or_else(|detail| panic!("{detail}"));
        let status = runtime.status().await;
        let closed = runtime.close().await;
        println!("Selected-credential model IDs: {}", status.models.iter().map(|model| model.id.as_str()).collect::<Vec<_>>().join(", "));
        println!("Catalog status: {}", status.detail);
        println!("Connected login: {}", status.login.as_deref().unwrap_or("unavailable"));
        assert!(closed.is_ok(), "Metadata-check runtime cleanup failed");
        assert!(status.authenticated, "Metadata authentication failed; see sanitized status");
        // Auto alone is a valid restricted catalog, not evidence that manual
        // selection is available. This smoke test does not prove entitlement.
        assert!(!status.models.is_empty(), "No account models returned");
        if let Some(account) = account {
            assert!(status.login.as_deref().is_some_and(|login| login.eq_ignore_ascii_case(&account)), "Account identity mismatch");
            assert!(status.models.iter().any(|model| model.id != "auto"), "Selected-account regression: no manually selectable models returned");
        }
    }

    /// LOCAL-ONLY, opt-in transport smoke. It reads local credentials at startup
    /// but never checks auth, lists models, creates sessions or sends inference.
    /// SDK startup + ping + close only; telemetry uses the production off policy.
    /// This does not prove entitlement or enforce an OS-level network sandbox.
    #[tokio::test]
    #[ignore = "LOCAL-ONLY: explicitly run local_bundled_runtime_handshake; starts the bundled runtime and reads local credentials"]
    async fn local_bundled_runtime_handshake() {
        let runtime = Runtime::start(None).await.unwrap_or_else(|detail| panic!("{detail}"));
        let home = runtime.directory.as_ref().expect("runtime directory").path().to_path_buf();
        // Save only success, never an SDK response/error which might carry
        // sensitive details. Always close even when the ping fails or times out.
        let ping_ok = matches!(timeout(RPC_TIMEOUT, runtime.client.as_ref().expect("runtime open").ping(None)).await, Ok(Ok(_)));
        let close_result = runtime.close().await;
        assert!(close_result.is_ok(), "Local runtime cleanup failed; inspect OS temp meetly-copilot-* directories");
        assert!(!home.exists(), "Isolated runtime directory was not removed");
        assert!(ping_ok, "Bundled runtime local ping failed or timed out");
    }
}