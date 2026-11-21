use anyhow::{Context, Result, bail};
use fs2::FileExt;
use metiva::{access, app, open, worker};
use std::{env, fs::OpenOptions, future::IntoFuture, path::PathBuf, pin::pin, time::Duration};
use tokio::signal::unix::{SignalKind, signal};

async fn shutdown() {
    let mut term = signal(SignalKind::terminate()).expect("register SIGTERM");
    tokio::select! { _ = term.recv() => (), _ = tokio::signal::ctrl_c() => () }
}

fn validate_limits() -> Result<()> {
    for (name, max) in [
        ("METIVA_CONNECT_TIMEOUT_SECS", 3600),
        ("METIVA_IDLE_TIMEOUT_SECS", 3600),
        ("METIVA_MAX_ARTIFACT_BYTES", i64::MAX as u64),
    ] {
        if let Ok(text) = env::var(name) {
            let value: u64 = text
                .parse()
                .with_context(|| format!("{name} requires an integer"))?;
            anyhow::ensure!(
                (1..=max).contains(&value),
                "{name} must be between 1 and {max}"
            );
        }
    }
    Ok(())
}

async fn heartbeat(state: &metiva::AppState) -> Result<()> {
    loop {
        sqlx::query("SELECT 1").execute(&state.db).await?;
        tokio::fs::write(state.data.join("db/worker-heartbeat"), b"ready").await?;
        tokio::time::sleep(Duration::from_secs(15)).await;
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let role = env::args().nth(1).unwrap_or_else(|| "api".into());
    let data = PathBuf::from(env::var_os("METIVA_DATA").unwrap_or_else(|| "./data".into()));
    let origin =
        env::var("METIVA_PUBLIC_URL").context("Set METIVA_PUBLIC_URL to the external origin")?;
    if role == "worker-health" {
        let age = std::fs::metadata(data.join("db/worker-heartbeat"))?
            .modified()?
            .elapsed()?;
        if age.as_secs() > 60 {
            bail!("Worker heartbeat expired")
        }
        return Ok(());
    }
    validate_limits()?;
    let state = open(&data, &origin).await?;
    match role.as_str() {
        "api" => {
            access::initialize(&state.db, &data).await?;
            let address = env::var("METIVA_LISTEN").unwrap_or_else(|_| "0.0.0.0:8080".into());
            let listener = tokio::net::TcpListener::bind(address).await?;
            eprintln!("Metiva API listening on {}", listener.local_addr()?);
            let stopping = state.shutdown.clone();
            let mut server = pin!(
                axum::serve(listener, app(state))
                    .with_graceful_shutdown(stopping.clone().cancelled_owned())
                    .into_future()
            );
            tokio::select! {
                result = &mut server => result?,
                () = shutdown() => {
                    stopping.cancel();
                    // Allow in-flight responses to finish within Compose's 30s stop grace period.
                    match tokio::time::timeout(Duration::from_secs(25), server).await {
                        Ok(result) => result?,
                        Err(_) => eprintln!("API drain deadline reached"),
                    }
                }
            }
        }
        "worker" => {
            let lock = OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(data.join("db/worker.lock"))?;
            lock.try_lock_exclusive()
                .context("A worker already owns this data directory")?;
            eprintln!("Metiva worker ready");
            tokio::select! { result = heartbeat(&state) => result?, result = worker::run(state.clone()) => result?, _ = shutdown() => () }
        }
        "recover" => {
            let password = rpassword::prompt_password("New administrator password: ")?;
            let confirm = rpassword::prompt_password("Confirm password: ")?;
            if password != confirm {
                bail!("Passwords differ")
            }
            access::recover(&state.db, &password).await?;
            println!("Password changed. Existing sessions have ended.");
        }
        _ => bail!("Usage: metiva [api|worker|recover]"),
    }
    Ok(())
}
