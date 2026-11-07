//! Connections to SQL Server for the compare commands. Everything else in the crate is
//! synchronous, so each public call drives its own single-threaded Tokio runtime.

use std::time::Duration;

use futures_util::TryStreamExt;
use tiberius::{AuthMethod, Client, Config, EncryptionLevel, Row, SqlBrowser};
use tokio::net::TcpStream;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

pub type Connection = Client<Compat<TcpStream>>;

/// How to sign in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Auth {
    /// The current Windows user (SSPI). Windows only.
    Windows,
    Sql { user: String, password: String },
    /// A Microsoft Entra access token for `https://database.windows.net/`.
    AccessToken(String),
    /// Microsoft Entra: the token is fetched when connecting (`crate::entra`).
    Entra(crate::entra::Login),
}

/// One side of a comparison: where the database is and how to reach it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionSpec {
    /// As typed by the user: `host`, `host,port`, `host\instance`, `tcp:host,port`, `.` or `(local)`.
    pub server: String,
    pub database: String,
    pub auth: Auth,
    /// Encrypt the whole session. On by default, as in SSMS 20 and later.
    pub encrypt: bool,
    pub trust_server_certificate: bool,
    pub connect_timeout: Duration,
    pub application_name: String,
    /// A full ADO.NET connection string; when set it replaces every other field except the database.
    pub connection_string: Option<String>,
}

impl ConnectionSpec {
    pub fn new(server: &str, database: &str, auth: Auth) -> Self {
        ConnectionSpec {
            server: server.to_string(),
            database: database.to_string(),
            auth,
            encrypt: true,
            trust_server_certificate: false,
            connect_timeout: Duration::from_secs(15),
            application_name: "QuillSQL Compare".to_string(),
            connection_string: None,
        }
    }

    /// `server / database`, for messages and reports.
    pub fn describe(&self) -> String {
        format!("{} / {}", self.server, self.database)
    }
}

/// The parts of a server name: host, explicit port, named instance.
#[derive(Debug, PartialEq, Eq)]
pub struct ServerAddress {
    pub host: String,
    pub port: Option<u16>,
    pub instance: Option<String>,
}

pub fn parse_server(server: &str) -> Result<ServerAddress, String> {
    let mut s = server.trim();
    if let Some(rest) = s.strip_prefix("tcp:") {
        s = rest;
    }
    if s.is_empty() {
        return Err("the server name is empty".to_string());
    }
    let (s, port) = match s.rsplit_once(',') {
        Some((host, port)) => {
            let port = port
                .trim()
                .parse::<u16>()
                .map_err(|_| format!("'{port}' is not a port number in server name '{server}'"))?;
            (host.trim(), Some(port))
        }
        None => (s, None),
    };
    let (host, instance) = match s.split_once('\\') {
        Some((host, instance)) if !instance.is_empty() => (host, Some(instance.to_string())),
        _ => (s, None),
    };
    let host = match host.to_ascii_lowercase().as_str() {
        "." | "(local)" | "(localhost)" | "localhost" => "localhost".to_string(),
        _ => host.to_string(),
    };
    Ok(ServerAddress { host, port, instance })
}

fn config(spec: &ConnectionSpec) -> Result<Config, String> {
    if let Some(cs) = &spec.connection_string {
        let mut config = Config::from_ado_string(cs).map_err(|e| format!("connection string: {e}"))?;
        if !spec.database.is_empty() {
            config.database(&spec.database);
        }
        return Ok(config);
    }
    let address = parse_server(&spec.server)?;
    let mut config = Config::new();
    config.host(&address.host);
    if let Some(port) = address.port {
        config.port(port);
    }
    if let Some(instance) = &address.instance {
        config.instance_name(instance);
    }
    if !spec.database.is_empty() {
        config.database(&spec.database);
    }
    config.application_name(&spec.application_name);
    config.encryption(if spec.encrypt { EncryptionLevel::Required } else { EncryptionLevel::Off });
    if spec.trust_server_certificate {
        config.trust_cert();
    }
    config.handshake_timeout(Some(spec.connect_timeout));
    config.authentication(match &spec.auth {
        Auth::Sql { user, password } => AuthMethod::sql_server(user, password),
        Auth::AccessToken(token) => AuthMethod::aad_token(token),
        Auth::Entra(_) => return Err("the Microsoft Entra token was not fetched".into()),
        #[cfg(windows)]
        Auth::Windows => AuthMethod::Integrated,
        #[cfg(not(windows))]
        Auth::Windows => {
            return Err("Windows authentication is only available on Windows; give a user name and password".into());
        }
    });
    Ok(config)
}

