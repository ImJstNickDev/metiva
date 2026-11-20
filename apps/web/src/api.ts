import { QueryClient, useQuery, useMutation } from "@tanstack/react-query";
import type { Session } from "./model";

export const cache = new QueryClient({
  defaultOptions: {
    queries: { retry: false, staleTime: 5000 },
    mutations: { retry: false, gcTime: 0 },
  },
});
export async function api<T>(
  path: string,
  method = "GET",
  payload?: unknown,
): Promise<T> {
  const headers = new Headers({ Accept: "application/json" });
  if (payload !== undefined) headers.set("Content-Type", "application/json");
  const csrf = cache.getQueryData<Session>(["session"])?.csrfToken;
  if (csrf && method !== "GET") headers.set("X-CSRF-Token", csrf);
  const response = await fetch("/api/admin/" + path, {
    method,
    headers,
    credentials: "same-origin",
    cache: "no-store",
    body: payload === undefined ? undefined : JSON.stringify(payload),
  });
  if (!response.ok) {
    if (response.status === 401 && path !== "session" && path !== "login")
      void cache.invalidateQueries({ queryKey: ["session"] });
    const data = await response.json().catch(() => null);
    throw new Error(data?.error || `Request failed (HTTP ${response.status})`);
  }
  const text = await response.text();
  return text ? (JSON.parse(text) as T) : (undefined as T);
}
export function useResource<T>(
  path: string,
  options: { enabled?: boolean; refetchInterval?: number } = {},
) {
  return useQuery({
    queryKey: [path],
    queryFn: () => api<T>(path),
    ...options,
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
  cache.removeQueries({ predicate: (q) => q.queryKey[0] !== "session" });
  cache.getMutationCache().clear();
  void cache.invalidateQueries({ queryKey: ["session"] });
}
