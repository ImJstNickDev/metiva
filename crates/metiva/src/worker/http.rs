use anyhow::{Context, Result, anyhow, bail, ensure};
use reqwest::{
    StatusCode,
    header::{HeaderMap, HeaderName, HeaderValue, LOCATION},
};
use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, SocketAddr},
    process::Stdio,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    net::lookup_host,
    process::{Child, ChildStdout, Command},
    sync::{OwnedSemaphorePermit, Semaphore},
    time::timeout,
};
use url::{Host, Url};

fn seconds(name: &str, default: u64) -> Duration {
    Duration::from_secs(
        std::env::var(name)
            .ok()
            .and_then(|n| n.parse::<u64>().ok())
            .unwrap_or(default)
            .clamp(1, 3600),
    )
}
fn connect_timeout() -> Duration {
    seconds("METIVA_CONNECT_TIMEOUT_SECS", 15)
}
fn idle_timeout() -> Duration {
    seconds("METIVA_IDLE_TIMEOUT_SECS", 45)
}
const REDIRECTS: usize = 6;

pub(crate) fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(matches!(a, 0 | 10 | 127 | 224..=255)
                || (a == 100 && (64..128).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..32).contains(&b))
                || (a == 192
                    && (b == 168 || (b == 0 && (c == 0 || c == 2)) || (b == 88 && c == 99)))
                || (a == 198 && (b == 18 || b == 19 || (b == 51 && c == 100)))
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(ip) => {
            let s = ip.segments();
            (s[0] & 0xe000) == 0x2000
                && s[0] != 0x2002
                && !(s[0] == 0x2001 && (s[1] < 0x200 || s[1] == 0xdb8))
                && !(s[0] == 0x3fff && s[1] < 0x1000)
        }
    }
}

pub(crate) fn validate_url(url: &Url) -> Result<()> {
    ensure!(
        matches!(url.scheme(), "http" | "https"),
        "expected an HTTP(S) URL"
    );
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "URL credentials are forbidden"
    );
    ensure!(
        !url.query_pairs().any(|(key, _)| {
            let k = key.to_ascii_lowercase();
            matches!(
                k.as_str(),
                "token"
                    | "access_token"
                    | "api_key"
                    | "apikey"
                    | "key"
                    | "authorization"
                    | "signature"
                    | "sig"
                    | "password"
                    | "credential"
            ) || k.starts_with("x-amz-")
                || k.starts_with("x-goog-")
        }),
        "URLs containing credentials are forbidden"
    );
    match url.host().context("URL requires a hostname")? {
        Host::Domain(host) => {
            let host = host.trim_end_matches('.');
            ensure!(
                host.contains('.')
                    && ![
                        "localhost",
                        "local",
                        "internal",
                        "home.arpa",
                        "onion",
                        "test",
                        "invalid"
                    ]
                    .iter()
                    .any(|suffix| host == *suffix || host.ends_with(&format!(".{suffix}"))),
                "local hostname is forbidden"
            );
        }
        Host::Ipv4(ip) => ensure!(public_ip(ip.into()), "non-public address is forbidden"),
        Host::Ipv6(ip) => ensure!(public_ip(ip.into()), "non-public address is forbidden"),
    }
    Ok(())
}

async fn resolve(url: &Url) -> Result<(String, Vec<SocketAddr>)> {
    validate_url(url)?;
    let host = url.host().unwrap().to_string();
    let port = url.port_or_known_default().context("missing port")?;
    let addresses = match url.host().unwrap() {
        Host::Ipv4(ip) => vec![SocketAddr::new(ip.into(), port)],
        Host::Ipv6(ip) => vec![SocketAddr::new(ip.into(), port)],
        Host::Domain(name) => timeout(connect_timeout(), lookup_host((name, port)))
            .await
            .context("DNS timed out")??
            .collect(),
    };
    ensure!(
        !addresses.is_empty() && addresses.iter().all(|address| public_ip(address.ip())),
        "hostname must resolve exclusively to public addresses"
    );
    Ok((host, addresses))
}

