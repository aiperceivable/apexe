//! Transport authentication for the HTTP-family MCP transports.
//!
//! apexe wraps arbitrary local binaries. On a non-loopback bind that makes an
//! unauthenticated server not "an API without auth" but a remote-execution
//! entry point for every executable on the host, so the default here is *on*
//! rather than off, and the dangerous configuration has to name itself.
//!
//! The three transports have genuinely different trust boundaries, so they get
//! different defaults — see [`resolve_auth`]:
//!
//! | transport | default |
//! |---|---|
//! | stdio | no auth — the boundary is the parent/child process relationship, and whatever can spawn apexe already holds the user's privileges |
//! | HTTP/SSE on loopback | bearer token, generated and written to stderr at startup, so a local dev server needs no secret management |
//! | HTTP/SSE elsewhere | authentication required; `--auth none` refuses to start without a separate acknowledgement |
//!
//! A bearer token is the primary mechanism because the dominant deployment is
//! one desktop MCP client talking to one local server, where JWT's issuer, key
//! management, expiry and rotation buy nothing. JWT stays available as a mode
//! for multi-user deployments, where an identity-bearing credential is what
//! makes the ACL's `callers` field and the audit log's caller dimension
//! meaningful.

use std::collections::HashMap;
use std::sync::Arc;

use apcore::{ErrorCode, ModuleError};
use apcore_mcp::{Authenticator, Identity, JWTAuthenticator};
use async_trait::async_trait;

/// The `type` recorded on an [`Identity`] minted from a static bearer token.
const TOKEN_IDENTITY_TYPE: &str = "token";

/// The caller id every holder of the shared bearer token authenticates as.
///
/// One token means one principal; there is nothing to distinguish two holders
/// of the same secret. Naming it explicitly (rather than leaving the identity
/// anonymous) is what lets `caller_id` reach the audit log at all.
const TOKEN_IDENTITY_ID: &str = "apexe-token";

/// Which credential the HTTP transports accept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    /// A shared bearer token in `Authorization: Bearer <token>`.
    Token,
    /// A signed JWT in `Authorization: Bearer <jwt>`.
    Jwt,
    /// No credential is checked. Explicit opt-out.
    None,
}

impl AuthMode {
    /// Parse the `--auth` value. Returns `None` for an unrecognized mode so
    /// the caller can build the error with its own context.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "token" => Some(Self::Token),
            "jwt" => Some(Self::Jwt),
            "none" => Some(Self::None),
            _ => None,
        }
    }

    /// The value as it is spelled on the command line.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Token => "token",
            Self::Jwt => "jwt",
            Self::None => "none",
        }
    }
}

/// What the operator asked for on the command line, before it is reconciled
/// with the transport and bind address.
#[derive(Debug, Clone, Default)]
pub struct AuthOptions {
    /// Explicit `--auth <mode>`. `None` means "use the per-transport default".
    pub mode: Option<AuthMode>,
    /// Explicit `--auth-token`, or `APEXE_AUTH_TOKEN`.
    pub token: Option<String>,
    /// Explicit `--jwt-secret`, or `APEXE_JWT_SECRET`.
    pub jwt_secret: Option<String>,
    /// The separate acknowledgement `--auth none` needs on a non-loopback
    /// bind. A `--disable-*` flag gets copied out of a tutorial once and then
    /// lives in everyone's startup script forever; requiring a second flag
    /// makes the dangerous configuration state its own name.
    pub allow_unauthenticated_bind: bool,
}

/// The authentication actually installed on the server.
pub enum ResolvedAuth {
    /// No authenticator is applied — stdio, or an acknowledged `--auth none`.
    Disabled,
    /// A bearer token is required.
    Token {
        authenticator: Arc<dyn Authenticator>,
        /// The token itself, so the caller can print a generated one.
        token: String,
        /// `true` when apexe minted the token rather than being given one.
        generated: bool,
    },
    /// A JWT is required.
    Jwt {
        authenticator: Arc<dyn Authenticator>,
    },
}

