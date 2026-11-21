// @vitest-environment jsdom
import { afterEach, beforeAll, expect, it, vi } from "vitest";
import { act, cleanup, render, screen } from "@testing-library/react";
import { onlineManager, QueryClientProvider } from "@tanstack/react-query";
import {
  api,
  cache,
  connectionLost,
  ConnectionError,
  isDisconnected,
  recoverConnection,
  useResource,
} from "./api";
import {
  DashboardConnection,
  useDashboardLiveness,
} from "./DashboardConnection";
import type { Session } from "./model";
import { Data } from "./ui";

const session = {
  authenticated: true,
  setupRequired: false,
  username: "operator",
  csrfToken: "csrf",
};
const ok = (value: unknown) => new Response(JSON.stringify(value));

function AuthenticatedView() {
  useDashboardLiveness();
  return <input aria-label="Draft" />;
}

function TestDashboard() {
  const session = useResource<Session>("session");
  return (
    <>
      {session.data?.authenticated ? <AuthenticatedView /> : <p>Sign in</p>}
      <DashboardConnection />
    </>
  );
}

function openDashboard() {
  vi.useFakeTimers();
  const streams: FakeStream[] = [];
  class FakeStream extends EventTarget {
    close = vi.fn();
    constructor(readonly url: string) {
      super();
      streams.push(this);
    }
  }
  vi.stubGlobal("EventSource", FakeStream);
  const request = vi.fn(async (_path: string, _options: RequestInit) =>
    ok(session),
  );
  vi.stubGlobal("fetch", request);
  cache.setQueryData(["session"], session);
  render(
    <QueryClientProvider client={cache}>
      <TestDashboard />
    </QueryClientProvider>,
  );
  expect(streams.map((s) => s.url)).toEqual(["/api/admin/lifecycle"]);
  return { streams, stream: streams[0], request };
}

beforeAll(() => {
  HTMLDialogElement.prototype.showModal = function () {
    this.open = true;
  };
  HTMLDialogElement.prototype.close = function () {
    this.open = false;
  };
});
afterEach(async () => {
  cleanup();
  vi.useRealTimers();
  vi.stubGlobal("fetch", async () =>
    ok({ authenticated: false, setupRequired: false }),
  );
  if (isDisconnected()) await recoverConnection(new AbortController().signal);
  onlineManager.setOnline(true);
  cache.clear();
  vi.unstubAllGlobals();
});

it("pauses immediately on graceful stop and resumes the same view after revalidation", async () => {
  const { stream, streams, request } = openDashboard();
  const input = screen.getByRole<HTMLInputElement>("textbox", {
    name: "Draft",
  });
  input.value = "unfinished";
  act(() => stream.dispatchEvent(new Event("server-stopping")));
  expect(
    screen.getByRole("alertdialog", { name: /Reconnecting/ }),
  ).toBeTruthy();
  expect(stream.close).toHaveBeenCalledOnce();
  expect(request).not.toHaveBeenCalled();
  await expect(api("settings", "PATCH", {})).rejects.toBeInstanceOf(
    ConnectionError,
  );
  await act(() => vi.advanceTimersByTimeAsync(1000));
  expect(screen.queryByRole("alertdialog")).toBeNull();
  expect(screen.getByLabelText("Draft")).toBe(input);
  expect(input.value).toBe("unfinished");
  expect(streams).toHaveLength(2);
  expect(
    request.mock.calls.map(([path, options]) => [path, options.method]),
  ).toEqual([["/api/admin/session", "GET"]]);
});

it("detects a silent stream within eight seconds of its last heartbeat", async () => {
  const { stream, request } = openDashboard();
  for (let i = 0; i < 15; i++) {
    await act(() => vi.advanceTimersByTimeAsync(2000));
    act(() => stream.dispatchEvent(new Event("heartbeat")));
  }
  expect(request).not.toHaveBeenCalled();
  // Opening or reconnecting a socket cannot substitute for a fresh heartbeat.
  await act(() => vi.advanceTimersByTimeAsync(7999));
  act(() => stream.dispatchEvent(new Event("open")));
  expect(screen.queryByRole("alertdialog")).toBeNull();
  await act(() => vi.advanceTimersByTimeAsync(1));
  expect(screen.getByRole("alertdialog")).toBeTruthy();
});

it("revalidates stream renewal and expired sessions without showing an outage", async () => {
  const { stream, request } = openDashboard();
  act(() => stream.dispatchEvent(new Event("error")));
  await act(() => vi.advanceTimersByTimeAsync(1000));
  act(() => stream.dispatchEvent(new Event("heartbeat")));
  expect(request).toHaveBeenCalledTimes(1);
  expect(screen.queryByRole("alertdialog")).toBeNull();
  cache.setQueryData(["settings"], { private: true });
  request.mockImplementation(async () =>
    ok({ authenticated: false, setupRequired: false }),
  );
  act(() => stream.dispatchEvent(new Event("error")));
  await act(() => vi.advanceTimersByTimeAsync(1000));
  expect(screen.getByText("Sign in")).toBeTruthy();
  expect(stream.close).toHaveBeenCalledOnce();
  expect(cache.getQueryData(["settings"])).toBeUndefined();
  await act(() => vi.advanceTimersByTimeAsync(10000));
  expect(screen.queryByRole("alertdialog")).toBeNull();
});

