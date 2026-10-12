use axum::{
    body::{to_bytes, Body},
    extract::Request,
    middleware::Next,
    response::Response,
};
use serde_json::Value;

/// JSON-RPC notifications execute normally but never emit a JSON-RPC reply.
pub(super) async fn suppress_reply(request: Request, next: Next) -> Response {
    if request.method() != axum::http::Method::POST || request.uri().path() != "/" {
        return next.run(request).await;
    }
    let (parts, body) = request.into_parts();
    let bytes = match to_bytes(body, 2 * 1024 * 1024).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return Response::builder()
                .status(413)
                .body(Body::empty())
                .unwrap_or_default()
        }
    };
    let notification = serde_json::from_slice::<Value>(&bytes)
        .ok()
        .is_some_and(|request| {
            request.is_object()
                && request["jsonrpc"] == "2.0"
                && request["method"].is_string()
                && request.get("id").is_none()
        });
    let response = next
        .run(Request::from_parts(parts, Body::from(bytes)))
        .await;
    if notification && response.status().is_success() {
        Response::builder()
            .status(204)
            .body(Body::empty())
            .unwrap_or_default()
    } else {
        response
    }
}