impl std::fmt::Debug for ResolvedAuth {
    /// Renders the mode only. The token is a live credential and this type
    /// ends up in server-assembly diagnostics.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disabled => f.write_str("ResolvedAuth::Disabled"),
            Self::Token { generated, .. } => f
                .debug_struct("ResolvedAuth::Token")
                .field("token", &"<redacted>")
                .field("generated", generated)
                .finish(),
            Self::Jwt { .. } => f.write_str("ResolvedAuth::Jwt"),
        }
    }
}

impl ResolvedAuth {
    /// The authenticator to hand to apcore-mcp, if any.
    pub fn authenticator(&self) -> Option<Arc<dyn Authenticator>> {
        match self {
            Self::Disabled => None,
            Self::Token { authenticator, .. } | Self::Jwt { authenticator } => {
                Some(authenticator.clone())
            }
        }
    }

    /// Whether unauthenticated requests must be rejected.
    pub fn require_auth(&self) -> bool {
        !matches!(self, Self::Disabled)
    }
}

/// Whether `host` binds only to the local machine.
///
/// Anything else is reachable from the network and gets the strict default.
/// An unparseable host is treated as non-loopback: guessing wrong in that
/// direction only costs a flag, guessing wrong the other way costs the host.
pub fn is_loopback_host(host: &str) -> bool {
    if host == "localhost" {
        return true;
    }
    let trimmed = host.trim_start_matches('[').trim_end_matches(']');
    trimmed
        .parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
}

/// Reconcile the requested auth options with the transport and bind address.
///
/// Returns `Err` for the two configurations that must not start: an
/// unacknowledged `--auth none` on a non-loopback bind, and `--auth jwt`
/// without a secret.
#[allow(clippy::result_large_err)] // ModuleError is the crate-wide domain error
pub fn resolve_auth(
    transport: &str,
    host: &str,
    opts: &AuthOptions,
) -> Result<ResolvedAuth, ModuleError> {
    if transport == "stdio" {
        if opts.mode.is_some_and(|mode| mode != AuthMode::None) || opts.token.is_some() {
            tracing::warn!(
                "Ignoring --auth on the stdio transport: the trust boundary is the \
                 parent/child process relationship, and a token adds nothing to it"
            );
        }
        return Ok(ResolvedAuth::Disabled);
    }

    let loopback = is_loopback_host(host);
    match opts.mode.unwrap_or(AuthMode::Token) {
        AuthMode::None => resolve_none(host, loopback, opts.allow_unauthenticated_bind),
        AuthMode::Token => Ok(resolve_token(opts.token.clone())),
        AuthMode::Jwt => resolve_jwt(opts.jwt_secret.as_deref()),
    }
}

/// `--auth none`: allowed on loopback with a warning, refused elsewhere unless
/// separately acknowledged.
#[allow(clippy::result_large_err)] // ModuleError is the crate-wide domain error
fn resolve_none(
    host: &str,
    loopback: bool,
    acknowledged: bool,
) -> Result<ResolvedAuth, ModuleError> {
    if !loopback && !acknowledged {
        return Err(ModuleError::new(
            ErrorCode::GeneralInvalidInput,
            format!(
                "Refusing to start: `--auth none` on the non-loopback bind '{host}' would expose \
                 every wrapped binary on this host to the network with no credential. apexe wraps \
                 arbitrary local commands, so this is a remote-execution entry point rather than \
                 an unauthenticated API. Bind to 127.0.0.1, use `--auth token`, or pass \
                 `--allow-unauthenticated-bind` to state that you mean it."
            ),
        ));
    }
    if !loopback {
        tracing::warn!(
            host,
            "Serving with NO authentication on a non-loopback bind, as explicitly acknowledged"
        );
    } else {
        tracing::warn!("Authentication disabled on a loopback bind (--auth none)");
    }
    Ok(ResolvedAuth::Disabled)
}

/// `--auth token`: use the supplied token, or mint one.
fn resolve_token(supplied: Option<String>) -> ResolvedAuth {
    let (token, generated) = match supplied.filter(|t| !t.is_empty()) {
        Some(token) => (token, false),
        None => (generate_token(), true),
    };
    ResolvedAuth::Token {
        authenticator: Arc::new(StaticTokenAuthenticator::new(token.clone())),
        token,
        generated,
    }
}

