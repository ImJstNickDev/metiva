# Developing Metiva

Use the Rust toolchain selected by `rust-toolchain.toml` and Node.js 22. The API, worker and recovery command share one crate. The Generic JSON adapter lives under `crates/metiva/src/worker`. Consumer authorization and the OpenAPI contract live under `crates/metiva/src/access`.

## Run the dashboard locally

From the repository root, set the following in each backend terminal. Choose a new data directory for development.

```sh
export METIVA_DATA="$PWD/data/dev"
export METIVA_LISTEN=127.0.0.1:8080
export METIVA_PUBLIC_URL=http://localhost:5173
```

Run `cargo run --locked -- api` in the first terminal and `cargo run --locked -- worker` in the second. In a third terminal, run:

```sh
cd apps/web
npm ci
npm run dev -- --host localhost --port 5173 --strictPort
```

Open `http://localhost:5173` and use the development directory's `bootstrap-secret`. Vite proxies the dashboard and publication APIs to the local service. For artifact byte delivery, run the Compose suite so Nginx can handle authorized internal redirects.

## Before opening a pull request

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --workspace
(cd apps/web && npm test && npm run build)
```

Run `python3 tests/compose/run.py` for changes to the worker, publication, file authorization or deployment. Use the pull request description to record the tested behavior and link the issue. Schema changes belong in new numbered migrations.

## Branches and releases

Start each change on a short-lived branch from `main`. Keep each PR independently reviewable and reversible. Split unrelated changes even when they share a milestone. Link the issues the PR closes and describe how you verified the changed behavior. Keep follow-up work in an issue.

Squash a completed PR after the required Rust, dashboard and deployment checks pass. GitHub removes its head branch after merge. Use an annotated `vX.Y.Z` tag on the tested main commit for each release. A maintenance branch makes sense when two release lines need fixes at the same time.

Follow the [release procedure](docs/deployment.md#publishing-a-release) to publish images and attach the verified deployment files.
