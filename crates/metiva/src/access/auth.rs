use super::*;
use anyhow::{Context, bail};
use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
};
use axum::{
    extract::{Request, State},
    http::Method,
    middleware::{self, Next},
    routing::{get, post},
};
use serde::Deserialize;
use sqlx::{Row, SqlitePool};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};
use subtle::ConstantTimeEq;

const COOKIE: &str = "metiva_admin";
const COOKIE_FLAGS: &str = "Path=/api/admin; HttpOnly; SameSite=Strict; Secure";
const FILE: &str = "bootstrap-secret";

pub(super) fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/api/admin/logout", post(logout))
        .route("/api/admin/password", post(change_password))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_admin))
        .route("/api/admin/session", get(session))
        .route("/api/admin/login", post(login))
        .route("/api/admin/setup", post(setup))
        .route_layer(middleware::from_fn_with_state(state, credential_headers))
        .layer(axum::extract::DefaultBodyLimit::max(8192))
}

// Login and setup run before a browser has a session-bound CSRF token.
async fn credential_headers(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let headers = req.headers();
    if headers.contains_key(header::AUTHORIZATION) {
        return Error(StatusCode::UNAUTHORIZED, "Administrator session required").into_response();
    }
    if req.method() == Method::POST
        && (headers
            .get(header::ORIGIN)
            .is_some_and(|origin| origin.as_bytes() != state.public_url.as_bytes())
            || headers
                .get("sec-fetch-site")
                .is_some_and(|site| site == "cross-site"))
    {
        return Error(StatusCode::FORBIDDEN, "Cross-origin authentication request").into_response();
    }
    next.run(req).await
}

fn load_secret(path: &Path) -> anyhow::Result<String> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.permissions().mode() & 0o077 != 0 {
        bail!("bootstrap-secret must be a regular owner-only file");
    }
    let mut value = String::new();
    File::open(path)?.take(100).read_to_string(&mut value)?;
    let value = value.trim_end();
    if value.len() != 68 || !value.starts_with("mtb_") {
        bail!("Invalid bootstrap-secret file");
    }
    Ok(value.to_owned())
}

fn create_secret(dir: &Path) -> anyhow::Result<()> {
    let path = dir.join(FILE);
    if path.try_exists()? {
        load_secret(&path)?;
        return Ok(());
    }
    let temp: PathBuf = dir.join(token::<32>(".bootstrap-"));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temp)?;
    let install = (|| -> anyhow::Result<()> {
        writeln!(file, "{}", token::<32>("mtb_"))?;
        file.sync_all()?;
        match fs::hard_link(&temp, &path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                load_secret(&path).map(|_| ())
            }
            Err(err) => Err(err.into()),
        }
    })();
    fs::remove_file(temp)?;
    install
}

pub async fn initialize(pool: &SqlitePool, data: &Path) -> anyhow::Result<()> {
    let configured: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM admin_credential)")
        .fetch_one(pool)
        .await?;
    if configured {
        match fs::remove_file(data.join(FILE)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
    } else {
        create_secret(data)?;
    }
    Ok(())
}

fn valid_password(value: &str) -> bool {
    (12..=256).contains(&value.len())
}

async fn hash_password(password: String) -> anyhow::Result<String> {
    tokio::task::spawn_blocking(move || {
        Argon2::default()
            .hash_password(password.as_bytes(), &SaltString::generate(&mut OsRng))
            .map(|hash| hash.to_string())
            .map_err(|_| anyhow::anyhow!("Password hashing failed"))
    })
    .await?
}

async fn verify(password: String, hash: String) -> bool {
    tokio::task::spawn_blocking(move || {
        PasswordHash::new(&hash).is_ok_and(|h| {
            Argon2::default()
                .verify_password(password.as_bytes(), &h)
                .is_ok()
        })
    })
    .await
    .unwrap_or(false)
}

pub async fn recover(pool: &SqlitePool, password: &str) -> anyhow::Result<()> {
    if !valid_password(password) {
        bail!("Password must contain 12–256 bytes");
    }
    let hash = hash_password(password.to_owned()).await?;
    let mut tx = pool.begin().await?;
    let n = sqlx::query("UPDATE admin_credential SET password_hash=? WHERE id=1")
        .bind(hash)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if n == 0 {
        bail!("Administrator setup has not been completed");
    }
    sqlx::query("DELETE FROM admin_sessions")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM auth_attempts")
        .execute(&mut *tx)
        .await?;
    tx.commit().await.context("Saving recovered credential")
}

