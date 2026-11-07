//! Microsoft Entra sign-in: gets an access token for Azure SQL (`https://database.windows.net/`)
//! by user name and password, from the Azure CLI's signed-in account, or by the device-code flow
//! (MFA in a browser). The driver only takes a ready token, so this is done before connecting.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// The resource and scope of Azure SQL tokens.
const RESOURCE: &str = "https://database.windows.net/";
const SCOPE: &str = "https://database.windows.net//.default";
/// The public client id Microsoft's SQL Server drivers sign in with.
const CLIENT_ID: &str = "2fd908ad-0664-4344-b9be-cd3e8b574c38";
const AUTHORITY: &str = "login.microsoftonline.com";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Method {
    /// User name (UPN) and password.
    Password,
    /// The account signed in on this machine (through the Azure CLI).
    Integrated,
    /// Device-code sign-in in a browser; works with MFA.
    Interactive,
}

#[derive(Clone, PartialEq, Eq)]
pub struct Login {
    pub method: Method,
    pub user: String,
    pub password: String,
    /// Tenant id or domain; empty for the user's domain or `organizations`.
    pub tenant: String,
}

impl std::fmt::Debug for Login {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Login").field("method", &self.method).field("user", &self.user).field("tenant", &self.tenant).finish_non_exhaustive()
    }
}

impl Login {
    /// The tenant to sign in to.
    pub fn tenant(&self) -> String {
        if !self.tenant.trim().is_empty() {
            return self.tenant.trim().to_string();
        }
        if let Ok(t) = std::env::var("QUILL_ENTRA_TENANT")
            && !t.trim().is_empty()
        {
            return t.trim().to_string();
        }
        match self.user.rsplit_once('@') {
            Some((_, domain)) if !domain.is_empty() => domain.to_string(),
            _ => "organizations".to_string(),
        }
    }
}

fn client_id() -> String {
    std::env::var("QUILL_ENTRA_CLIENT_ID").ok().filter(|c| !c.trim().is_empty()).unwrap_or_else(|| CLIENT_ID.to_string())
}

/// What the device-code flow shows: Entra's message and the code details.
pub type Prompt = Arc<dyn Fn(&str, &Value) + Send + Sync>;

static PROMPT: LazyLock<Mutex<Option<Prompt>>> = LazyLock::new(|| Mutex::new(None));
static CACHE: LazyLock<Mutex<HashMap<(Method, String, String), (String, Instant)>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Where the device-code message goes; stderr when none is set.
pub fn set_prompt(prompt: Option<Prompt>) {
    *PROMPT.lock().unwrap() = prompt;
}

fn show(message: &str, details: &Value) {
    let prompt = PROMPT.lock().unwrap().clone();
    match prompt {
        Some(p) => p(message, details),
        None => eprintln!("{message}"),
    }
}

/// An access token for the login, from the cache while it has 5 minutes left.
pub async fn token(login: &Login) -> Result<String, String> {
    let key = (login.method, login.tenant(), login.user.to_ascii_lowercase());
    if let Some((t, until)) = CACHE.lock().unwrap().get(&key)
        && *until > Instant::now()
    {
        return Ok(t.clone());
    }
    let (token, lifetime) = match login.method {
        Method::Password => password_token(login).await?,
        Method::Integrated => cli_token(login)?,
        Method::Interactive => device_code_token(login).await?,
    };
    let until = Instant::now() + lifetime.saturating_sub(Duration::from_secs(300));
    CACHE.lock().unwrap().insert(key, (token.clone(), until));
    Ok(token)
}

async fn password_token(login: &Login) -> Result<(String, Duration), String> {
    if login.user.is_empty() {
        return Err("Microsoft Entra password sign-in needs a user name".into());
    }
    let form = [
        ("grant_type", "password"),
        ("client_id", &client_id()),
        ("scope", SCOPE),
        ("username", &login.user),
        ("password", &login.password),
    ];
    let reply = post(&format!("/{}/oauth2/v2.0/token", login.tenant()), &form).await?;
    token_of(&reply).map_err(|e| format!("Microsoft Entra sign-in for {} failed: {e}", login.user))
}

