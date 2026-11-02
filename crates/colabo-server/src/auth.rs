use std::{collections::HashMap, time::Duration};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use getrandom::fill as random_fill;
use openidconnect::{
    AuthorizationCode, ClientId, ClientSecret, CsrfToken, EmptyAdditionalClaims, EndpointMaybeSet,
    EndpointNotSet, EndpointSet, IssuerUrl, Nonce, OAuth2TokenResponse, PkceCodeChallenge,
    PkceCodeVerifier, RedirectUrl, Scope, TokenResponse,
    core::{CoreAuthenticationFlow, CoreClient, CoreGenderClaim, CoreProviderMetadata},
};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::config::Settings;

type ConfiguredCoreClient = CoreClient<
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointSet,
    EndpointMaybeSet,
>;

struct PendingLogin {
    nonce: String,
    verifier: String,
    return_to: String,
    expires_at: DateTime<Utc>,
}

pub struct OidcIdentity {
    pub issuer: String,
    pub subject: String,
    pub display_name: String,
    pub preferred_username: Option<String>,
    pub picture_url: Option<String>,
    pub return_to: String,
}

pub struct AuthProvider {
    issuer: String,
    client: Option<ConfiguredCoreClient>,
    http: reqwest::Client,
    pending: Mutex<HashMap<String, PendingLogin>>,
}

