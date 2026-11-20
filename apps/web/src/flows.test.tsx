// @vitest-environment jsdom
import { afterEach, beforeAll, expect, it, vi } from "vitest";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { QueryClientProvider } from "@tanstack/react-query";
import { Archive } from "./Archive";
import { Projects } from "./Projects";
import { ImportSource } from "./SourceImport";
import { Auth } from "./Auth";
import { Keys } from "./Keys";
import { RuntimeView } from "./Runtime";
import { api, cache } from "./api";
import type { ReactNode } from "react";
import type { Project } from "./model";

beforeAll(() => {
  Object.defineProperty(HTMLDialogElement.prototype, "showModal", {
    value: function (this: HTMLDialogElement) {
      this.setAttribute("open", "");
    },
  });
  Object.defineProperty(HTMLDialogElement.prototype, "close", {
    value: function (this: HTMLDialogElement) {
      this.removeAttribute("open");
    },
  });
});
afterEach(() => {
  cleanup();
  cache.clear();
  vi.unstubAllGlobals();
});
function mount(node: ReactNode) {
  return render(
    <QueryClientProvider client={cache}>{node}</QueryClientProvider>,
  );
}
const ok = (data: unknown) =>
  Promise.resolve(
    new Response(JSON.stringify(data), {
      headers: { "Content-Type": "application/json" },
    }),
  );
const project: Project = {
  id: 1,
  name: "Builds",
  slug: "builds",
  description: "",
  visibility: "private",
  runtime_count: 1,
  last_success: null,
  status: "idle",
};

