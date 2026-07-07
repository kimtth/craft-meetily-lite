use serde::{Deserialize, Serialize};
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
use std::process::Command;

/// Resource required to call Azure AI Speech (Cognitive Services) with a bearer token.
const COGNITIVE_SERVICES_RESOURCE: &str = "https://cognitiveservices.azure.com/";
/// Resource used only to confirm that a usable Azure CLI token can be issued.
const CONFIRM_RESOURCE: &str = "https://management.azure.com/";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AzureCliAccessToken {
    pub token: String,
    pub expires_on_timestamp: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AzureCliState {
    Connected,
    Unconfigured,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AzureCliStatus {
    pub id: String,
    pub state: AzureCliState,
    pub detail: String,
    pub account: Option<String>,
}

#[derive(Debug, Clone)]
struct AzCommandResult {
    code: i32,
    stdout: String,
    stderr: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AzAccessTokenResponse {
    access_token: String,
    #[serde(default, rename = "expires_on")]
    expires_on: Option<i64>,
}

fn normalized_optional(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Build `az account get-access-token` arguments. The Azure CLI rejects
/// `--tenant` and `--subscription` together, so prefer tenant. The subscription
/// is still honored after sign-in through `az account set`.
fn access_token_args(
    resource: &str,
    tenant_id: Option<String>,
    subscription_id: Option<String>,
) -> Vec<String> {
    let tenant_id = normalized_optional(tenant_id);
    let subscription_id = normalized_optional(subscription_id);
    let mut args = vec![
        "account".to_string(),
        "get-access-token".to_string(),
        "--resource".to_string(),
        resource.to_string(),
        "--only-show-errors".to_string(),
        "-o".to_string(),
        "json".to_string(),
    ];
    if let Some(tenant_id) = tenant_id {
        args.push("--tenant".to_string());
        args.push(tenant_id);
    } else if let Some(subscription_id) = subscription_id {
        args.push("--subscription".to_string());
        args.push(subscription_id);
    }
    args
}

/// Build a `Command` that invokes the Azure CLI. On Windows the CLI ships as
/// `az.cmd` (a batch script) that `Command::new("az")` cannot resolve, because
/// Windows process creation only searches for `az.exe`. Routing through
/// `cmd /C az` lets the shell resolve `az.cmd` on PATH; CREATE_NO_WINDOW keeps
/// a console window from flashing in the windowed desktop app.
fn az_command() -> Command {
    #[cfg(target_os = "windows")]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut command = Command::new("cmd");
        command.arg("/C").arg("az").creation_flags(CREATE_NO_WINDOW);
        command
    }
    #[cfg(not(target_os = "windows"))]
    {
        Command::new("az")
    }
}

fn run_az(args: &[&str]) -> AzCommandResult {
    match az_command().args(args).output() {
        Ok(output) => AzCommandResult {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        },
        Err(error) => AzCommandResult {
            code: -1,
            stdout: String::new(),
            stderr: error.to_string(),
        },
    }
}

fn run_az_owned(args: Vec<String>) -> AzCommandResult {
    match az_command().args(args).output() {
        Ok(output) => AzCommandResult {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        },
        Err(error) => AzCommandResult {
            code: -1,
            stdout: String::new(),
            stderr: error.to_string(),
        },
    }
}

fn get_cli_access_token_blocking(
    resource: &str,
    tenant_id: Option<String>,
    subscription_id: Option<String>,
) -> Result<AzureCliAccessToken, String> {
    let result = run_az_owned(access_token_args(resource, tenant_id, subscription_id));
    if result.code != 0 {
        let detail = result.stderr.trim();
        return Err(if detail.is_empty() {
            "Azure CLI could not issue an access token. Sign in, then retry.".to_string()
        } else {
            detail.to_string()
        });
    }
    let token: AzAccessTokenResponse = serde_json::from_str(&result.stdout)
        .map_err(|error| format!("Azure CLI returned an invalid token response: {error}"))?;
    Ok(AzureCliAccessToken {
        token: token.access_token,
        expires_on_timestamp: token
            .expires_on
            .unwrap_or_else(|| chrono::Utc::now().timestamp() + 50 * 60)
            .saturating_mul(1000),
    })
}

async fn get_cli_access_token(
    resource: &'static str,
    tenant_id: Option<String>,
    subscription_id: Option<String>,
) -> Result<AzureCliAccessToken, String> {
    tokio::task::spawn_blocking(move || {
        get_cli_access_token_blocking(resource, tenant_id, subscription_id)
    })
    .await
    .unwrap_or_else(|error| Err(format!("Azure token task failed to run: {error}")))
}

fn current_account() -> String {
    let result = run_az(&["account", "show", "--query", "user.name", "-o", "tsv"]);
    if result.code == 0 {
        result.stdout.trim().to_string()
    } else {
        String::new()
    }
}

fn connected_status(account: String) -> AzureCliStatus {
    AzureCliStatus {
        id: "azure".to_string(),
        state: AzureCliState::Connected,
        detail: if account.is_empty() {
            "Signed in to Azure.".to_string()
        } else {
            format!("Signed in to Azure as {account}.")
        },
        account: if account.is_empty() {
            None
        } else {
            Some(account)
        },
    }
}

fn failed_login_status(stderr: &str, tenant_id: Option<&str>) -> AzureCliStatus {
    let missing_cli = stderr.contains("program not found")
        || stderr.contains("The system cannot find the file")
        || stderr.contains("os error 2")
        || stderr.contains("not recognized")
        || stderr.contains("not found")
        || stderr.contains("ENOENT");
    let canceled = stderr.contains("user canceled the flow")
        || stderr.contains("Status_UserCanceled")
        || stderr.contains("Authentication failed against tenant");
    let wrong_tenant =
        stderr.contains("AADSTS50020") || stderr.contains("does not exist in tenant");

    let detail = if missing_cli {
        "Azure CLI (az) is not installed or not on PATH. Install it, then retry.".to_string()
    } else if canceled {
        "Sign-in was canceled. Click sign in again and pick an account in the browser.".to_string()
    } else if wrong_tenant {
        match tenant_id {
            Some(tenant_id) => format!(
                "That account is not a member of tenant {tenant_id}. Pick an account that belongs to this tenant, is a guest of this tenant, or clear the Tenant ID to use the account's home tenant."
            ),
            None => "That account is not a member of the requested tenant. Pick a different account in the browser.".to_string(),
        }
    } else {
        let trimmed = stderr.trim();
        if trimmed.is_empty() {
            "Azure sign-in failed.".to_string()
        } else {
            trimmed.to_string()
        }
    };

    AzureCliStatus {
        id: "azure".to_string(),
        state: AzureCliState::Error,
        detail,
        account: None,
    }
}

/// Interactive Azure CLI sign-in. Runs on a blocking thread because `az login`
/// waits for the user to finish a browser flow, which must not block the UI.
pub async fn sign_in_azure_cli(
    tenant_id: Option<String>,
    subscription_id: Option<String>,
) -> AzureCliStatus {
    tokio::task::spawn_blocking(move || sign_in_azure_cli_blocking(tenant_id, subscription_id))
        .await
        .unwrap_or_else(|error| AzureCliStatus {
            id: "azure".to_string(),
            state: AzureCliState::Error,
            detail: format!("Azure sign-in task failed to run: {error}"),
            account: None,
        })
}

fn sign_in_azure_cli_blocking(
    tenant_id: Option<String>,
    subscription_id: Option<String>,
) -> AzureCliStatus {
    let tenant_id = normalized_optional(tenant_id);
    let subscription_id = normalized_optional(subscription_id);

    // Best effort: clear cached CLI accounts so tenant/account switching shows
    // the browser account picker instead of silently reusing a stale identity.
    let _ = run_az(&["logout", "--only-show-errors"]);

    let mut login_args = vec![
        "login",
        "--only-show-errors",
        "--allow-no-subscriptions",
        "-o",
        "none",
    ];
    if let Some(tenant_id) = &tenant_id {
        login_args.push("--tenant");
        login_args.push(tenant_id.as_str());
    }

    let login_result = run_az(&login_args);
    if login_result.code != 0 {
        return failed_login_status(&login_result.stderr, tenant_id.as_deref());
    }

    if let Some(subscription_id) = &subscription_id {
        let subscription_result =
            run_az(&["account", "set", "--subscription", subscription_id.as_str()]);
        if subscription_result.code != 0 {
            let detail = subscription_result.stderr.trim();
            return AzureCliStatus {
                id: "azure".to_string(),
                state: AzureCliState::Error,
                detail: if detail.is_empty() {
                    format!("Signed in, but could not select subscription {subscription_id}.")
                } else {
                    detail.to_string()
                },
                account: None,
            };
        }
    }

    connected_status(current_account())
}

pub async fn check_azure_cli_sign_in(
    tenant_id: Option<String>,
    subscription_id: Option<String>,
) -> AzureCliStatus {
    match get_cli_access_token(CONFIRM_RESOURCE, tenant_id, subscription_id).await {
        Ok(_) => connected_status(current_account()),
        Err(_) => AzureCliStatus {
            id: "azure".to_string(),
            state: AzureCliState::Unconfigured,
            detail: "Not signed in to Azure. Click sign in and pick an account in the browser."
                .to_string(),
            account: None,
        },
    }
}

/// Acquire a Cognitive Services access token using the Azure CLI session. This
/// calls `az account get-access-token` through `az_command`, so Windows resolves
/// `az.cmd` without flashing console windows from the desktop app.
pub async fn get_azure_cli_access_token(
    tenant_id: Option<String>,
    subscription_id: Option<String>,
) -> Result<AzureCliAccessToken, String> {
    get_cli_access_token(COGNITIVE_SERVICES_RESOURCE, tenant_id, subscription_id)
        .await
        .map_err(|error| {
            format!("Azure CLI could not acquire a Cognitive Services token for the selected tenant/subscription. Use Sign in in the Azure Speech settings, pick an account that belongs to the tenant, then retry: {error}")
        })
}
