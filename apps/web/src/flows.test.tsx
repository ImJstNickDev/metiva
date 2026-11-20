// @vitest-environment jsdom
import { afterEach, beforeAll, expect, it, vi } from "vitest";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { QueryClientProvider } from "@tanstack/react-query";
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
