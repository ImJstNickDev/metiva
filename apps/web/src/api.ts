import { QueryClient, useQuery, useMutation } from "@tanstack/react-query";
import { useSyncExternalStore } from "react";
import type { Session } from "./model";

export const cache = new QueryClient({
  defaultOptions: {
    queries: {
      retry: false,
      staleTime: 5000,
      networkMode: "always",
      refetchOnReconnect: false,
    },
    // Even while the browser is offline, run the guard now. A paused mutation
    // would otherwise be submitted by React Query when connectivity returns.
    mutations: { retry: false, gcTime: 0, networkMode: "always" },
  },
});

let phase: "online" | "offline" | "refreshing" = "online";
const listeners = new Set<() => void>();
function setPhase(next: typeof phase) {
  if (next === phase) return;
  phase = next;
  listeners.forEach((listener) => listener());
}
export const isDisconnected = () => phase !== "online";
function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}
export function useDisconnected() {
  return useSyncExternalStore(subscribe, isDisconnected);
}
export function connectionLost() {
  if (phase === "offline") return;
  setPhase("offline");
  // Roll back in-flight reads so a late response cannot replace the snapshot.
  void cache.cancelQueries();
}
export class ConnectionError extends Error {
  constructor(mutation = false) {
    super(
      mutation
        ? "Connection lost. Check the result before submitting again."
        : "Server unavailable.",
    );
  }
}

function unavailable(mutation = false): never {
  connectionLost();
  throw new ConnectionError(mutation);
}

function parseSession(body: string): Session {
  let session: Partial<Session> | null;
  try {
    session = JSON.parse(body);
  } catch {
    return unavailable();
  }
  if (
    typeof session?.authenticated !== "boolean" ||
    typeof session.setupRequired !== "boolean" ||
    (session.authenticated &&
      (typeof session.username !== "string" ||
        typeof session.csrfToken !== "string"))
  )
    return unavailable();
  return session as Session;
}

function clearPrivateData() {
  cache.removeQueries({ predicate: (q) => q.queryKey[0] !== "session" });
  cache.getMutationCache().clear();
}

async function request<T>(
  path: string,
  method: string,
  payload?: unknown,
  signal?: AbortSignal,
): Promise<T> {
  const headers = new Headers({ Accept: "application/json" });
  if (payload !== undefined) headers.set("Content-Type", "application/json");
  const previous = cache.getQueryData<Session>(["session"]);
  const mutation = method !== "GET";
  if (previous?.csrfToken && mutation)
    headers.set("X-CSRF-Token", previous.csrfToken);
  const timeout = mutation
    ? undefined
    : AbortSignal.timeout(path === "session" ? 8000 : 30000);
  let response: Response, body: string;
  try {
    response = await fetch("/api/admin/" + path, {
      method,
      headers,
      credentials: "same-origin",
      cache: "no-store",
      signal:
        signal && timeout
          ? AbortSignal.any([signal, timeout])
          : (timeout ?? signal),
      body: payload === undefined ? undefined : JSON.stringify(payload),
    });
    body = await response.text();
  } catch (error) {
    if (signal?.aborted) throw error;
    return unavailable(mutation);
  }
  if (!response.ok) {
    let message: string | undefined;
    try {
      message = JSON.parse(body)?.error;
    } catch {
      /* Gateway error pages can be HTML. */
    }
    if (
      (path === "session" && response.status >= 500) ||
      ([502, 503, 504].includes(response.status) && (!mutation || !message))
    ) {
      return unavailable(mutation);
    }
    if (response.status === 401 && path !== "session" && path !== "login") {
      if (phase === "refreshing") connectionLost();
      else if (!isDisconnected())
        void cache.invalidateQueries({ queryKey: ["session"] });
    }
    throw new Error(message || `Request failed (HTTP ${response.status})`);
  }
  if (path === "session") {
    const session = parseSession(body);
    if (
      previous?.authenticated &&
      (!session.authenticated || previous.username !== session.username)
    )
      clearPrivateData();
    return session as T;
  }
  return (body ? JSON.parse(body) : undefined) as T;
}

export async function api<T>(
  path: string,
  method = "GET",
  payload?: unknown,
  signal?: AbortSignal,
): Promise<T> {
  if (phase === "offline" || (phase === "refreshing" && method !== "GET"))
    throw new ConnectionError();
  return request<T>(path, method, payload, signal);
}

export async function recoverConnection(signal: AbortSignal) {
  const session = await request<Session>("session", "GET", undefined, signal);
  if (signal.aborted) return;
  cache.setQueryData(["session"], session);
  if (session.authenticated) {
    setPhase("refreshing");
    await cache.invalidateQueries({
      predicate: (q) => q.queryKey[0] !== "session",
    });
    // A read may have encountered another outage while refreshing.
    if (phase === "offline") throw new ConnectionError();
  }
  if (!signal.aborted) setPhase("online");
}

export function useResource<T>(
  path: string,
  options: { enabled?: boolean; refetchInterval?: number } = {},
) {
  const disconnected = useDisconnected();
  return useQuery({
    queryKey: [path],
    queryFn: ({ signal }) => api<T>(path, "GET", undefined, signal),
    ...options,
    refetchInterval: disconnected ? false : options.refetchInterval,
    refetchOnWindowFocus: !disconnected,
  });
}
export function useAction<T>(
  fn: (data: FormData) => Promise<T>,
  onSuccess?: (result: T) => void,
) {
  return useMutation({
    mutationFn: fn,
    onSuccess: async (result) => {
      await cache.invalidateQueries({
        predicate: (q) => q.queryKey[0] !== "session",
      });
      onSuccess?.(result);
    },
  });
}
export function signedOut() {
  cache.setQueryData<Session>(["session"], {
    authenticated: false,
    setupRequired: false,
  });
  clearPrivateData();
  void cache.invalidateQueries({ queryKey: ["session"] });
}