/// `--auth jwt`: a secret is mandatory, since there is nothing sensible to
/// generate.
#[allow(clippy::result_large_err)] // ModuleError is the crate-wide domain error
fn resolve_jwt(secret: Option<&str>) -> Result<ResolvedAuth, ModuleError> {
    let secret = secret.filter(|s| !s.is_empty()).ok_or_else(|| {
        ModuleError::new(
            ErrorCode::GeneralInvalidInput,
            "`--auth jwt` needs a signing secret: pass `--jwt-secret <value>` or set \
             APEXE_JWT_SECRET."
                .to_string(),
        )
    })?;
    let authenticator = JWTAuthenticator::new(secret, None, None, None, None, None, Some(true));
    Ok(ResolvedAuth::Jwt {
        authenticator: Arc::new(authenticator),
    })
}

/// Mint a bearer token.
///
/// Two v4 UUIDs' worth of hex — 244 bits from the OS CSPRNG, well past what a
/// bearer token needs, and no new dependency beyond the `uuid` crate already
/// present in the dependency graph.
fn generate_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// Compare two byte strings without an early return on the first difference.
///
/// A `==` on the token would leak its prefix through response timing. The
/// length is not secret (it is fixed by the format), so comparing lengths
/// first is fine.
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (l, r) in left.iter().zip(right.iter()) {
        diff |= l ^ r;
    }
    diff == 0
}

/// Accepts one shared bearer token in the `Authorization` header.
///
/// The header map handed to an [`Authenticator`] has lowercased names, so only
/// `authorization` is looked up.
pub struct StaticTokenAuthenticator {
    token: String,
}

impl StaticTokenAuthenticator {
    /// Create an authenticator for `token`.
    pub fn new(token: String) -> Self {
        Self { token }
    }
}

impl std::fmt::Debug for StaticTokenAuthenticator {
    /// Never renders the token: this type ends up inside server config structs
    /// that get logged.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StaticTokenAuthenticator")
            .field("token", &"<redacted>")
            .finish()
    }
}

