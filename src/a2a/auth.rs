//! Reuse the established transport credential policy for A2A.
use std::collections::HashMap;
use std::sync::Arc;

use apcore::{Identity, ModuleError};
use apcore_a2a::Authenticator;
use async_trait::async_trait;
use serde_json::{json, Value};

struct CredentialAdapter {
    inner: Arc<dyn apcore_mcp::Authenticator>,
    jwt: bool,
}

#[async_trait]
impl Authenticator for CredentialAdapter {
    async fn authenticate(&self, headers: &HashMap<String, String>) -> Option<Identity> {
        self.inner.authenticate(headers).await
    }

    fn security_schemes(&self) -> Option<Value> {
        Some(json!({"bearerAuth": {"type":"http", "scheme":"bearer",
            "bearerFormat": if self.jwt { "JWT" } else { "opaque" }}}))
    }
}

#[allow(clippy::result_large_err)] // Preserve the shared ModuleError boundary.
pub(crate) fn resolve(
    host: &str,
    options: &crate::auth::AuthOptions,
) -> Result<Option<Arc<dyn Authenticator>>, ModuleError> {
    let resolved = crate::auth::resolve_auth("http", host, options)?;
    if let crate::auth::ResolvedAuth::Token {
        token,
        generated: true,
        ..
    } = &resolved
    {
        // Startup credentials must remain visible independently of tracing filters.
        use std::io::Write;
        let _ = writeln!(std::io::stderr().lock(), "A2A bearer token: {token}");
    }
    Ok(resolved.authenticator().map(|inner| {
        Arc::new(CredentialAdapter {
            inner,
            jwt: matches!(resolved, crate::auth::ResolvedAuth::Jwt { .. }),
        }) as Arc<dyn Authenticator>
    }))
}
