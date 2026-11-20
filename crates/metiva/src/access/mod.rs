use crate::AppState;
use axum::{
    Json, Router,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use rand::{RngCore, rngs::OsRng};
use serde_json::json;
use sha2::{Digest, Sha256};

mod auth;
mod consumer;
mod keys;
pub use auth::{initialize, recover, require_admin, valid_session};
pub use consumer::openapi;

pub fn router(state: AppState) -> Router<AppState> {
    auth::router(state.clone())
        .merge(keys::router(state.clone()))
        .merge(consumer::router(state))
        .layer(axum::middleware::from_fn(json_rejections))
}

// Axum's extractor rejections normally return plain text. Keep the dashboard and
// consumer error contract consistent without echoing rejected credentials or bodies.
pub(crate) async fn json_rejections(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let head = req.method() == axum::http::Method::HEAD;
    let mut response = next.run(req).await;
    let status = response.status();
    if !status.is_client_error()
        || response
            .headers()
            .get(header::CONTENT_TYPE)
            .is_some_and(|v| v.as_bytes().starts_with(b"application/json"))
    {
        return response;
    }
    let message = match status {
        StatusCode::BAD_REQUEST => "Invalid request parameters",
        StatusCode::UNPROCESSABLE_ENTITY => "Invalid request body",
        StatusCode::UNSUPPORTED_MEDIA_TYPE => "Send a JSON request body",
        StatusCode::PAYLOAD_TOO_LARGE => "Request body is too large",
        _ => status.canonical_reason().unwrap_or("Request rejected"),
    };
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response.headers_mut().remove(header::CONTENT_LENGTH);
    *response.body_mut() = if head {
        axum::body::Body::empty()
    } else {
        axum::body::Body::from(json!({"error":message}).to_string())
    };
    response
}

struct Error(StatusCode, &'static str);
type Result<T> = std::result::Result<T, Error>;

impl From<sqlx::Error> for Error {
    fn from(_err: sqlx::Error) -> Self {
        Error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Database operation failed",
        )
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (
            self.0,
            [(header::CACHE_CONTROL, "no-store")],
            Json(json!({"error":self.1})),
        )
            .into_response()
    }
}

fn invalid(msg: &'static str) -> Error {
    Error(StatusCode::BAD_REQUEST, msg)
}

fn missing() -> Error {
    Error(StatusCode::NOT_FOUND, "Resource not found")
}

fn token<const N: usize>(prefix: &str) -> String {
    let mut bytes = [0u8; N];
    OsRng.fill_bytes(&mut bytes);
    format!("{prefix}{}", hex::encode(bytes))
}

fn digest(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

fn bearer(headers: &HeaderMap) -> Result<Option<&str>> {
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let Some(first) = values.next() else {
        return Ok(None);
    };
    let value = first
        .to_str()
        .ok()
        .and_then(|s| s.split_once(' '))
        .filter(|(scheme, raw)| {
            scheme.eq_ignore_ascii_case("Bearer")
                && raw.starts_with("mtv_")
                && matches!(raw.len(), 68 | 76)
        })
        .map(|(_, raw)| raw);
    if values.next().is_some() || value.is_none() {
        return Err(Error(StatusCode::UNAUTHORIZED, "Invalid bearer credential"));
    }
    Ok(value)
}

#[cfg(test)]
mod tests;
