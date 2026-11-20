use super::{adapter::Artifact, http, integrity};
use crate::AppState;
use anyhow::{Context, Result, ensure};
use reqwest::{
    StatusCode,
    header::{
        CONTENT_LENGTH, CONTENT_RANGE, ETAG, HeaderMap, HeaderValue, IF_NONE_MATCH, IF_RANGE, RANGE,
    },
};
use sqlx::Row;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{fs, io::AsyncWriteExt};

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}
fn limit() -> u64 {
    std::env::var("METIVA_MAX_ARTIFACT_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(32u64 << 30)
        .min(i64::MAX as u64)
}

fn retry_after(value: &str, clock: std::time::SystemTime) -> Option<i64> {
    let delay = value.parse::<u64>().ok().or_else(|| {
        Some(
            httpdate::parse_http_date(value)
                .ok()?
                .duration_since(clock)
                .unwrap_or_default()
                .as_secs(),
        )
    })?;
    Some(delay.min(24 * 3600) as i64)
}

fn resume_total(value: &str, offset: u64) -> Option<u64> {
    let (interval, total) = value.strip_prefix("bytes ")?.split_once('/')?;
    let (start, end) = interval.split_once('-')?;
    let total: u64 = total.parse().ok()?;
    let start: u64 = start.parse().ok()?;
    let end: u64 = end.parse().ok()?;
    (start == offset && end >= start && end.checked_add(1) == Some(total)).then_some(total)
}
fn strong_tag(headers: &HeaderMap) -> Option<String> {
    headers
        .get(ETAG)?
        .to_str()
        .ok()
        .filter(|s| s.starts_with('"') && s.ends_with('"') && s.len() >= 2)
        .map(str::to_string)
}

pub(crate) struct Transfer {
    pub id: i64,
    pub filename: String,
    pub item: Artifact,
    pub curl: bool,
    pub retries: u32,
    pub limits: Arc<http::Limits>,
}

impl Transfer {
    fn path(&self, state: &AppState) -> PathBuf {
        state.data.join("staging").join(format!("{}.part", self.id))
    }

    async fn verify(&self, path: PathBuf, digest: Option<String>) -> Result<(String, u64)> {
        let name = self.filename.clone();
        let size = self.item.size;
        let hashes = self.item.hashes.clone();
        tokio::task::spawn_blocking(move || {
            integrity::verify(&path, &name, size, digest.as_deref(), &hashes)
        })
        .await?
    }

    async fn store(&self, state: &AppState, path: &Path) -> Result<(String, u64)> {
        sqlx::query("UPDATE transfers SET state='verifying',speed=0 WHERE id=?")
            .bind(self.id)
            .execute(&state.db)
            .await?;
        let verified = self.verify(path.to_owned(), self.item.sha256.clone()).await;
        let (digest, size) = match verified {
            Ok(v) => v,
            Err(error) => {
                fs::remove_file(path).await?;
                return Err(error);
            }
        };
        // A digest name can only expose complete, durable bytes. Retain all objects for history.
        let target = state.data.join("artifacts").join(&digest);
        match fs::hard_link(path, &target).await {
            Ok(()) => {
                fs::File::open(state.data.join("artifacts"))
                    .await?
                    .sync_all()
                    .await?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if self
                    .verify(target.clone(), Some(digest.clone()))
                    .await
                    .is_err()
                {
                    fs::rename(path, &target).await?;
                    fs::File::open(state.data.join("artifacts"))
                        .await?
                        .sync_all()
                        .await?;
                    return Ok((digest, size));
                }
            }
            Err(e) => return Err(e.into()),
        }
        fs::remove_file(path).await?;
        Ok((digest, size))
    }

    async fn attempt(&self, state: &AppState) -> Result<(String, u64)> {
        ensure!(
            self.item.size.unwrap_or(0) <= limit(),
            "artifact exceeds configured limit"
        );
        let row = sqlx::query("SELECT validator,effective_url,digest FROM transfers WHERE id=?")
            .bind(self.id)
            .fetch_one(&state.db)
            .await?;
        if let Some(digest) = self.item.sha256.clone().or_else(|| row.get("digest")) {
            let object = state.data.join("artifacts").join(&digest);
            if fs::try_exists(&object).await?
                && let Ok(verified) = self.verify(object, Some(digest)).await
            {
                return Ok(verified);
            }
        }
        let previous=sqlx::query("SELECT digest,validator,effective_url FROM transfers WHERE id<>? AND state='completed' AND source_url=? AND validator IS NOT NULL AND digest IS NOT NULL ORDER BY id DESC LIMIT 1").bind(self.id).bind(self.item.url.as_str()).fetch_optional(&state.db).await?;
        let mut revalidate = previous.is_some() && self.item.sha256.is_none();
        let path = self.path(state);
        let offset = match fs::metadata(&path).await {
            Ok(m) => m.len(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
            Err(e) => return Err(e.into()),
        };
        let tag: Option<String> = row.get("validator");
        let previous_url: Option<String> = row.get("effective_url");
        let mut range =
            offset > 0 && offset <= limit() && (tag.is_some() || self.item.sha256.is_some());
        let (mut response, start, total, tag) = loop {
            sqlx::query("UPDATE transfers SET state='connecting',speed=0,error=NULL,retry_at=NULL WHERE id=?").bind(self.id).execute(&state.db).await?;
            let mut headers = HeaderMap::new();
            if range {
                headers.insert(RANGE, HeaderValue::from_str(&format!("bytes={offset}-"))?);
                if let Some(tag) = &tag {
                    headers.insert(IF_RANGE, HeaderValue::from_str(tag)?);
                }
            }
            if !range
                && revalidate
                && let Some(prev) = &previous
            {
                headers.insert(
                    IF_NONE_MATCH,
                    HeaderValue::from_str(prev.get::<&str, _>("validator"))?,
                );
            }
            let response = http::limited_get(
                self.item.url.clone(),
                headers,
                self.curl,
                Some(&self.limits),
            )
            .await?;
            if matches!(response.status.as_u16(), 429 | 503)
                && let Some(delay) = response
                    .headers
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| retry_after(v, std::time::SystemTime::now()))
            {
                let deadline = now() + delay;
                sqlx::query("INSERT INTO origin_cooldowns(host,retry_after) VALUES(?,?) ON CONFLICT(host) DO UPDATE SET retry_after=MAX(retry_after,excluded.retry_after)").bind(self.item.url.host_str()).bind(deadline).execute(&state.db).await?;
                anyhow::bail!(
                    "HTTP {} requested Retry-After of {delay} seconds",
                    response.status.as_u16()
                );
            }
            if !range && revalidate && response.status == StatusCode::NOT_MODIFIED {
                let prev = previous.as_ref().context("unsolicited artifact 304")?;
                let digest: String = prev.get("digest");
                let effective: Option<String> = prev.get("effective_url");
                if effective.as_deref() == Some(response.url.as_str())
                    && let Ok(verified) = self
                        .verify(state.data.join("artifacts").join(&digest), Some(digest))
                        .await
                {
                    sqlx::query("UPDATE transfers SET effective_url=?,validator=? WHERE id=?")
                        .bind(effective)
                        .bind(prev.get::<&str, _>("validator"))
                        .bind(self.id)
                        .execute(&state.db)
                        .await?;
                    return Ok(verified);
                }
                revalidate = false;
                continue;
            }
            let new_tag = strong_tag(&response.headers);
            let length = response
                .headers
                .get(CONTENT_LENGTH)
                .map(|v| v.to_str()?.parse::<u64>().context("invalid Content-Length"))
                .transpose()?;
            if range && response.status == StatusCode::PARTIAL_CONTENT {
                let total = response
                    .headers
                    .get(CONTENT_RANGE)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| resume_total(s, offset));
                let validator_matches = previous_url.as_deref() == Some(response.url.as_str())
                    && match &tag {
                        Some(expected) => new_tag.as_ref() == Some(expected),
                        None => self.item.sha256.is_some() && new_tag.is_none(),
                    };
                if let Some(total) =
                    total.filter(|n| validator_matches && length.is_none_or(|l| l == *n - offset))
                {
                    break (response, offset, Some(total), new_tag);
                }
                range = false;
                continue;
            }
            if range && response.status == StatusCode::RANGE_NOT_SATISFIABLE {
                range = false;
                continue;
            }
            ensure!(
                response.status == StatusCode::OK,
                "artifact server returned HTTP {}",
                response.status.as_u16()
            );
            break (response, 0, length, new_tag);
        };
        ensure!(
            response
                .headers
                .get(reqwest::header::CONTENT_ENCODING)
                .is_none_or(|v| v == "identity"),
            "unsupported content encoding"
        );
        if let (Some(a), Some(b)) = (total, self.item.size) {
            ensure!(a == b, "advertised artifact size mismatch");
        }
        let total = total.or(self.item.size);
        ensure!(
            total.unwrap_or(0) <= limit(),
            "artifact exceeds configured limit"
        );
        if let Some(total) = total {
            ensure!(
                fs2::available_space(&state.data)? >= total.saturating_sub(start),
                "insufficient storage"
            );
        }
        let mut options = fs::OpenOptions::new();
        options.create(true).write(true);
        if start == 0 {
            options.truncate(true);
        } else {
            options.append(true);
        }
        let mut file = options.open(&path).await?;
        ensure!(
            start == 0 || file.metadata().await?.len() == start,
            "staging size changed during resume"
        );
        sqlx::query("UPDATE transfers SET state='downloading',bytes=?,total=?,validator=?,effective_url=? WHERE id=?").bind(start as i64).bind(total.map(|n|n as i64)).bind(tag).bind(response.url.as_str()).bind(self.id).execute(&state.db).await?;
        let mut bytes = start;
        let mut previous = (Instant::now(), start);
        while let Some(chunk) = response.chunk().await? {
            bytes = bytes
                .checked_add(chunk.len() as u64)
                .context("size overflow")?;
            ensure!(
                bytes <= total.unwrap_or(limit()) && bytes <= limit(),
                "artifact exceeds byte limit"
            );
            file.write_all(&chunk).await?;
            if previous.0.elapsed() >= Duration::from_millis(400) {
                let speed = (bytes - previous.1) as f64 / previous.0.elapsed().as_secs_f64();
                sqlx::query("UPDATE transfers SET bytes=?,speed=? WHERE id=?")
                    .bind(bytes as i64)
                    .bind(speed)
                    .bind(self.id)
                    .execute(&state.db)
                    .await?;
                previous = (Instant::now(), bytes);
            }
        }
        ensure!(total.is_none_or(|n| bytes == n), "truncated artifact");
        file.flush().await?;
        file.sync_all().await?;
        drop(file);
        self.store(state, &path).await
    }

    async fn stop_command(&self, state: &AppState) -> Result<String> {
        loop {
            let cmd: Option<String> =
                sqlx::query_scalar("SELECT command FROM transfers WHERE id=?")
                    .bind(self.id)
                    .fetch_one(&state.db)
                    .await?;
            if let Some(cmd) = cmd.filter(|c| matches!(c.as_str(), "pause" | "cancel")) {
                return Ok(cmd);
            }
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
    }

    pub async fn run(&self, state: &AppState) -> Result<(String, u64)> {
        loop {
            let row =
                sqlx::query("SELECT state,command,retries,retry_at FROM transfers WHERE id=?")
                    .bind(self.id)
                    .fetch_one(&state.db)
                    .await?;
            let status: String = row.get("state");
            let cmd: Option<String> = row.get("command");
            if cmd.as_deref() == Some("cancel") || status == "cancelled" {
                sqlx::query("UPDATE transfers SET state='cancelled',speed=0,command=NULL,retry_at=NULL WHERE id=?").bind(self.id).execute(&state.db).await?;
                anyhow::bail!("transfer cancelled");
            }
            if cmd.as_deref() == Some("pause") {
                sqlx::query("UPDATE transfers SET state='paused',speed=0,command=NULL WHERE id=? AND command='pause'").bind(self.id).execute(&state.db).await?;
                continue;
            }
            let origin_delay: Option<i64> = sqlx::query_scalar(
                "SELECT retry_after FROM origin_cooldowns WHERE host=? AND retry_after>unixepoch()",
            )
            .bind(self.item.url.host_str())
            .fetch_optional(&state.db)
            .await?;
            if let Some(deadline) = origin_delay
                && (status != "paused" || cmd.as_deref() == Some("resume"))
            {
                sqlx::query("UPDATE transfers SET retry_at=max(coalesce(retry_at,0),?),state='retrying',command=CASE WHEN command IN('resume','retry') THEN NULL ELSE command END WHERE id=?").bind(deadline).bind(self.id).execute(&state.db).await?;
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
            if status == "paused" && cmd.as_deref() != Some("resume")
                || status == "retrying"
                    && cmd.as_deref() != Some("retry")
                    && row.get::<Option<i64>, _>("retry_at").unwrap_or(0) > now()
            {
                tokio::time::sleep(Duration::from_millis(400)).await;
                continue;
            }
            if let Some(cmd) = cmd {
                sqlx::query(
                    "UPDATE transfers SET command=NULL,state='queued' WHERE id=? AND command=?",
                )
                .bind(self.id)
                .bind(cmd)
                .execute(&state.db)
                .await?;
            }
            let result = tokio::select! {
                result=self.attempt(state)=>result,
                command=self.stop_command(state)=>{command?;continue;}
            };
            match result {
                Ok((digest, size)) => {
                    sqlx::query("UPDATE transfers SET state='completed',bytes=?,total=?,speed=0,digest=?,retry_at=NULL,error=NULL,command=NULL WHERE id=?").bind(size as i64).bind(size as i64).bind(&digest).bind(self.id).execute(&state.db).await?;
                    return Ok((digest, size));
                }
                Err(error) => {
                    let retries: u32 = row.get("retries");
                    let message = format!("{error:#}");
                    if retries >= self.retries {
                        sqlx::query("UPDATE transfers SET state='failed',speed=0,retry_at=NULL,error=? WHERE id=?").bind(&message).bind(self.id).execute(&state.db).await?;
                        return Err(error);
                    }
                    let base = 2_i64.pow(retries.min(8) + 1);
                    let jitter = i64::from(rand::random::<u16>()) % (base / 2 + 1);
                    let backoff = now() + base + jitter;
                    let retry_at:i64=sqlx::query_scalar("SELECT MAX(?,COALESCE((SELECT retry_after FROM origin_cooldowns WHERE host=?),0))").bind(backoff).bind(self.item.url.host_str()).fetch_one(&state.db).await?;
                    sqlx::query("UPDATE transfers SET state='retrying',speed=0,retry_at=?,retries=retries+1,error=? WHERE id=?").bind(retry_at).bind(message).bind(self.id).execute(&state.db).await?;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resume_requires_an_exact_tail() {
        assert_eq!(resume_total("bytes 42-99/100", 42), Some(100));
        for value in [
            "bytes 0-99/100",
            "bytes 42-97/100",
            "bytes 42-100/*",
            "bytes 42-41/42",
        ] {
            assert_eq!(resume_total(value, 42), None);
        }
    }
}