// A shared database-backed limit also bounds simultaneous expensive password verifications.
async fn attempt(db: &SqlitePool, purpose: &str) -> Result<()> {
    let permit: Option<i64> = sqlx::query_scalar(
        "INSERT INTO auth_attempts(purpose,since,attempts) VALUES(?,unixepoch(),1)
         ON CONFLICT(purpose) DO UPDATE SET attempts=CASE WHEN since<=unixepoch()-300 THEN 1 ELSE attempts+1 END,
         since=CASE WHEN since<=unixepoch()-300 THEN unixepoch() ELSE since END
         WHERE since<=unixepoch()-300 OR attempts<8 RETURNING attempts"
    ).bind(purpose).fetch_optional(db).await?;
    permit.map(|_| ()).ok_or(Error(
        StatusCode::TOO_MANY_REQUESTS,
        "Too many attempts. Try again in five minutes.",
    ))
}

fn cookie(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|s| s.split(';'))
        .filter_map(|s| s.trim().split_once('='))
        .find(|(name, value)| *name == COOKIE && value.len() == 68 && value.starts_with("mts_"))
        .map(|(_, value)| value)
}

fn csrf(raw: &str) -> String {
    digest(&format!("csrf:{raw}"))
}

async fn identity(db: &SqlitePool, headers: &HeaderMap) -> Result<Option<String>> {
    let Some(raw) = cookie(headers) else {
        return Ok(None);
    };
    Ok(sqlx::query_scalar("SELECT username FROM admin_credential JOIN admin_sessions ON digest=? WHERE expires_at>unixepoch()")
        .bind(digest(raw)).fetch_optional(db).await?)
}

pub async fn valid_session(db: &SqlitePool, headers: &HeaderMap) -> bool {
    !headers.contains_key(header::AUTHORIZATION)
        && matches!(identity(db, headers).await, Ok(Some(_)))
}

pub async fn require_admin(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let headers = req.headers();
    let method = req.method();
    let check = async {
        if headers.contains_key(header::AUTHORIZATION)
            || identity(&state.db, headers).await?.is_none()
        {
            return Err(Error(
                StatusCode::UNAUTHORIZED,
                "Administrator session required",
            ));
        }
        if !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
            let supplied = headers
                .get("x-csrf-token")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default();
            if !bool::from(
                csrf(cookie(headers).unwrap_or_default())
                    .as_bytes()
                    .ct_eq(supplied.as_bytes()),
            ) {
                return Err(Error(StatusCode::FORBIDDEN, "Invalid CSRF token"));
            }
        }
        Ok(())
    }
    .await;
    match check {
        Ok(()) => {
            let mut res = next.run(req).await;
            res.headers_mut()
                .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
            res
        }
        Err(err) => err.into_response(),
    }
}

fn logged_in(name: &str, raw: &str) -> serde_json::Value {
    json!({"authenticated":true,"setupRequired":false,"username":name,"csrfToken":csrf(raw)})
}

async fn session(State(state): State<AppState>, headers: HeaderMap) -> Result<Response> {
    let body = if let Some(name) = identity(&state.db, &headers).await? {
        logged_in(&name, cookie(&headers).unwrap_or_default())
    } else {
        let setup: bool = sqlx::query_scalar("SELECT NOT EXISTS(SELECT 1 FROM admin_credential)")
            .fetch_one(&state.db)
            .await?;
        json!({"authenticated":false,"setupRequired":setup})
    };
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(body)).into_response())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Setup {
    bootstrap_secret: String,
    username: String,
    password: String,
}

async fn setup(State(state): State<AppState>, Json(input): Json<Setup>) -> Result<Response> {
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM admin_credential)")
        .fetch_one(&state.db)
        .await?;
    if exists {
        return Err(Error(
            StatusCode::CONFLICT,
            "Administrator already configured",
        ));
    }
    attempt(&state.db, "setup").await?;
    let path = state.data.join(FILE);
    let expected = load_secret(&path).map_err(|_| {
        Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Bootstrap file unavailable",
        )
    })?;
    if !bool::from(
        digest(&expected)
            .as_bytes()
            .ct_eq(digest(&input.bootstrap_secret).as_bytes()),
    ) {
        return Err(Error(StatusCode::UNAUTHORIZED, "Invalid bootstrap secret"));
    }
    let username = input.username.trim();
    if username.is_empty()
        || username.len() > 64
        || username.chars().any(char::is_control)
        || !valid_password(&input.password)
    {
        return Err(invalid(
            "Enter a username of 1–64 bytes and password of 12–256 bytes",
        ));
    }
    let hash = hash_password(input.password)
        .await
        .map_err(|_| Error(StatusCode::INTERNAL_SERVER_ERROR, "Password hashing failed"))?;
    let mut tx = state.db.begin().await?;
    let n=sqlx::query("INSERT INTO admin_credential(id,username,password_hash) VALUES(1,?,?) ON CONFLICT DO NOTHING")
        .bind(username).bind(&hash).execute(&mut *tx).await?.rows_affected();
    if n == 0 {
        return Err(Error(
            StatusCode::CONFLICT,
            "Administrator already configured",
        ));
    }
    let res = new_session(&mut tx, username).await?;
    tx.commit().await?;
    // The database permanently consumes the bootstrap credential even if unlinking fails.
    let _ = fs::remove_file(path);
    Ok(res)
}

