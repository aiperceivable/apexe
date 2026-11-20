//! Reuse the established transport credential policy for A2A.
use std::collections::HashMap;
use std::sync::Arc;

use apcore::Identity;
use apcore_a2a::Authenticator;
use async_trait::async_trait;
use serde_json::{json, Value};

use crate::auth::ResolvedAuth;

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

pub(super) fn announce_generated_token(auth: &ResolvedAuth) {
    if let ResolvedAuth::Token {
        token,
        generated: true,
        ..
    } = auth
    {
        // Startup credentials must remain visible independently of tracing filters.
        use std::io::Write;
        let _ = writeln!(std::io::stderr().lock(), "A2A bearer token: {token}");
    }
}

pub(super) fn adapt(auth: &ResolvedAuth) -> Option<Arc<dyn Authenticator>> {
    auth.authenticator().map(|inner| {
        Arc::new(CredentialAdapter {
            inner,
            jwt: matches!(auth, ResolvedAuth::Jwt { .. }),
        }) as Arc<dyn Authenticator>
    })
}
