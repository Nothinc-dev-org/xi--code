//! Flujo OAuth 2.0 + PKCE para "conectar cuenta ChatGPT" replicando el
//! plugin `codex` de OpenCode (`packages/opencode/src/plugin/codex.ts`).
//!
//! Constantes calcadas: el `CLIENT_ID` de Codex CLI es público y reutilizable
//! por clientes terceros que sigan el mismo contrato OAuth. Si OpenAI rota el
//! `CLIENT_ID` o cambia los scopes, este módulo deja de funcionar y hay que
//! re-alinear con OpenCode.

use std::time::Duration;

use serde::Deserialize;

use crate::oauth::pkce::{base64_url_decode, random_state, Pkce};
use crate::oauth::server::{CallbackResult, LocalCallbackServer};
use crate::{AuthInfo, Error, Result};

pub const ISSUER: &str = "https://auth.openai.com";
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const CODEX_API_ENDPOINT: &str = "https://chatgpt.com/backend-api/codex/responses";
/// Puerto fijo: el `CLIENT_ID` de Codex tiene `http://localhost:1455/auth/callback`
/// registrado como redirect URI permitido. Cambiarlo rompe el flujo.
pub const OAUTH_PORT: u16 = 1455;
const SCOPES: &str = "openid profile email offline_access";
const AWAIT_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Construye la URL de `/oauth/authorize` con todos los parámetros que
/// `auth.openai.com` espera para el flujo simplificado de Codex CLI.
pub fn build_authorize_url(pkce: &Pkce, state: &str, redirect_uri: &str) -> String {
    let q = [
        ("response_type", "code"),
        ("client_id", CLIENT_ID),
        ("redirect_uri", redirect_uri),
        ("scope", SCOPES),
        ("code_challenge", pkce.challenge.as_str()),
        ("code_challenge_method", "S256"),
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
        ("state", state),
        ("originator", "xie-code"),
    ];
    let mut url = format!("{ISSUER}/oauth/authorize?");
    for (i, (k, v)) in q.iter().enumerate() {
        if i > 0 {
            url.push('&');
        }
        url.push_str(&urlencode(k));
        url.push('=');
        url.push_str(&urlencode(v));
    }
    url
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    id_token: Option<String>,
    access_token: String,
    refresh_token: String,
    #[serde(default)]
    expires_in: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct IdTokenClaims {
    chatgpt_account_id: Option<String>,
    organizations: Option<Vec<Org>>,
    #[serde(rename = "https://api.openai.com/auth")]
    openai_auth: Option<OpenaiAuthClaim>,
}

#[derive(Debug, Deserialize)]
struct Org {
    id: String,
}

#[derive(Debug, Deserialize)]
struct OpenaiAuthClaim {
    chatgpt_account_id: Option<String>,
}

/// Extrae el payload (parte central) de un JWT. No verifica firma — solo
/// parsea claims para extraer `chatgpt_account_id`. La verificación real la
/// hace el endpoint Codex al recibir el `Bearer`.
fn parse_jwt_claims(token: &str) -> Option<IdTokenClaims> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let bytes = base64_url_decode(parts[1]).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn extract_account_id(tokens: &TokenResponse) -> Option<String> {
    let candidates = [tokens.id_token.as_deref(), Some(&tokens.access_token[..])];
    for token in candidates.into_iter().flatten() {
        let Some(claims) = parse_jwt_claims(token) else {
            continue;
        };
        if let Some(id) = claims.chatgpt_account_id {
            return Some(id);
        }
        if let Some(id) = claims.openai_auth.and_then(|a| a.chatgpt_account_id) {
            return Some(id);
        }
        if let Some(id) = claims
            .organizations
            .and_then(|o| o.into_iter().next().map(|x| x.id))
        {
            return Some(id);
        }
    }
    None
}

/// Cambia el `authorization_code` recibido en el callback por tokens vía
/// `POST /oauth/token`. Falla con `Error::Oauth` si el endpoint rechaza.
async fn exchange_code_for_tokens(
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<TokenResponse> {
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{ISSUER}/oauth/token"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(form_urlencoded(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", CLIENT_ID),
            ("code_verifier", verifier),
        ]))
        .send()
        .await?;
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        return Err(Error::Oauth(format!("token exchange {status}: {body}")));
    }
    serde_json::from_str(&body).map_err(Error::from)
}

/// Refresca el `access_token` cuando expira. El `refresh_token` se conserva
/// (algunos servidores rotan, en OpenAI hoy lo devuelven igual).
pub async fn refresh_access_token(refresh_token: &str) -> Result<AuthInfo> {
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{ISSUER}/oauth/token"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(form_urlencoded(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", CLIENT_ID),
        ]))
        .send()
        .await?;
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        return Err(Error::Oauth(format!("token refresh {status}: {body}")));
    }
    let tokens: TokenResponse = serde_json::from_str(&body)?;
    Ok(build_auth_info(tokens))
}