pub(crate) struct Limits {
    global: Arc<Semaphore>,
    hosts: Mutex<HashMap<String, Weak<Semaphore>>>,
    objects: Mutex<HashMap<String, Weak<Semaphore>>>,
    per_host: usize,
}
impl Limits {
    pub fn new(global: usize, per_host: usize) -> Self {
        Self {
            global: Arc::new(Semaphore::new(global)),
            hosts: Mutex::new(HashMap::new()),
            objects: Mutex::default(),
            per_host,
        }
    }
    fn keyed(
        map: &Mutex<HashMap<String, Weak<Semaphore>>>,
        key: String,
        count: usize,
    ) -> Arc<Semaphore> {
        let mut owners = map.lock().unwrap();
        owners.retain(|_, weak| weak.strong_count() > 0);
        let slot = owners.entry(key).or_default();
        slot.upgrade().unwrap_or_else(|| {
            let sem = Arc::new(Semaphore::new(count));
            *slot = Arc::downgrade(&sem);
            sem
        })
    }
    pub async fn object(&self, key: String) -> Result<OwnedSemaphorePermit> {
        Ok(Self::keyed(&self.objects, key, 1).acquire_owned().await?)
    }
    async fn acquire(&self, host: &str) -> Result<(OwnedSemaphorePermit, OwnedSemaphorePermit)> {
        let host_limit = Self::keyed(
            &self.hosts,
            host.trim_end_matches('.').to_lowercase(),
            self.per_host,
        );
        let host = host_limit.acquire_owned().await?;
        Ok((host, self.global.clone().acquire_owned().await?))
    }
}

pub(crate) enum Body {
    Native(reqwest::Response),
    Curl {
        child: Child,
        reader: BufReader<ChildStdout>,
    },
}

pub(crate) struct Response {
    pub url: Url,
    pub status: StatusCode,
    pub headers: HeaderMap,
    body: Body,
    _permits: Option<(OwnedSemaphorePermit, OwnedSemaphorePermit)>,
}

impl Response {
    pub async fn chunk(&mut self) -> Result<Option<Vec<u8>>> {
        timeout(idle_timeout(), async {
            match &mut self.body {
                Body::Native(response) => Ok(response
                    .chunk()
                    .await
                    .map_err(|e| e.without_url())?
                    .map(|x| x.to_vec())),
                Body::Curl { child, reader } => {
                    let mut buf = vec![0; 64 * 1024];
                    let n = reader.read(&mut buf).await?;
                    if n == 0 {
                        ensure!(child.wait().await?.success(), "curl transfer failed");
                        return Ok(None);
                    }
                    buf.truncate(n);
                    Ok(Some(buf))
                }
            }
        })
        .await
        .context("HTTP body read timed out")?
    }
    pub async fn bounded(mut self, limit: usize) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        while let Some(chunk) = self.chunk().await? {
            ensure!(
                chunk.len() <= limit.saturating_sub(bytes.len()),
                "response exceeds byte limit"
            );
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }
}

async fn curl_response(
    url: &Url,
    host: &str,
    addresses: &[SocketAddr],
    headers: &HeaderMap,
) -> Result<Response> {
    let mut cmd = Command::new("curl");
    // -q must be the first option to disable user curl configuration.
    cmd.args([
        "-q",
        "--silent",
        "--http1.1",
        "--globoff",
        "--proxy",
        "",
        "--proto",
        "=http,https",
        "--connect-timeout",
        &connect_timeout().as_secs().to_string(),
        "--max-time",
        "3600",
        "--include",
        "--suppress-connect-headers",
        "--user-agent",
        "Metiva/1.0",
    ]);
    if matches!(url.host(), Some(Host::Domain(_))) {
        let target = match addresses[0].ip() {
            IpAddr::V4(ip) => ip.to_string(),
            IpAddr::V6(ip) => format!("[{ip}]"),
        };
        cmd.arg("--resolve")
            .arg(format!("{host}:{}:{target}", addresses[0].port()));
    }
    for (key, value) in headers {
        cmd.arg("--header")
            .arg(format!("{}: {}", key, value.to_str()?));
    }
    let mut child = cmd
        .arg("--")
        .arg(url.as_str())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("start curl backend")?;
    let mut reader = BufReader::new(child.stdout.take().context("curl output unavailable")?);
    let mut remaining: usize = 64 * 1024;
    let (status, headers) = loop {
        let mut lines = Vec::new();
        loop {
            let mut line = Vec::new();
            let n = (&mut reader)
                .take(remaining as u64)
                .read_until(b'\n', &mut line)
                .await?;
            ensure!(
                n > 0 && n < remaining && line.ends_with(b"\n"),
                "invalid or oversized response headers"
            );
            remaining -= n;
            if line == b"\r\n" || line == b"\n" {
                break;
            }
            lines.push(line);
        }
        let first = lines.first().context("missing HTTP status")?;
        let first = std::str::from_utf8(first)?
            .split_whitespace()
            .collect::<Vec<_>>();
        ensure!(
            first.len() >= 2 && first[0].starts_with("HTTP/"),
            "invalid HTTP status"
        );
        let status = StatusCode::from_bytes(first[1].as_bytes())?;
        if status.is_informational() {
            continue;
        }
        let mut headers = HeaderMap::new();
        for line in &lines[1..] {
            let split = line
                .iter()
                .position(|b| *b == b':')
                .context("invalid HTTP header")?;
            headers.append(
                HeaderName::from_bytes(&line[..split])?,
                HeaderValue::from_bytes(line[split + 1..].trim_ascii())?,
            );
        }
        break (status, headers);
    };
    Ok(Response {
        url: url.clone(),
        status,
        headers,
        body: Body::Curl { child, reader },
        _permits: None,
    })
}