async fn open(mut config: Config, timeout: Duration) -> Result<Connection, String> {
    // Azure SQL may redirect the login to another node; follow it a few times.
    for _ in 0..3 {
        let tcp = tokio::time::timeout(timeout, TcpStream::connect_named(&config))
            .await
            .map_err(|_| format!("could not reach {} within {}s", config.get_addr(), timeout.as_secs()))?
            .map_err(|e| format!("could not reach {}: {e}", config.get_addr()))?;
        tcp.set_nodelay(true).map_err(|e| e.to_string())?;
        match Client::connect(config.clone(), tcp.compat_write()).await {
            Ok(client) => return Ok(client),
            Err(tiberius::error::Error::Routing { host, port }) => {
                config.host(&host);
                config.port(port);
            }
            Err(e) => return Err(format!("could not sign in: {e}")),
        }
    }
    Err("the server redirected the connection too many times".to_string())
}

/// Opens a connection inside an existing runtime.
pub async fn connect_async(spec: &ConnectionSpec) -> Result<Connection, String> {
    if let Auth::Entra(login) = &spec.auth {
        let token = crate::entra::token(login).await?;
        let spec = ConnectionSpec { auth: Auth::AccessToken(token), ..spec.clone() };
        return open(config(&spec)?, spec.connect_timeout).await;
    }
    open(config(spec)?, spec.connect_timeout).await
}

/// A runtime for the blocking wrappers.
pub fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("could not start the network runtime: {e}"))
}

/// Runs a batch and returns every result set, in order.
pub async fn query_sets(conn: &mut Connection, sql: &str) -> Result<Vec<Vec<Row>>, String> {
    conn.simple_query(sql)
        .await
        .map_err(|e| e.to_string())?
        .into_results()
        .await
        .map_err(|e| e.to_string())
}

/// Runs a query and returns its first result set.
pub async fn query(conn: &mut Connection, sql: &str) -> Result<Vec<Row>, String> {
    let mut stream = conn.simple_query(sql).await.map_err(|e| e.to_string())?.into_row_stream();
    let mut rows = Vec::new();
    while let Some(row) = stream.try_next().await.map_err(|e| e.to_string())? {
        rows.push(row);
    }
    Ok(rows)
}

/// Runs statements that return no rows.
pub async fn execute(conn: &mut Connection, sql: &str) -> Result<(), String> {
    conn.simple_query(sql)
        .await
        .map_err(|e| e.to_string())?
        .into_results()
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Quotes a string as an N'...' literal.
pub fn literal(s: &str) -> String {
    format!("N'{}'", s.replace('\'', "''"))
}

/// Quotes an identifier as [name].
pub fn quote(name: &str) -> String {
    format!("[{}]", name.replace(']', "]]"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_server_names() {
        let p = |s| parse_server(s).unwrap();
        assert_eq!(p("db.example.com"), ServerAddress { host: "db.example.com".into(), port: None, instance: None });
        assert_eq!(p("tcp:db,1444"), ServerAddress { host: "db".into(), port: Some(1444), instance: None });
        assert_eq!(p(r"SQL01\DEV"), ServerAddress { host: "SQL01".into(), port: None, instance: Some("DEV".into()) });
        assert_eq!(p("."), ServerAddress { host: "localhost".into(), port: None, instance: None });
        assert_eq!(p("(local),14333").port, Some(14333));
        assert!(parse_server("db,abc").is_err());
        assert!(parse_server(" ").is_err());
    }

    #[test]
    fn quotes() {
        assert_eq!(quote("a]b"), "[a]]b]");
        assert_eq!(literal("O'Neil"), "N'O''Neil'");
    }
}