#[async_trait]
impl Authenticator for StaticTokenAuthenticator {
    async fn authenticate(&self, headers: &HashMap<String, String>) -> Option<Identity> {
        let presented = headers
            .get("authorization")?
            .strip_prefix("Bearer ")
            .or_else(|| headers.get("authorization")?.strip_prefix("bearer "))?
            .trim();
        if !constant_time_eq(presented.as_bytes(), self.token.as_bytes()) {
            return None;
        }
        Some(Identity::new(
            TOKEN_IDENTITY_ID.to_string(),
            TOKEN_IDENTITY_TYPE.to_string(),
            vec![],
            HashMap::new(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bearer(value: &str) -> HashMap<String, String> {
        HashMap::from([("authorization".to_string(), format!("Bearer {value}"))])
    }

    #[test]
    fn test_is_loopback_host_recognizes_local_binds() {
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("127.0.0.53"));
        assert!(is_loopback_host("::1"));
        assert!(is_loopback_host("[::1]"));
        assert!(is_loopback_host("localhost"));
    }

    #[test]
    fn test_is_loopback_host_treats_unknown_as_remote() {
        assert!(!is_loopback_host("0.0.0.0"));
        assert!(!is_loopback_host("192.168.1.10"));
        // Unparseable must fail safe towards "reachable from the network".
        assert!(!is_loopback_host("example.com"));
    }

    #[test]
    fn test_resolve_auth_stdio_is_disabled() {
        let resolved = resolve_auth("stdio", "127.0.0.1", &AuthOptions::default()).unwrap();
        assert!(!resolved.require_auth());
        assert!(resolved.authenticator().is_none());
    }

    #[test]
    fn test_resolve_auth_http_defaults_to_generated_token() {
        let resolved = resolve_auth("http", "127.0.0.1", &AuthOptions::default()).unwrap();
        assert!(resolved.require_auth());
        match resolved {
            ResolvedAuth::Token {
                token, generated, ..
            } => {
                assert!(generated);
                assert_eq!(token.len(), 64);
            }
            _ => panic!("expected a generated token"),
        }
    }

    #[test]
    fn test_resolve_auth_uses_supplied_token() {
        let opts = AuthOptions {
            token: Some("supplied-secret".to_string()),
            ..AuthOptions::default()
        };
        match resolve_auth("http", "0.0.0.0", &opts).unwrap() {
            ResolvedAuth::Token {
                token, generated, ..
            } => {
                assert!(!generated);
                assert_eq!(token, "supplied-secret");
            }
            _ => panic!("expected the supplied token"),
        }
    }

    #[test]
    fn test_resolve_auth_refuses_none_on_non_loopback_bind() {
        let opts = AuthOptions {
            mode: Some(AuthMode::None),
            ..AuthOptions::default()
        };
        let err = resolve_auth("http", "0.0.0.0", &opts)
            .expect_err("--auth none on a public bind must refuse to start");
        assert_eq!(err.code, ErrorCode::GeneralInvalidInput);
        assert!(err.message.contains("--allow-unauthenticated-bind"));
    }

    #[test]
    fn test_resolve_auth_allows_acknowledged_none_on_non_loopback_bind() {
        let opts = AuthOptions {
            mode: Some(AuthMode::None),
            allow_unauthenticated_bind: true,
            ..AuthOptions::default()
        };
        let resolved = resolve_auth("http", "0.0.0.0", &opts).unwrap();
        assert!(!resolved.require_auth());
    }

    #[test]
    fn test_resolve_auth_allows_none_on_loopback_without_acknowledgement() {
        let opts = AuthOptions {
            mode: Some(AuthMode::None),
            ..AuthOptions::default()
        };
        assert!(resolve_auth("http", "127.0.0.1", &opts).is_ok());
    }

    #[test]
    fn test_resolve_auth_jwt_requires_secret() {
        let opts = AuthOptions {
            mode: Some(AuthMode::Jwt),
            ..AuthOptions::default()
        };
        let err = resolve_auth("http", "127.0.0.1", &opts)
            .expect_err("--auth jwt without a secret must refuse to start");
        assert!(err.message.contains("--jwt-secret"));
    }

    #[test]
    fn test_auth_mode_parse_roundtrip() {
        for mode in [AuthMode::Token, AuthMode::Jwt, AuthMode::None] {
            assert_eq!(AuthMode::parse(mode.as_str()), Some(mode));
        }
        assert_eq!(AuthMode::parse("basic"), None);
    }

    #[tokio::test]
    async fn test_static_token_authenticator_accepts_matching_token() {
        let auth = StaticTokenAuthenticator::new("s3cret".to_string());
        let identity = auth
            .authenticate(&bearer("s3cret"))
            .await
            .expect("matching token authenticates");
        assert_eq!(identity.id(), TOKEN_IDENTITY_ID);
    }

    #[tokio::test]
    async fn test_static_token_authenticator_rejects_wrong_or_missing_token() {
        let auth = StaticTokenAuthenticator::new("s3cret".to_string());
        assert!(auth.authenticate(&bearer("wrong")).await.is_none());
        // A prefix must not pass: the comparison is over the whole value.
        assert!(auth.authenticate(&bearer("s3cre")).await.is_none());
        assert!(auth.authenticate(&HashMap::new()).await.is_none());
        // A bare token with no scheme is not a Bearer credential.
        assert!(auth
            .authenticate(&HashMap::from([(
                "authorization".to_string(),
                "s3cret".to_string()
            )]))
            .await
            .is_none());
    }

    #[test]
    fn test_constant_time_eq_matches_equality() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }

    #[test]
    fn test_generate_token_is_unique_and_long() {
        let first = generate_token();
        let second = generate_token();
        assert_ne!(first, second);
        assert_eq!(first.len(), 64);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_static_token_authenticator_debug_hides_token() {
        let auth = StaticTokenAuthenticator::new("s3cret".to_string());
        let rendered = format!("{auth:?}");
        assert!(!rendered.contains("s3cret"), "token leaked: {rendered}");
    }
}
