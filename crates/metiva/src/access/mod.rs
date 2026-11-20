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

fn token(prefix: &str) -> String {
    let mut bytes = [0u8; 32];
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
            scheme.eq_ignore_ascii_case("Bearer") && raw.starts_with("mtv_") && raw.len() == 68
        })
        .map(|(_, raw)| raw);
    if values.next().is_some() || value.is_none() {
        return Err(Error(StatusCode::UNAUTHORIZED, "Invalid bearer credential"));
    }
    Ok(value)
}

#[cfg(test)]
mod tests;