impl AuthProvider {
    pub async fn new(settings: &Settings) -> Result<Self, String> {
        let has_id = !settings.client_id.trim().is_empty();
        let has_secret = !settings.client_secret.trim().is_empty();
        if has_id != has_secret {
            return Err("OIDC_CLIENT_ID and OIDC_CLIENT_SECRET must be configured together".into());
        }
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|error| format!("could not create OIDC HTTP client: {error}"))?;
        let client = if has_id {
            let issuer = IssuerUrl::new(settings.issuer.clone())
                .map_err(|error| format!("invalid OIDC_ISSUER: {error}"))?;
            let metadata = CoreProviderMetadata::discover_async(issuer, &http)
                .await
                .map_err(|error| format!("OIDC discovery failed: {error}"))?;
            let token_url = metadata
                .token_endpoint()
                .cloned()
                .ok_or_else(|| "OIDC Provider does not advertise a token endpoint".to_owned())?;
            Some(
                CoreClient::from_provider_metadata(
                    metadata,
                    ClientId::new(settings.client_id.clone()),
                    Some(ClientSecret::new(settings.client_secret.clone())),
                )
                .set_token_uri(token_url)
                .set_redirect_uri(
                    RedirectUrl::new(settings.redirect_uri.clone())
                        .map_err(|error| format!("invalid OIDC_REDIRECT_URI: {error}"))?,
                )
                .set_auth_type(openidconnect::AuthType::BasicAuth),
            )
        } else {
            None
        };
        Ok(Self {
            issuer: settings.issuer.clone(),
            client,
            http,
            pending: Mutex::new(HashMap::new()),
        })
    }

    pub fn configured(&self) -> bool {
        self.client.is_some()
    }

    pub async fn begin_login(&self, return_to: Option<&str>) -> Result<(String, String), String> {
        let client = self
            .client
            .as_ref()
            .ok_or_else(|| "OIDC login is not configured".to_owned())?;
        let state = random_secret(32)?;
        let nonce = random_secret(32)?;
        let verifier = random_secret(32)?;
        let pkce_verifier = PkceCodeVerifier::new(verifier.clone());
        let challenge = PkceCodeChallenge::from_code_verifier_sha256(&pkce_verifier);
        let csrf = CsrfToken::new(state.clone());
        let nonce_value = Nonce::new(nonce.clone());
        let (url, _, _) = client
            .authorize_url(
                CoreAuthenticationFlow::AuthorizationCode,
                move || csrf,
                move || nonce_value,
            )
            .add_scope(Scope::new("profile".into()))
            .set_pkce_challenge(challenge)
            .url();
        let pending = PendingLogin {
            nonce,
            verifier,
            return_to: safe_return_to(return_to.unwrap_or("/")),
            expires_at: Utc::now() + chrono::Duration::minutes(10),
        };
        let mut flows = self.pending.lock().await;
        flows.retain(|_, flow| flow.expires_at > Utc::now());
        if flows.len() >= 4096 {
            return Err("too many pending OIDC logins".into());
        }
        flows.insert(state.clone(), pending);
        Ok((url.to_string(), state))
    }

    pub async fn finish_login(
        &self,
        state: &str,
        cookie_state: &str,
        code: &str,
    ) -> Result<OidcIdentity, String> {
        if state.is_empty() || state.len() > 128 || state != cookie_state {
            return Err("OIDC state validation failed".into());
        }
        if code.is_empty() || code.len() > 4096 {
            return Err("OIDC authorization code is invalid".into());
        }
        let pending = self
            .pending
            .lock()
            .await
            .remove(state)
            .ok_or_else(|| "OIDC login state expired or was already used".to_owned())?;
        if pending.expires_at <= Utc::now() {
            return Err("OIDC login state expired".into());
        }
        let client = self
            .client
            .as_ref()
            .ok_or_else(|| "OIDC login is not configured".to_owned())?;
        let tokens = client
            .exchange_code(AuthorizationCode::new(code.to_owned()))
            .set_pkce_verifier(PkceCodeVerifier::new(pending.verifier))
            .request_async(&self.http)
            .await
            .map_err(|error| format!("OIDC token exchange failed: {error}"))?;
        let id_token = tokens
            .id_token()
            .ok_or_else(|| "OIDC response did not include an ID token".to_owned())?;
        let claims = id_token
            .claims(&client.id_token_verifier(), &Nonce::new(pending.nonce))
            .map_err(|error| format!("OIDC ID token validation failed: {error}"))?;
        let issuer = claims.issuer().as_str();
        let subject = claims.subject().as_str();
        if issuer != self.issuer || subject.is_empty() || subject.len() > 512 {
            return Err("OIDC identity claims are invalid".into());
        }
        let claim_value = serde_json::to_value(claims).unwrap_or(Value::Null);
        let userinfo_value = match client.user_info(
            tokens.access_token().clone(),
            Some(claims.subject().clone()),
        ) {
            Ok(request) => request
                .request_async::<EmptyAdditionalClaims, _, CoreGenderClaim>(&self.http)
                .await
                .ok()
                .and_then(|userinfo| serde_json::to_value(userinfo).ok())
                .unwrap_or(Value::Null),
            Err(_) => Value::Null,
        };
        let preferred_username = profile_claim(&userinfo_value, &claim_value, "preferred_username");
        let provider_name = preferred_username
            .clone()
            .or_else(|| profile_claim(&userinfo_value, &claim_value, "name"));
        let display_name = provider_name
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| subject.to_owned());
        let picture_url = [&userinfo_value, &claim_value]
            .into_iter()
            .filter_map(|claims| claims.get("picture").and_then(Value::as_str))
            .map(str::trim)
            .find_map(validate_picture_url);
        Ok(OidcIdentity {
            issuer: issuer.to_owned(),
            subject: subject.to_owned(),
            display_name,
            preferred_username,
            picture_url,
            return_to: pending.return_to,
        })
    }
}

fn profile_claim(userinfo: &Value, id_token: &Value, name: &str) -> Option<String> {
    [userinfo, id_token]
        .into_iter()
        .filter_map(|claims| claims.get(name).and_then(Value::as_str))
        .map(str::trim)
        .find(|value| !value.is_empty())
        .map(str::to_owned)
}

fn validate_picture_url(value: &str) -> Option<String> {
    if value.len() > 2048 {
        return None;
    }
    let url = url::Url::parse(value).ok()?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    Some(url.to_string())
}

pub fn random_secret(bytes: usize) -> Result<String, String> {
    let mut buffer = vec![0; bytes];
    random_fill(&mut buffer)
        .map_err(|error| format!("secure random generation failed: {error}"))?;
    Ok(URL_SAFE_NO_PAD.encode(buffer))
}

fn safe_return_to(value: &str) -> String {
    if value.starts_with('/')
        && !value.starts_with("//")
        && value.len() <= 1024
        && !value.chars().any(char::is_control)
    {
        value.to_owned()
    } else {
        "/".into()
    }
}