async fn device_code_token(login: &Login) -> Result<(String, Duration), String> {
    let tenant = login.tenant();
    let client = client_id();
    let mut form = vec![("client_id", client.as_str()), ("scope", SCOPE)];
    if !login.user.is_empty() {
        form.push(("login_hint", &login.user));
    }
    let code = post(&format!("/{tenant}/oauth2/v2.0/devicecode"), &form).await?;
    if let Some(e) = error_text(&code) {
        return Err(format!("Microsoft Entra sign-in failed: {e}"));
    }
    let device_code = code["device_code"].as_str().ok_or("Microsoft Entra sent no device code")?.to_string();
    let uri = code["verification_uri"].as_str().unwrap_or("https://microsoft.com/devicelogin");
    let user_code = code["user_code"].as_str().unwrap_or_default();
    let expires = code["expires_in"].as_u64().unwrap_or(900);
    let message = code["message"].as_str().map(str::to_string).unwrap_or_else(|| format!("To sign in, use a web browser to open the page {uri} and enter the code {user_code} to authenticate."));
    show(&message, &serde_json::json!({"verificationUri": uri, "userCode": user_code, "expiresIn": expires}));
    let mut interval = code["interval"].as_u64().unwrap_or(5).max(1);
    let deadline = Instant::now() + Duration::from_secs(expires);
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_secs(interval)).await;
        let reply = post(
            &format!("/{tenant}/oauth2/v2.0/token"),
            &[("grant_type", "urn:ietf:params:oauth:grant-type:device_code"), ("client_id", &client), ("device_code", &device_code)],
        )
        .await?;
        match reply["error"].as_str() {
            Some("authorization_pending") => continue,
            Some("slow_down") => interval += 5,
            _ => return token_of(&reply).map_err(|e| format!("Microsoft Entra sign-in failed: {e}")),
        }
    }
    Err("Microsoft Entra sign-in timed out: the device code expired before sign-in finished".into())
}

/// The token of the Azure CLI's signed-in account.
fn cli_token(login: &Login) -> Result<(String, Duration), String> {
    let mut args = vec!["account", "get-access-token", "--resource", RESOURCE, "--output", "json"];
    let tenant = login.tenant();
    if tenant != "organizations" {
        args.extend(["--tenant", tenant.as_str()]);
    }
    // `az` is a batch file on Windows.
    let output = if cfg!(windows) {
        std::process::Command::new("cmd").arg("/C").arg("az").args(&args).output()
    } else {
        std::process::Command::new("az").args(&args).output()
    }
    .map_err(|e| format!("Microsoft Entra integrated sign-in uses the Azure CLI, which could not be started ({e}); install it and run 'az login', or use interactive sign-in"))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!("Microsoft Entra integrated sign-in: the Azure CLI gave no token; run 'az login' or use interactive sign-in. {err}"));
    }
    let v: Value = serde_json::from_slice(&output.stdout).map_err(|e| format!("Azure CLI output: {e}"))?;
    let token = v["accessToken"].as_str().ok_or("the Azure CLI returned no access token")?.to_string();
    // `expires_on` is a Unix time in newer versions; assume 30 minutes otherwise.
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
    let lifetime = v["expires_on"].as_u64().map(|t| t.saturating_sub(now)).unwrap_or(1800);
    Ok((token, Duration::from_secs(lifetime)))
}

fn error_text(reply: &Value) -> Option<String> {
    let error = reply["error"].as_str()?;
    Some(reply["error_description"].as_str().map(|d| d.lines().next().unwrap_or(d).trim().to_string()).unwrap_or_else(|| error.to_string()))
}

fn token_of(reply: &Value) -> Result<(String, Duration), String> {
    if let Some(e) = error_text(reply) {
        return Err(e);
    }
    let token = reply["access_token"].as_str().ok_or("the reply has no access token")?.to_string();
    Ok((token, Duration::from_secs(reply["expires_in"].as_u64().unwrap_or(3600))))
}

/// `application/x-www-form-urlencoded`.
pub fn form_encode(pairs: &[(&str, &str)]) -> String {
    let enc = |s: &str| {
        let mut out = String::new();
        for b in s.bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(b as char),
                b' ' => out.push('+'),
                _ => out.push_str(&format!("%{b:02X}")),
            }
        }
        out
    };
    pairs.iter().map(|(k, v)| format!("{}={}", enc(k), enc(v))).collect::<Vec<_>>().join("&")
}

/// Posts a form to the Entra authority over HTTPS and parses the JSON reply (also on 4xx).
async fn post(path: &str, form: &[(&str, &str)]) -> Result<Value, String> {
    let body = form_encode(form);
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: {AUTHORITY}\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\nAccept: application/json\r\nUser-Agent: QuillSQL\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let raw = tokio::time::timeout(Duration::from_secs(60), exchange(request.as_bytes()))
        .await
        .map_err(|_| format!("{AUTHORITY} did not answer within 60s"))??;
    let body = http_body(&raw)?;
    serde_json::from_slice(&body).map_err(|e| format!("{AUTHORITY} sent a reply that is not JSON: {e}"))
}

