// @vitest-environment jsdom
import { afterEach, expect, it } from "vitest";
import { act, cleanup, render, screen } from "@testing-library/react";
import { QueryClientProvider } from "@tanstack/react-query";
import type { PropsWithChildren } from "react";
import { cache } from "./api";
import { Overview } from "./Overview";
import { Transfers } from "./Transfers";
import type { Overview as Summary, Transfer } from "./model";

afterEach(() => {
  cleanup();
  cache.clear();
});

function WithCache({ children }: PropsWithChildren) {
  return <QueryClientProvider client={cache}>{children}</QueryClientProvider>;
}

it("links the Overview transfer counters to their status filters", () => {
  cache.setQueryData<Summary>(["overview"], {
    projects: 0,
    runtimes: 0,
    active: 3,
    queued: 1,
    last_success: null,
    attention: 0,
    storage_bytes: 0,
    engine: "native",
    activity: [],
  });
  cache.setQueryData(["projects"], []);
  cache.setQueryData(["settings"], { concurrency: 4 });
  render(<Overview />, { wrapper: WithCache });

  expect(
    screen.getByRole("link", { name: /Active transfers/ }).getAttribute("href"),
  ).toBe("#transfers/active");
  expect(
    screen.getByRole("link", { name: /Queued transfers/ }).getAttribute("href"),
  ).toBe("#transfers/queued");
});

it("keeps the selected status as transfers move between Queued and Active", async () => {
  const transfers: Transfer[] = [
    "queued",
    "connecting",
    "downloading",
    "verifying",
    "completed",
    "retrying",
  ].map((state, index) => ({
    id: index + 1,
    project: "Builds",
    runtime: "Linux",
    filename: state + ".zip",
    host: "example.org",
    backend: "native",
    state,
    bytes: 0,
    total: 1024,
    speed: null,
    retries: 0,
    retry_at: null,
    job_id: 1,
  }));
  cache.setQueryData(["transfers"], transfers);
  const { rerender } = render(<Transfers statusSegment="queued" />, {
    wrapper: WithCache,
  });
  const status = screen.getByRole<HTMLSelectElement>("combobox", {
    name: "Filter by status",
  });
  expect(status.value).toBe("queued");
  expect(screen.getByText("queued.zip")).toBeTruthy();
  expect(screen.queryByText("downloading.zip")).toBeNull();
  expect(screen.queryByText("completed.zip")).toBeNull();

  act(() => {
    cache.setQueryData(["transfers"], [
      { ...transfers[0], state: "downloading" },
      ...transfers.slice(1),
    ]);
  });
  await screen.findByText("No matching transfers");
  expect(status.value).toBe("queued");

  rerender(<Transfers statusSegment="active" />);
  expect(status.value).toBe("active");
  expect(screen.getByText("queued.zip")).toBeTruthy();
  expect(screen.getByText("connecting.zip")).toBeTruthy();
  expect(screen.getByText("downloading.zip")).toBeTruthy();
  expect(screen.getByText("verifying.zip")).toBeTruthy();
  expect(screen.queryByText("completed.zip")).toBeNull();
  expect(screen.queryByText("retrying.zip")).toBeNull();

  act(() => {
    cache.setQueryData(["transfers"], [
      { ...transfers[0], state: "completed" },
      ...transfers.slice(4),
    ]);
  });
  await screen.findByText("No matching transfers");
  expect(status.value).toBe("active");
});