it("gets the CSRF token from the current session on each mutation", async () => {
  const request = vi.fn(() => ok({}));
  vi.stubGlobal("fetch", request);
  cache.setQueryData(["session"], { csrfToken: "first" });
  await api("projects", "POST", {});
  cache.setQueryData(["session"], { csrfToken: "next" });
  await api("keys/7", "DELETE");
  const calls = request.mock.calls as unknown as [string, RequestInit][];
  expect(new Headers(calls[0][1].headers).get("X-CSRF-Token")).toBe("first");
  expect(new Headers(calls[1][1].headers).get("X-CSRF-Token")).toBe("next");
});
it("requires matching passwords before administrator setup", async () => {
  const request = vi.fn();
  vi.stubGlobal("fetch", request);
  mount(<Auth setup />);
  fireEvent.change(screen.getByLabelText("Bootstrap secret"), {
    target: { value: "bootstrap" },
  });
  fireEvent.change(screen.getByLabelText("Username"), {
    target: { value: "admin" },
  });
  fireEvent.change(screen.getByLabelText("Password", { exact: true }), {
    target: { value: "a-long-test-password" },
  });
  fireEvent.change(screen.getByLabelText("Confirm password"), {
    target: { value: "different-password" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Create administrator" }));
  expect((await screen.findByRole("alert")).textContent).toContain(
    "Passwords do not match",
  );
  expect(request).not.toHaveBeenCalled();
});
it("displays an issued key once and clears the token when dismissed", async () => {
  let created = false;
  const key = {
    id: 1,
    prefix: "example",
    name: "Worker",
    scope: "global",
    createdAt: 42,
    revokedAt: null,
    lastUsedAt: null,
  };
  vi.stubGlobal(
    "fetch",
    vi.fn((url: string, opts: RequestInit) => {
      if (url.endsWith("projects")) return ok([]);
      if (opts.method === "POST") {
        created = true;
        return ok({ ...key, token: "visible-only-on-issue" });
      }
      return ok(created ? [key] : []);
    }),
  );
  mount(<Keys />);
  fireEvent.click(screen.getByRole("button", { name: "Create key" }));
  fireEvent.change(screen.getByLabelText("Key name"), {
    target: { value: "Worker" },
  });
  fireEvent.change(screen.getByLabelText("Scope"), {
    target: { value: "global" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Create access key" }));
  await screen.findByText("visible-only-on-issue");
  fireEvent.click(screen.getByRole("button", { name: "Done" }));
  await waitFor(() =>
    expect(screen.queryByText("visible-only-on-issue")).toBeNull(),
  );
  expect(screen.getByText("Worker")).toBeTruthy();
});
it("previews the source before enabling import and sends the reviewed input", async () => {
  const calls: { url: string; body?: string }[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn((url: string, opts: RequestInit) => {
      calls.push({ url, body: opts.body as string });
      if (url.endsWith("/preview"))
        return ok({
          releases: [
            {
              version: "1.0",
              artifacts: [
                { filename: "package.zip", url: "https://example.org/file" },
              ],
            },
          ],
        });
      if (url.endsWith("/sources")) return ok({ id: 12 });
      if (url.endsWith("/settings"))
        return ok({ public_url: "https://example.org" });
      return ok({
        id: 9,
        slug: "linux",
        project_slug: "builds",
        name: "Linux",
        sync_interval: 60,
        status: "idle",
        sources: [],
        history: [],
        manifest: null,
      });
    }),
  );
  mount(<RuntimeView id={9} project={project} />);
  fireEvent.click(await screen.findByRole("button", { name: "Import source" }));
  fireEvent.change(screen.getByLabelText("Manifest URL"), {
    target: { value: "https://example.org/index.json" },
  });
  fireEvent.change(screen.getByLabelText("File selection"), {
    target: { value: "auto" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Preview manifest" }));
  await screen.findByRole("heading", { name: "Review source" });
  expect(calls.some((c) => c.url.endsWith("/sources"))).toBe(false);
  const dialog = screen.getByRole("dialog");
  fireEvent.click(
    [...dialog.querySelectorAll("button")].find(
      (b) => b.textContent === "Import source",
    )!,
  );
  await waitFor(() =>
    expect(calls.some((c) => c.url.endsWith("/sources"))).toBe(true),
  );
  expect(calls.find((c) => c.url.endsWith("/sources"))?.body).toBe(
    calls.find((c) => c.url.endsWith("/preview"))?.body,
  );
});

const index = {
  path: "/versions",
  link_path: "/releases",
  keys: ["alpha", "beta", "snapshot"],
};
const collection = {
  path: "/items",
  version_path: "/version",
  groups: [
    { path: "/server", keys: ["linux-x64", "macos-arm64"] },
    { path: "/client", keys: ["linux-x64"] },
  ],
};
const url = "https://example.org/catalog.json";
const reviewed = {
  releases: [
    {
      version: "2.0",
      channel: "alpha",
      artifacts: [
        { filename: "alpha.tar.gz", size: 1024, url: "https://example.org/a" },
      ],
    },
    {
      version: "1.0",
      channel: "snapshot",
      artifacts: [
        { filename: "snapshot.zip", size: 2048, url: "https://example.org/b" },
      ],
    },
  ],
};
it("saves the root URL and only the reviewed historical channels and platform", async () => {
  const calls: { path: string; data: any }[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn((path: string, opts: RequestInit) => {
      const data = JSON.parse(String(opts.body));
      calls.push({ path, data });
      if (path.endsWith("/discover"))
        return ok(
          data.index
            ? { indexes: [], collections: [collection] }
            : { indexes: [index], collections: [collection] },
        );
      if (path.endsWith("/preview")) return ok(reviewed);
      return ok({ id: 10 });
    }),
  );
  mount(<ImportSource id={9} onClose={() => {}} />);
  fireEvent.change(screen.getByLabelText("Manifest URL"), {
    target: { value: url },
  });
  fireEvent.click(screen.getByRole("button", { name: "Inspect manifest" }));
  fireEvent.change(await screen.findByLabelText("Release inventory"), {
    target: { value: "index-0" },
  });
  expect(
    (
      screen.getByRole("button", {
        name: "Inspect linked manifests",
      }) as HTMLButtonElement
    ).disabled,
  ).toBe(true);
  fireEvent.click(screen.getByRole("checkbox", { name: "alpha" }));
  fireEvent.click(screen.getByRole("checkbox", { name: "snapshot" }));
  fireEvent.click(
    screen.getByRole("button", { name: "Inspect linked manifests" }),
  );
  fireEvent.click(
    within(await screen.findByRole("group", { name: "/server" })).getByRole(
      "checkbox",
      { name: "linux-x64" },
    ),
  );
  fireEvent.click(
    screen.getByRole("button", { name: "Preview selected files" }),
  );
  await screen.findByRole("heading", { name: "Review source" });
  expect(screen.getByText("2 releases · 2 files · 3 KiB total")).toBeTruthy();
  expect(screen.getByText("alpha.tar.gz")).toBeTruthy();
  expect(screen.getByText("snapshot.zip")).toBeTruthy();
  expect(screen.getByText("2 KiB")).toBeTruthy();
  expect(calls.some((c) => c.path.endsWith("/sources"))).toBe(false);
  const rule = {
    index: { ...index, keys: ["alpha", "snapshot"] },
    path: "/items",
    version_path: "/version",
    groups: [{ path: "/server", keys: ["linux-x64"] }],
  };
  expect(calls.find((c) => c.path.endsWith("/preview"))?.data).toEqual({
    url,
    mapping: { selection: rule },
  });
  fireEvent.click(screen.getByRole("button", { name: "Change selection" }));
  expect(
    (
      within(screen.getByRole("group", { name: "/server" })).getByLabelText(
        "linux-x64",
      ) as HTMLInputElement
    ).checked,
  ).toBe(true);
  fireEvent.click(
    screen.getByRole("button", { name: "Preview selected files" }),
  );
  fireEvent.click(await screen.findByRole("button", { name: "Import source" }));
  await waitFor(() =>
    expect(calls.find((c) => c.path.endsWith("/sources"))?.data).toEqual({
      url,
      mapping: { selection: rule },
    }),
  );
});
it("edits a saved direct-list rule and sends its selected channel and artifact keys", async () => {
  const direct = {
    ...collection,
    channels: { path: "/channel", keys: ["alpha", "beta"] },
  };
  const selection = {
    ...direct,
    channels: { ...direct.channels, keys: ["alpha"] },
    groups: [{ path: "/client", keys: ["linux-x64"] }],
  };
  const request = vi.fn((path: string) =>
    ok(
      path.endsWith("/discover")
        ? { indexes: [], collections: [direct] }
        : path.endsWith("/preview")
          ? reviewed
          : { id: 3 },
    ),
  );
  vi.stubGlobal("fetch", request);
  mount(
    <ImportSource
      id={9}
      source={{
        id: 3,
        kind: "url",
        url,
        mapping: JSON.stringify({ selection }),
        status: "ok",
        last_checked: null,
        last_success: null,
      }}
      onClose={() => {}}
    />,
  );
  fireEvent.click(screen.getByRole("button", { name: "Inspect manifest" }));
  const alpha = (await screen.findByLabelText("alpha")) as HTMLInputElement;
  expect(alpha.checked).toBe(true);
  expect(
    (
      within(screen.getByRole("group", { name: "/client" })).getByLabelText(
        "linux-x64",
      ) as HTMLInputElement
    ).checked,
  ).toBe(true);
  fireEvent.click(alpha);
  expect(
    (
      screen.getByRole("button", {
        name: "Preview selected files",
      }) as HTMLButtonElement
    ).disabled,
  ).toBe(true);
  fireEvent.click(screen.getByLabelText("beta"));
  fireEvent.click(
    screen.getByRole("button", { name: "Preview selected files" }),
  );
  fireEvent.click(await screen.findByRole("button", { name: "Save source" }));
  await waitFor(() =>
    expect(request).toHaveBeenCalledWith(
      "/api/admin/sources/3",
      expect.objectContaining({
        method: "PATCH",
        body: JSON.stringify({
          url,
          mapping: {
            selection: {
              path: selection.path,
              version_path: selection.version_path,
              channels: { path: "/channel", keys: ["beta"] },
              groups: selection.groups,
            },
          },
        }),
      }),
    ),
  );
});
const runtimeData = {
  id: 9,
  slug: "linux",
  project_slug: "builds",
  name: "Linux",
  visibility: "inherit",
  sync_interval: 60,
  status: "idle",
  sources: [],
  history: [],
  manifest: null,
  project_parked_at: null,
};
it("confirms runtime parking, inspects its retained revisions and downloads old bytes", async () => {
  let parked: number | null = null;
  const file = {
    id: 5,
    filename: "pkg.tgz",
    release: "v1",
    size: 4,
    digest: "new-digest",
    source_url: url,
  };
  const request = vi.fn((path: string) => {
    if (path.endsWith("/park")) {
      parked = 100;
      return ok({ id: 9, parked_at: parked });
    }
    if (path.endsWith("/settings")) return ok({});
    if (path.includes("/history")) {
      const current = !path.includes("before");
      return ok({
        revisions: [
          {
            id: current ? 5 : 3,
            created_at: 89,
            digest: current ? "new-digest" : "older-digest",
            size: 4,
            source_url: url,
            job_id: 12,
            current,
            download_url: current
              ? "/api/admin/files/5/download"
              : "/api/admin/files/3/download",
          },
        ],
        next_before: current ? 5 : null,
      });
    }
    if (path.includes("past-files"))
      return ok({
        files: [{ ...file, id: 23, filename: "retired.zip", created_at: 25 }],
        next_before: null,
      });
    return ok({
      ...runtimeData,
      parked_at: parked,
      current_files: [file],
      has_past_files: true,
    });
  });
  vi.stubGlobal("fetch", request);
  mount(<RuntimeView id={9} project={project} />);
  fireEvent.click(await screen.findByRole("button", { name: "Park runtime" }));
  const dialog = screen.getByRole("dialog");
  expect(parked).toBeNull();
  fireEvent.click(within(dialog).getByRole("button", { name: "Park runtime" }));
  await screen.findByRole("button", { name: "Restore runtime" });
  expect(screen.queryByRole("button", { name: "Sync now" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Publication" }));
  fireEvent.click(screen.getByRole("button", { name: "pkg.tgz" }));
  await screen.findByText("Current", { exact: true });
  fireEvent.click(screen.getByRole("button", { name: "Load older revisions" }));
  await screen.findByText("older-digest");
  expect(
    screen
      .getByRole("link", { name: "Download pkg.tgz revision 3" })
      .getAttribute("href"),
  ).toBe("/api/admin/files/3/download");
  fireEvent.click(screen.getByRole("button", { name: "Close dialog" }));
  fireEvent.click(screen.getByRole("button", { name: "Past files" }));
  expect(
    (
      await screen.findByRole("link", { name: "Download retired.zip" })
    ).getAttribute("href"),
  ).toBe("/api/admin/archive/files/23");
});
it("opens parked projects by ID and allows confirmation before restoration in the archive", async () => {
  let parked = true;
  const request = vi.fn((path: string) => {
    if (path.endsWith("/restore")) {
      parked = false;
      return ok({ id: 1, parked_at: null });
    }
    if (path.endsWith("/archive"))
      return ok(
        parked
          ? [
              {
                kind: "project",
                id: 1,
                name: "Builds",
                slug: "builds",
                parent_parked: false,
                parked_at: 1,
              },
            ]
          : [],
      );
    if (path.endsWith("/runtimes")) return ok([{ ...runtimeData }]);
    if (path.endsWith("projects/1"))
      return ok({ ...project, parked_at: parked ? 1 : null });
    throw new Error("Unexpected request " + path);
  });
  vi.stubGlobal("fetch", request);
  const view = mount(<Projects route={["projects", "1"]} />);
  expect(
    (await screen.findByRole("link", { name: "Linux linux" })).getAttribute(
      "href",
    ),
  ).toBe("#projects/1/runtimes/9");
  expect(screen.queryByRole("button", { name: "New runtime" })).toBeNull();
  view.unmount();
  mount(<Archive />);
  expect(
    (await screen.findByRole("link", { name: "Builds builds" })).getAttribute(
      "href",
    ),
  ).toBe("#projects/1");
  fireEvent.click(screen.getByRole("button", { name: "Restore project" }));
  fireEvent.click(
    within(screen.getByRole("dialog")).getByRole("button", { name: "Cancel" }),
  );
  expect(parked).toBe(true);
  fireEvent.click(screen.getByRole("button", { name: "Restore project" }));
  fireEvent.click(
    within(screen.getByRole("dialog")).getByRole("button", {
      name: "Restore project",
    }),
  );
  expect(await screen.findByText("No parked resources")).toBeTruthy();
});

it("loads earlier publication metadata and its original source documents on demand", async () => {
  vi.stubGlobal(
    "fetch",
    vi.fn((path: string) => {
      if (path.endsWith("settings")) return ok({});
      if (path.endsWith("publications"))
        return ok({
          publications: [{ id: 12, created_at: 101, current: true }],
          next_before: 12,
        });
      if (path.endsWith("publications?before=12"))
        return ok({
          publications: [{ id: 10, created_at: 90, current: false }],
          next_before: null,
        });
      if (path.endsWith("publications/12"))
        return ok({ manifest: reviewed, originals: [] });
      if (path.endsWith("publications/10"))
        return ok({
          manifest: { releases: [] },
          originals: [
            {
              id: 34,
              source_id: 3,
              page: 1,
              size: 10,
              url: "https://example.org/old.json",
              digest: "original-digest",
            },
          ],
        });
      return ok({ ...runtimeData, active_publication: 12 });
    }),
  );
  mount(<RuntimeView id={9} project={project} />);
  fireEvent.click(await screen.findByRole("button", { name: "Sync history" }));
  fireEvent.click(screen.getByRole("button", { name: "Publication history" }));
  fireEvent.click(
    await screen.findByRole("button", { name: "Load older publications" }),
  );
  await screen.findByRole("option", { name: /#10/ });
  fireEvent.change(screen.getByLabelText("Publication"), {
    target: { value: "10" },
  });
  await screen.findByText("original-digest");
  expect(screen.getByRole("link", { name: "10 B" }).getAttribute("href")).toBe(
    "/api/admin/originals/34",
  );
  expect(screen.getByText("Published manifest JSON")).toBeTruthy();
});