it("checks promptly when a suspended dashboard becomes visible", async () => {
  const { request } = openDashboard();
  vi.setSystemTime(Date.now() + 60000);
  request.mockImplementation(async () => new Response("", { status: 502 }));
  await act(async () => {
    document.dispatchEvent(new Event("visibilitychange"));
  });
  expect(request).toHaveBeenCalled();
  expect(screen.getByRole("alertdialog")).toBeTruthy();
  request.mockImplementation(async () => ok(session));
  await act(async () => {
    document.dispatchEvent(new Event("visibilitychange"));
  });
  expect(screen.queryByRole("alertdialog")).toBeNull();
});

it("keeps content mounted after a connection failure", () => {
  const query = {
    isPending: false,
    error: null as Error | null,
    data: "snapshot",
    refetch: vi.fn(),
  };
  const view = () => (
    <Data query={query}>{(data) => <input aria-label={data} />}</Data>
  );
  const component = render(view());
  const input = screen.getByLabelText("snapshot") as HTMLInputElement;
  input.value = "draft";
  query.error = new ConnectionError();
  component.rerender(view());
  expect(screen.getByLabelText("snapshot")).toBe(input);
  expect(input.value).toBe("draft");
  expect(screen.queryByText("Try again")).toBeNull();
});

it("probes the session with bounded backoff, then drops expired private data", async () => {
  vi.useFakeTimers();
  cache.setQueryData(["session"], session);
  cache.setQueryData(["transfers"], ["private"]);
  let reachable = false;
  const request = vi.fn(async () =>
    reachable
      ? ok({ authenticated: false, setupRequired: false })
      : new Response("Unavailable", { status: 502 }),
  );
  vi.stubGlobal("fetch", request);
  render(<DashboardConnection />);
  act(connectionLost);
  expect(screen.getByRole("alertdialog").textContent).toContain("Reconnecting");
  for (const [i, delay] of [
    1000, 2000, 4000, 8000, 16000, 30000, 30000,
  ].entries()) {
    await act(() => vi.advanceTimersByTimeAsync(delay - 1));
    expect(request).toHaveBeenCalledTimes(i);
    await act(() => vi.advanceTimersByTimeAsync(1));
    expect(request).toHaveBeenCalledTimes(i + 1);
    expect(cache.getQueryData(["transfers"])).toEqual(["private"]);
  }
  reachable = true;
  await act(() => vi.advanceTimersByTimeAsync(30000));
  expect(screen.queryByRole("alertdialog")).toBeNull();
  expect(cache.getQueryData(["session"])).toMatchObject({
    authenticated: false,
  });
  expect(cache.getQueryData(["transfers"])).toBeUndefined();
  for (const args of request.mock.calls as unknown as [string, RequestInit][]) {
    expect(args[0]).toBe("/api/admin/session");
    expect(args[1].method).toBe("GET");
  }
});

it("does not queue or replay a mutation after a lost response or while offline", async () => {
  const request = vi.fn(async (_path: string, options: RequestInit) => {
    if (options.method === "POST") throw new TypeError("failed to fetch");
    return ok(session);
  });
  vi.stubGlobal("fetch", request);
  const mutate = () =>
    cache
      .getMutationCache()
      .build(cache, {
        mutationFn: () => api("projects", "POST", {}),
      })
      .execute(undefined);
  await expect(mutate()).rejects.toThrow(
    "Check the result before submitting again",
  );
  onlineManager.setOnline(false);
  await expect(mutate()).rejects.toBeInstanceOf(ConnectionError);
  onlineManager.setOnline(true);
  await recoverConnection(new AbortController().signal);
  await cache.resumePausedMutations();
  expect(request.mock.calls.map(([, options]) => options.method)).toEqual([
    "POST",
    "GET",
  ]);
});

it("keeps application errors and cancelled reads out of the reconnect loop", async () => {
  vi.stubGlobal(
    "fetch",
    async () =>
      new Response(JSON.stringify({ error: "Bootstrap file unavailable" }), {
        status: 503,
      }),
  );
  await expect(api("setup", "POST", {})).rejects.toThrow(
    "Bootstrap file unavailable",
  );
  const controller = new AbortController();
  controller.abort();
  vi.stubGlobal("fetch", async () => {
    throw controller.signal.reason;
  });
  await expect(
    api("transfers", "GET", undefined, controller.signal),
  ).rejects.toThrow();
  expect(isDisconnected()).toBe(false);
});

it.each([
  [502, JSON.stringify({ error: "upstream unavailable" })],
  [503, JSON.stringify({ error: "maintenance" })],
  [504, JSON.stringify({ error: "timeout" })],
  [200, "<html>Maintenance</html>"],
  [200, JSON.stringify({})],
  [200, JSON.stringify({ authenticated: true, setupRequired: false })],
])(
  "keeps the session and stale data on an invalid session response (%s, %s)",
  async (status, body) => {
    cache.setQueryData(["session"], session);
    cache.setQueryData(["transfers"], ["private"]);
    vi.stubGlobal(
      "fetch",
      async () => new Response(body as string, { status: status as number }),
    );
    await expect(api("session")).rejects.toBeInstanceOf(ConnectionError);
    await expect(
      recoverConnection(new AbortController().signal),
    ).rejects.toBeInstanceOf(ConnectionError);
    expect(isDisconnected()).toBe(true);
    expect(cache.getQueryData(["session"])).toEqual(session);
    expect(cache.getQueryData(["transfers"])).toEqual(["private"]);
  },
);