async fn new_session(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    username: &str,
) -> Result<Response> {
    let raw = token::<32>("mts_");
    sqlx::query("DELETE FROM admin_sessions WHERE expires_at<=unixepoch()")
        .execute(&mut **tx)
        .await?;
    sqlx::query("INSERT INTO admin_sessions(digest,expires_at) VALUES(?,unixepoch()+43200)")
        .bind(digest(&raw))
        .execute(&mut **tx)
        .await?;
    Ok((
        [
            (
                header::SET_COOKIE,
                format!("{COOKIE}={raw}; {COOKIE_FLAGS}; Max-Age=43200"),
            ),
            (header::CACHE_CONTROL, "no-store".to_owned()),
        ],
        Json(logged_in(username, &raw)),
    )
        .into_response())
}

#[derive(Deserialize)]
struct Login {
    username: String,
    password: String,
}

async fn login(State(state): State<AppState>, Json(input): Json<Login>) -> Result<Response> {
    attempt(&state.db, "login").await?;
    let err = || Error(StatusCode::UNAUTHORIZED, "Invalid username or password");
    let row = sqlx::query("SELECT username,password_hash FROM admin_credential")
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(err)?;
    let name: String = row.get("username");
    let hash: String = row.get("password_hash");
    if input.password.len() > 256
        || !verify(input.password, hash.clone()).await
        || name != input.username
    {
        return Err(err());
    }
    let mut tx = state.db.begin().await?;
    sqlx::query("DELETE FROM auth_attempts WHERE purpose='login'")
        .execute(&mut *tx)
        .await?;
    let still_valid: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM admin_credential WHERE password_hash=?)")
            .bind(hash)
            .fetch_one(&mut *tx)
            .await?;
    if !still_valid {
        return Err(err());
    }
    let res = new_session(&mut tx, &name).await?;
    tx.commit().await?;
    Ok(res)
}

fn signed_out() -> Response {
    (
        StatusCode::NO_CONTENT,
        [(
            header::SET_COOKIE,
            format!("{COOKIE}=; {COOKIE_FLAGS}; Max-Age=0"),
        )],
    )
        .into_response()
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Result<Response> {
    sqlx::query("DELETE FROM admin_sessions WHERE digest=?")
        .bind(digest(cookie(&headers).unwrap_or_default()))
        .execute(&state.db)
        .await?;
    Ok(signed_out())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Change {
    current_password: String,
    new_password: String,
}
async fn change_password(
    State(state): State<AppState>,
    Json(input): Json<Change>,
) -> Result<Response> {
    if !valid_password(&input.new_password) {
        return Err(invalid("Password must contain 12–256 bytes"));
    }
    attempt(&state.db, "change").await?;
    let old: String = sqlx::query_scalar("SELECT password_hash FROM admin_credential")
        .fetch_one(&state.db)
        .await?;
    if input.current_password.len() > 256 || !verify(input.current_password, old.clone()).await {
        return Err(Error(StatusCode::UNAUTHORIZED, "Invalid current password"));
    }
    let hash = hash_password(input.new_password)
        .await
        .map_err(|_| Error(StatusCode::INTERNAL_SERVER_ERROR, "Password hashing failed"))?;
    let mut tx = state.db.begin().await?;
    let n = sqlx::query("UPDATE admin_credential SET password_hash=? WHERE password_hash=?")
        .bind(hash)
        .bind(old)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if n == 0 {
        return Err(Error(
            StatusCode::CONFLICT,
            "Credential changed concurrently",
        ));
    }
    sqlx::query("DELETE FROM admin_sessions")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM auth_attempts")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(signed_out())
}