async fn exchange(request: &[u8]) -> Result<Vec<u8>, String> {
    let mut roots = tokio_rustls::rustls::RootCertStore::empty();
    roots.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
    let config = tokio_rustls::rustls::ClientConfig::builder().with_root_certificates(roots).with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let tcp = tokio::net::TcpStream::connect((AUTHORITY, 443)).await.map_err(|e| format!("could not reach {AUTHORITY}: {e}"))?;
    let name = tokio_rustls::rustls::pki_types::ServerName::try_from(AUTHORITY).map_err(|e| e.to_string())?;
    let mut tls = connector.connect(name, tcp).await.map_err(|e| format!("TLS with {AUTHORITY}: {e}"))?;
    tls.write_all(request).await.map_err(|e| e.to_string())?;
    let mut raw = Vec::new();
    // Servers may close without a TLS close_notify; keep what arrived.
    if let Err(e) = tls.read_to_end(&mut raw).await
        && raw.is_empty()
    {
        return Err(format!("reading from {AUTHORITY}: {e}"));
    }
    Ok(raw)
}

/// The body of an HTTP/1.1 response, de-chunked.
pub fn http_body(raw: &[u8]) -> Result<Vec<u8>, String> {
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or("incomplete HTTP response")?;
    let head = String::from_utf8_lossy(&raw[..split]).to_ascii_lowercase();
    let body = &raw[split + 4..];
    if !head.lines().any(|l| l.starts_with("transfer-encoding:") && l.contains("chunked")) {
        return Ok(body.to_vec());
    }
    let mut out = Vec::new();
    let mut rest = body;
    loop {
        let eol = rest.windows(2).position(|w| w == b"\r\n").ok_or("bad chunked HTTP body")?;
        let size_text = String::from_utf8_lossy(&rest[..eol]);
        let size = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16).map_err(|_| "bad chunk size in HTTP body")?;
        rest = &rest[eol + 2..];
        if size == 0 || rest.len() < size {
            out.extend_from_slice(&rest[..size.min(rest.len())]);
            return Ok(out);
        }
        out.extend_from_slice(&rest[..size]);
        rest = rest.get(size + 2..).unwrap_or_default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_forms_and_picks_the_tenant() {
        assert_eq!(form_encode(&[("username", "ann@contoso.com"), ("password", "a b&c=d")]), "username=ann%40contoso.com&password=a+b%26c%3Dd");
        let login = |user: &str, tenant: &str| Login { method: Method::Password, user: user.into(), password: String::new(), tenant: tenant.into() };
        if std::env::var("QUILL_ENTRA_TENANT").is_err() {
            assert_eq!(login("ann@contoso.com", "").tenant(), "contoso.com");
            assert_eq!(login("", "").tenant(), "organizations");
        }
        assert_eq!(login("ann@contoso.com", "1111-2222").tenant(), "1111-2222");
        assert!(!format!("{:?}", Login { password: "secret".into(), ..login("a", "") }).contains("secret"));
    }

    #[test]
    fn reads_plain_and_chunked_bodies() {
        assert_eq!(http_body(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}").unwrap(), b"{}");
        let chunked = b"HTTP/1.1 400 Bad Request\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{\"a\"\r\n3\r\n:1}\r\n0\r\n\r\n";
        assert_eq!(http_body(chunked).unwrap(), b"{\"a\":1}");
    }

    #[test]
    fn passes_entra_errors_on() {
        let v = serde_json::json!({"error": "invalid_grant", "error_description": "AADSTS50126: Error validating credentials.\r\nTrace ID: x"});
        assert_eq!(token_of(&v).unwrap_err(), "AADSTS50126: Error validating credentials.");
        let ok = serde_json::json!({"access_token": "t", "expires_in": 3599});
        assert_eq!(token_of(&ok).unwrap(), ("t".to_string(), Duration::from_secs(3599)));
    }

    /// Talks to the real Entra endpoint with a made-up account; runs when QUILL_TEST_ENTRA_NET is set.
    #[test]
    fn real_endpoint_refuses_a_made_up_account() {
        if std::env::var("QUILL_TEST_ENTRA_NET").is_err() {
            return;
        }
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let login = Login { method: Method::Password, user: "nobody@quillsql-test.invalid".into(), password: "x".into(), tenant: "organizations".into() };
        let e = rt.block_on(token(&login)).unwrap_err();
        assert!(e.contains("AADSTS"), "{e}");
    }
}
