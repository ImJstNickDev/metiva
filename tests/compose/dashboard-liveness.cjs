// Opt-in browser acceptance. Install playwright-core and Chromium, then run
// node tests/compose/dashboard-liveness.cjs SERVICE_IMAGE INGRESS_IMAGE
// with the candidate images built locally. CHROME can select a Chromium binary.
// METIVA_TEST_SUBNET can select an available test CIDR.
const assert = require("node:assert/strict");
const { execFile } = require("node:child_process");
const { randomBytes } = require("node:crypto");
const fs = require("node:fs/promises");
const net = require("node:net");
const os = require("node:os");
const path = require("node:path");
const { promisify } = require("node:util");
const { chromium } = require("playwright-core");

const exec = promisify(execFile);
const root = path.resolve(__dirname, "../..");
const [serviceImage, ingressImage] = process.argv.slice(2);
assert(
  serviceImage && ingressImage,
  "supply the candidate service and ingress image names",
);

async function availablePort() {
  const server = net.createServer();
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const port = server.address().port;
  await new Promise((resolve) => server.close(resolve));
  return port;
}

async function exercise(browser, file) {
  const name = "metiva-browser-" + randomBytes(6).toString("hex");
  const tmp = await fs.mkdtemp(path.join(os.tmpdir(), name));
  const config = path.join(tmp, "compose.json");
  const data = path.join(tmp, "data");
  const port = await availablePort();
  const origin = "http://localhost:" + port;
  const docker = (...args) =>
    exec("docker", args, {
      cwd: root,
      env: {
        ...process.env,
        METIVA_PUBLIC_URL: origin,
        METIVA_DATA_DIR: data,
        METIVA_UID: String(process.getuid()),
        METIVA_GID: String(process.getgid()),
      },
    });
  const compose = (...args) =>
    docker("compose", "-p", name, "-f", config, ...args);
  const up = () =>
    compose(
      "up",
      "-d",
      "--wait",
      "--wait-timeout",
      "120",
      "--no-build",
      "--pull",
      "never",
    );
  const contexts = [];
  try {
    await fs.mkdir(path.join(data, "artifacts"), { recursive: true });
    const cfg = JSON.parse(
      (
        await docker(
          "compose",
          "--env-file",
          "/dev/null",
          "-p",
          name,
          "-f",
          file,
          "config",
          "--format",
          "json",
        )
      ).stdout,
    );
    // Preserve each deployment's dependency graph and shutdown configuration.
    // Isolate its network, bind mounts, image names and host port from other stacks.
    const subnet = process.env.METIVA_TEST_SUBNET;
    cfg.networks = {
      default: subnet ? { ipam: { config: [{ subnet }] } } : {},
    };
    for (const [key, service] of Object.entries(cfg.services)) {
      delete service.container_name;
      delete service.build;
      service.image = key === "metiva" ? ingressImage : serviceImage;
      service.networks = ["default"];
      service.restart = "no"; // Hold an intentional SIGKILL until recovery is tested.
    }
    cfg.services.metiva.ports = ["127.0.0.1:" + port + ":80"];
    await fs.writeFile(config, JSON.stringify(cfg));
    await up();

    const pages = [];
    const writes = [];
    const reads = [[], []];
    const idleTransferRequests = [];
    const errors = [];
    let storageState;
    for (const [i, route] of ["settings", "transfers/active"].entries()) {
      const context = await browser.newContext({ storageState });
      contexts.push(context);
      if (!storageState) {
        const setup = await context.request.post(origin + "/api/admin/setup", {
          headers: { Origin: origin },
          data: {
            username: "operator",
            password: randomBytes(20).toString("hex"),
            bootstrapSecret: (
              await fs.readFile(path.join(data, "bootstrap-secret"), "utf8")
            ).trim(),
          },
        });
        assert.equal(setup.status(), 200);
        storageState = await context.storageState();
      }
      const page = await context.newPage();
      pages.push(page);
      page.on("pageerror", (error) => errors.push(error.message));
      page.on("request", (r) => {
        const resource = new URL(r.url()).pathname;
        if (
          i === 0 &&
          ["/api/admin/transfers", "/api/admin/events"].includes(resource)
        )
          idleTransferRequests.push(resource);
        if (!["GET", "HEAD"].includes(r.method()))
          writes.push([r.method(), resource]);
      });
      page.on("response", (r) => {
        if (r.status() === 200) reads[i].push(new URL(r.url()).pathname);
      });
      await page.addInitScript(() => {
        // Observe native EventSource delivery in the dashboard, including the
        // final frame on the real Nginx connection and the first visible dialog.
        window.lifecycle = { opens: 0, beats: 0, stops: [], overlays: [] };
        window.EventSource = class extends EventSource {
          constructor(...args) {
            super(...args);
            if (new URL(this.url).pathname !== "/api/admin/lifecycle") return;
            this.addEventListener("open", () => lifecycle.opens++);
            this.addEventListener("heartbeat", () => lifecycle.beats++);
            this.addEventListener("server-stopping", () =>
              lifecycle.stops.push(Date.now()),
            );
          }
        };
        let shown = false;
        new MutationObserver(() => {
          const open = !!document.querySelector(".reconnect-dialog[open]");
          if (open && !shown) lifecycle.overlays.push(Date.now());
          shown = open;
        }).observe(document, {
          subtree: true,
          childList: true,
          attributes: true,
          attributeFilter: ["open"],
        });
      });
      await page.goto(origin + "/#" + route);
      await page.waitForFunction(
        () => document.visibilityState === "visible" && lifecycle.beats > 0,
      );
    }
    const [settings, transfers] = pages;
    const draft = settings.getByLabel("Concurrent transfers");
    const filter = transfers.getByLabel("Search transfers");
    await draft.fill("7");
    await settings.getByRole("button", { name: "Save settings" }).click();
    await settings.getByText("Instance settings saved").waitFor();
    await draft.fill("9");
    await filter.fill("keep-this-filter");
    await Promise.all(
      pages.map((p) =>
        p.waitForFunction(
          () => lifecycle.opens >= 2 && lifecycle.beats >= 16,
          null,
          { timeout: 50000 },
        ),
      ),
    );
    for (const p of pages)
      assert.deepEqual(await p.evaluate(() => lifecycle.overlays), []);
    console.log(file, "healthy 30 s stream renewal in both foreground windows");

    const cases = [
      ["orderly down", () => compose("down"), up],
      ["SIGKILL API", () => compose("kill", "-s", "SIGKILL", "metiva-api"), up],
      [
        "silent ingress pause",
        () => compose("pause", "metiva"),
        () => compose("unpause", "metiva"),
      ],
    ];
    for (const [label, stop, start] of cases) {
      const orderly = label === "orderly down";
      for (const p of pages)
        await p.evaluate(() => {
          lifecycle.stops.length = 0;
          lifecycle.overlays.length = 0;
        });
      // Both windows have a fresh heartbeat when the fault is introduced.
      await draft.focus();
      await filter.focus();
      const lastBeats = await Promise.all(
        pages.map((p) => p.evaluate(() => lifecycle.beats)),
      );
      await Promise.all(
        pages.map((p, i) =>
          p.waitForFunction((last) => lifecycle.beats > last, lastBeats[i]),
        ),
      );
      const started = Date.now();
      await stop();
      const measurements = await Promise.all(
        pages.map(async (p) => {
          await p
            .getByRole("alertdialog", { name: /Reconnecting/ })
            .waitFor({ timeout: 10500 });
          const state = await p.evaluate(() => ({
            ...lifecycle,
            online: navigator.onLine,
          }));
          assert(state.online);
          assert.equal(state.stops.length, orderly ? 1 : 0);
          if (orderly)
            assert(
              state.overlays[0] >= state.stops[0] &&
                state.overlays[0] - state.stops[0] < 500,
              "final event did not promptly show dialog",
            );
          else
            assert(
              state.overlays[0] - started <= 10000,
              "missed foreground deadline",
            );
          return {
            lossToOverlayMs: state.overlays[0] - started,
            ...(orderly && {
              eventToOverlayMs: state.overlays[0] - state.stops[0],
            }),
          };
        }),
      );
      console.log(file, label, JSON.stringify(measurements));
      assert.equal(await draft.inputValue(), "9");
      assert.equal(await filter.inputValue(), "keep-this-filter");
      for (const log of reads) log.length = 0;
      const beatsBeforeRecovery = await Promise.all(
        pages.map((p) => p.evaluate(() => lifecycle.beats)),
      );
      await start();
      await Promise.all(
        pages.map(async (p, i) => {
          await p
            .getByRole("alertdialog")
            .waitFor({ state: "detached", timeout: 60000 });
          await p.waitForFunction(
            (before) => lifecycle.beats > before,
            beatsBeforeRecovery[i],
          );
          assert(reads[i].includes("/api/admin/session"));
          assert(
            reads[i].includes(
              i === 0 ? "/api/admin/settings" : "/api/admin/transfers",
            ),
          );
        }),
      );
      await transfers.getByText("Live updates", { exact: true }).waitFor();
      for (const input of [draft, filter])
        assert(await input.evaluate((el) => el === document.activeElement));
      assert.equal(await draft.inputValue(), "9");
      assert.equal(await filter.inputValue(), "keep-this-filter");
      assert.equal(
        await transfers.getByLabel("Filter by status").inputValue(),
        "active",
      );
    }
    // Model client-side cookie expiry. Both views should follow the ordinary
    // auth path at the next renewal, even with a previously healthy SSE stream.
    for (const p of pages)
      await p.evaluate(() => {
        lifecycle.overlays.length = 0;
      });
    await Promise.all(contexts.map((c) => c.clearCookies()));
    await Promise.all(
      pages.map(async (p) => {
        await p
          .getByRole("heading", { name: "Sign in" })
          .waitFor({ timeout: 45000 });
        assert.deepEqual(await p.evaluate(() => lifecycle.overlays), []);
      }),
    );
    assert.deepEqual(idleTransferRequests, []);
    assert.deepEqual(writes, [["PATCH", "/api/admin/settings"]]);
    assert.deepEqual(errors, []);
  } finally {
    await Promise.all(contexts.map((context) => context.close()));
    try {
      await fs.access(config);
      await compose("unpause", "metiva");
      await compose("down", "--remove-orphans");
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
    }
    await fs.rm(tmp, { recursive: true, force: true });
  }
}

(async () => {
  const browser = await chromium.launch({ executablePath: process.env.CHROME });
  try {
    for (const file of ["compose.yaml", "compose-prod.yaml"])
      await exercise(browser, file);
  } finally {
    await browser.close();
  }
})().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