fn build_auth_info(tokens: TokenResponse) -> AuthInfo {
    let account_id = extract_account_id(&tokens);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let expires_at_ms = now + tokens.expires_in.unwrap_or(3600) * 1000;
    AuthInfo::Oauth {
        refresh: tokens.refresh_token,
        access: tokens.access_token,
        expires_at_ms,
        account_id,
    }
}

/// Tipo público para arrancar el flujo browser: el caller llama
/// [`OpenAiOauth::start_browser_flow`], recibe la URL para abrir en el
/// navegador y un futuro que resuelve cuando el callback llega.
pub struct OpenAiOauth;

/// Re-exportado con un alias más expresivo: la cuenta que el usuario conecta
/// es de **ChatGPT**, no la API key clásica de OpenAI.
pub use OpenAiOauth as ChatGptOauth;

/// Handle del flujo en marcha. Contiene la URL que el caller debe abrir en
/// el navegador y el futuro que espera el callback.
pub struct OpenAiBrowserFlow {
    pub authorize_url: String,
    pub redirect_uri: String,
    pkce: Pkce,
    state: String,
    server: crate::oauth::server::BoundServer,
}

impl OpenAiBrowserFlow {
    /// Espera al callback (con timeout de 5 minutos), valida `state`,
    /// intercambia el `code` por tokens y devuelve la `AuthInfo` lista para
    /// guardar en el [`AuthStore`](crate::AuthStore).
    pub async fn await_completion(self) -> Result<AuthInfo> {
        let Self {
            pkce,
            state,
            server,
            redirect_uri,
            ..
        } = self;
        let result: CallbackResult = server.await_callback(AWAIT_TIMEOUT).await?;
        if let Some(err) = result.error {
            return Err(Error::Oauth(format!(
                "{err}: {}",
                result.error_description.unwrap_or_default()
            )));
        }
        let Some(code) = result.code else {
            return Err(Error::Oauth("el callback no incluyó `code`".into()));
        };
        if result.state.as_deref() != Some(state.as_str()) {
            return Err(Error::Oauth(
                "el parámetro `state` no coincide (posible CSRF)".into(),
            ));
        }
        let tokens = exchange_code_for_tokens(&code, &redirect_uri, &pkce.verifier).await?;
        Ok(build_auth_info(tokens))
    }
}

impl OpenAiOauth {
    /// Levanta el servidor local, genera PKCE+state y construye la URL de
    /// autorización. El caller debe abrir `authorize_url` en el navegador y
    /// luego llamar [`OpenAiBrowserFlow::await_completion`].
    pub async fn start_browser_flow() -> Result<OpenAiBrowserFlow> {
        let server = LocalCallbackServer::bind(OAUTH_PORT).await?;
        let pkce = Pkce::generate();
        let state = random_state(32);
        let authorize_url = build_authorize_url(&pkce, &state, server.redirect_uri());
        let redirect_uri = server.redirect_uri().to_string();
        Ok(OpenAiBrowserFlow {
            authorize_url,
            redirect_uri,
            pkce,
            state,
            server,
        })
    }
}

/// `application/x-www-form-urlencoded` para los POST a `/oauth/token`.
fn form_urlencoded(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .enumerate()
        .map(|(i, (k, v))| {
            let prefix = if i == 0 { "" } else { "&" };
            format!("{prefix}{}={}", urlencode(k), urlencode(v))
        })
        .collect()
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorize_url_has_required_params() {
        let pkce = Pkce::generate();
        let state = "test-state";
        let url = build_authorize_url(&pkce, state, "http://127.0.0.1:1455/auth/callback");
        assert!(url.starts_with(&format!("{ISSUER}/oauth/authorize?")));
        assert!(url.contains("response_type=code"));
        assert!(url.contains(&format!("client_id={CLIENT_ID}")));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains(&format!("code_challenge={}", pkce.challenge)));
        assert!(url.contains(&format!("state={state}")));
        assert!(url.contains("originator=xie-code"));
    }

    #[test]
    fn parses_jwt_claims_with_account_id() {
        // JWT manual: header.payload.signature, payload con chatgpt_account_id.
        let payload = serde_json::json!({
            "chatgpt_account_id": "acct_abc",
        });
        let payload_b64 = crate::oauth::pkce::base64_url_encode(
            serde_json::to_string(&payload).unwrap().as_bytes(),
        );
        let jwt = format!("aaa.{payload_b64}.bbb");
        let claims = parse_jwt_claims(&jwt).expect("claims");
        assert_eq!(claims.chatgpt_account_id.as_deref(), Some("acct_abc"));
    }

    #[test]
    fn urlencode_keeps_unreserved() {
        assert_eq!(urlencode("hello world"), "hello%20world");
        assert_eq!(urlencode("a/b?c"), "a%2Fb%3Fc");
        assert_eq!(urlencode("foo-bar.baz_qux~"), "foo-bar.baz_qux~");
    }
}