pub(crate) fn retry_after(value: &str, clock: std::time::SystemTime) -> Option<i64> {
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

async fn set_cooldown(db: &sqlx::SqlitePool, host: &str, until: i64) -> Result<()> {
    sqlx::query("INSERT INTO origin_cooldowns(host,retry_after) VALUES(?,?) ON CONFLICT(host) DO UPDATE SET retry_after=MAX(retry_after,excluded.retry_after)").bind(host).bind(until).execute(db).await?;
    Ok(())
}
pub(crate) async fn cooldown(db: &sqlx::SqlitePool, host: &str) -> Result<Option<i64>> {
    Ok(sqlx::query_scalar(
        "SELECT retry_after FROM origin_cooldowns WHERE host=? AND retry_after>unixepoch()",
    )
    .bind(host)
    .fetch_optional(db)
    .await?)
}
pub(crate) async fn limited_get(
    db: &sqlx::SqlitePool,
    mut url: Url,
    mut headers: HeaderMap,
    curl: bool,
    limits: Option<&Limits>,
) -> Result<Response> {
    let mut seen = HashSet::new();
    let original = url.host_str().context("missing hostname")?.to_owned();
    for _ in 0..=REDIRECTS {
        url.set_fragment(None);
        ensure!(seen.insert(url.clone()), "redirect cycle");
        let (host, addresses) = resolve(&url).await?;
        let permits = match limits {
            Some(l) => Some(l.acquire(&host).await?),
            None => None,
        };
        if let Some(until) = cooldown(db, &host).await? {
            if original != host {
                set_cooldown(db, &original, until).await?;
            }
            bail!("upstream Retry-After is active until {until}");
        }
        let mut response = if curl {
            timeout(
                idle_timeout(),
                curl_response(&url, &host, &addresses, &headers),
            )
            .await
            .context("HTTP headers timed out")??
        } else {
            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .user_agent("Metiva/1.0")
                .connect_timeout(connect_timeout())
                .timeout(Duration::from_secs(3600))
                .read_timeout(idle_timeout())
                .resolve_to_addrs(&host, &addresses)
                .build()?;
            let response = client
                .get(url.clone())
                .headers(headers.clone())
                .send()
                .await
                .map_err(|e| e.without_url())?;
            Response {
                url: url.clone(),
                status: response.status(),
                headers: response.headers().clone(),
                body: Body::Native(response),
                _permits: None,
            }
        };
        response._permits = permits;
        if matches!(response.status.as_u16(), 429 | 503)
            && let Some(delay) = response
                .headers
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| retry_after(v, std::time::SystemTime::now()))
        {
            let until = chrono::Utc::now().timestamp() + delay;
            set_cooldown(db, &host, until).await?;
            if original != host {
                set_cooldown(db, &original, until).await?;
            }
        }
        if !matches!(response.status.as_u16(), 301 | 302 | 303 | 307 | 308) {
            return Ok(response);
        }
        let location = response
            .headers
            .get(LOCATION)
            .ok_or_else(|| anyhow!("redirect missing Location"))?
            .to_str()?;
        let next = url.join(location)?;
        ensure!(
            !(url.scheme() == "https" && next.scheme() == "http"),
            "insecure redirect"
        );
        if url.origin() != next.origin() {
            headers.remove(reqwest::header::AUTHORIZATION);
            headers.remove(reqwest::header::COOKIE);
            headers.remove(reqwest::header::IF_MODIFIED_SINCE);
            headers.remove(reqwest::header::IF_NONE_MATCH);
        }
        url = next;
    }
    bail!("too many redirects")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_destinations_only() {
        for value in [
            "http://2130706433",
            "http://169.254.169.254",
            "http://[::ffff:127.0.0.1]",
            "http://[2002:7f00:1::]",
            "http://[fd00::1]",
            "http://router.local",
            "file:///etc/hosts",
            "https://x:y@example.com",
        ] {
            assert!(
                validate_url(&Url::parse(value).unwrap()).is_err(),
                "{value}"
            );
        }
        assert!(validate_url(&Url::parse("https://[2606:4700:4700::1111]/").unwrap()).is_ok());
    }
}
