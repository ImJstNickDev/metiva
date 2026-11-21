// @vitest-environment jsdom
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { Wiki } from "./Wiki";
import { contractUrl, type Contract } from "./contract";

const path = "/p/{project}/manifest.json";
const ok = {
  description: "Current publication",
  content: {
    "application/json": {
      schema: { $ref: "#/components/schemas/Manifest" },
      example: { project: "kit" },
    },
  },
};
const spec: Contract = {
  info: { title: "Test API", version: "1.7" },
  security: [{}, { bearerKey: [] }],
  components: {
    schemas: {
      Manifest: {
        type: "object",
        properties: { project: { type: "string" } },
        required: ["project"],
      },
    },
  },
  paths: {
    [path]: {
      parameters: [
        {
          name: "project",
          in: "path",
          required: true,
          description: "Project slug",
          example: "a b",
          schema: { type: "string" },
        },
      ],
      get: {
        operationId: "getManifest",
        summary: "Current manifest",
        tags: ["Publications"],
        responses: { "200": ok, "404": { description: "Publication missing" } },
      },
      head: {
        operationId: "headGetManifest",
        summary: "Current manifest headers",
        tags: ["Publications"],
        security: [],
        responses: { "200": { description: "Headers only" } },
      },
    },
  },
};
const response = () => new Response(JSON.stringify(spec));
beforeEach(() => {
  window.history.replaceState(null, "", "/docs/");
  vi.stubGlobal("scrollTo", vi.fn());
  Object.defineProperty(HTMLDialogElement.prototype, "close", {
    configurable: true,
    value() {
      this.removeAttribute("open");
    },
  });
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

it("opens the landing and guides with a single credential-free public contract request", async () => {
  const fetcher = vi.fn(async () => response());
  vi.stubGlobal("fetch", fetcher);
  render(<Wiki />);
  expect(screen.getByRole("heading", { name: "Consumer API" })).toBeDefined();
  fireEvent.click(screen.getByRole("link", { name: "Start here" }));
  await screen.findByRole("heading", { name: "Your first download" });
  expect(
    screen
      .getByRole("link", { name: "Normalized manifest" })
      .getAttribute("href"),
  ).toBe("#/reference/getManifest");
  expect(screen.getByLabelText("List projects").textContent).toContain(
    window.location.origin,
  );
  expect(fetcher).toHaveBeenCalledExactlyOnceWith(
    contractUrl,
    expect.objectContaining({ credentials: "omit" }),
  );
});

it("restores an operation from a deep link and uses its contract for path, examples, headers and method switches", async () => {
  window.history.replaceState(null, "", "/docs/#/reference/getManifest");
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => response()),
  );
  render(<Wiki />);
  await screen.findByRole("heading", { name: "Current manifest" });
  expect(screen.getByLabelText("curl").textContent).toContain(
    "/p/a%20b/manifest.json",
  );
  expect(screen.getByLabelText("200 · application/json").textContent).toContain(
    '"project": "kit"',
  );
  expect(screen.getByText("Response fields")).toBeDefined();
  fireEvent.change(screen.getByLabelText("HTTP status"), {
    target: { value: "404" },
  });
  expect(screen.getByLabelText("404 · response").textContent).toContain("404");
  fireEvent.click(
    within(
      screen.getByRole("navigation", { name: "Request method" }),
    ).getByRole("link", { name: "HEAD" }),
  );
  await screen.findByRole("heading", { name: "Current manifest headers" });
  expect(
    (screen.getByLabelText("HTTP status") as HTMLSelectElement).value,
  ).toBe("200");
  expect(screen.getByLabelText("curl").textContent).toContain("--head");
  expect(
    screen.getByText("This operation is publicly readable."),
  ).toBeDefined();
  expect(document.activeElement).toBe(screen.getByRole("main"));
});

it("recovers a reference deep link after the public contract is temporarily unavailable", async () => {
  window.history.replaceState(null, "", "/docs/#/reference/getManifest");
  const fetcher = vi
    .fn()
    .mockResolvedValueOnce(new Response("", { status: 503 }))
    .mockResolvedValueOnce(response());
  vi.stubGlobal("fetch", fetcher);
  render(<Wiki />);
  fireEvent.click(
    await screen.findByRole("button", { name: "Retry loading reference" }),
  );
  await screen.findByRole("heading", { name: "Current manifest" });
  await waitFor(() => expect(document.title).toContain("Current manifest"));
});
