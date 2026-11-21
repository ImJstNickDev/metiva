import { useEffect, useLayoutEffect, useRef } from "react";
import { Radio } from "lucide-react";
import {
  cache,
  connectionLost,
  recoverConnection,
  useDisconnected,
} from "./api";

// The API emits a heartbeat every two seconds and renews the stream every 30.
const heartbeatDeadline = 8000;

export function useDashboardLiveness() {
  const disconnected = useDisconnected();
  useEffect(() => {
    if (disconnected || typeof EventSource === "undefined") return;
    const stream = new EventSource("/api/admin/lifecycle");
    let timer: ReturnType<typeof setTimeout>;
    let deadline: number;
    function armWatchdog() {
      clearTimeout(timer);
      deadline = Date.now() + heartbeatDeadline;
      timer = setTimeout(connectionLost, heartbeatDeadline);
    }
    function checkSession() {
      // EventSource hides the HTTP status. Revalidate expiry through the normal
      // session query while a healthy stream renews without an outage dialog.
      void cache.refetchQueries(
        { queryKey: ["session"] },
        { cancelRefetch: false },
      );
    }
    function onVisible() {
      if (document.visibilityState !== "visible") return;
      // A suspended browser may have delayed both events and timers. Give
      // queued heartbeats a chance to arrive and check the session immediately.
      if (Date.now() >= deadline) armWatchdog();
      checkSession();
    }
    armWatchdog();
    stream.addEventListener("heartbeat", armWatchdog);
    stream.addEventListener("server-stopping", connectionLost);
    stream.addEventListener("error", checkSession);
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      stream.close();
      clearTimeout(timer);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [disconnected]);
}

export function DashboardConnection() {
  const disconnected = useDisconnected();
  const dialog = useRef<HTMLDialogElement>(null);

  useEffect(() => {
    window.addEventListener("offline", connectionLost);
    if (!navigator.onLine) connectionLost();
    return () => window.removeEventListener("offline", connectionLost);
  }, []);

  useEffect(() => {
    if (!disconnected) return;
    const controller = new AbortController();
    let delay = 1000;
    let timer: ReturnType<typeof setTimeout>;
    let checking = false;
    async function check() {
      if (checking || controller.signal.aborted) return;
      checking = true;
      clearTimeout(timer);
      try {
        await recoverConnection(controller.signal);
      } catch {
        if (!controller.signal.aborted) {
          delay = Math.min(delay * 2, 30000);
          timer = setTimeout(check, delay);
        }
      } finally {
        checking = false;
      }
    }
    timer = setTimeout(check, delay);
    const onVisible = () => {
      if (document.visibilityState === "visible") void check();
    };
    window.addEventListener("online", check);
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      controller.abort();
      clearTimeout(timer);
      window.removeEventListener("online", check);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [disconnected]);

  useLayoutEffect(() => {
    const el = dialog.current;
    if (!disconnected || !el) return;
    const previousFocus = document.activeElement;
    el.showModal();
    return () => {
      el.close();
      if (previousFocus instanceof HTMLElement && previousFocus.isConnected)
        previousFocus.focus({ preventScroll: true });
    };
  }, [disconnected]);

  if (!disconnected) return null;
  return (
    <dialog
      ref={dialog}
      className="reconnect-dialog"
      role="alertdialog"
      aria-modal="true"
      aria-labelledby="connection-title"
      aria-describedby="connection-description"
      onCancel={(event) => event.preventDefault()}
    >
      <Radio aria-hidden="true" size={22} />
      <h2 id="connection-title">Server unavailable. Reconnecting…</h2>
      <p id="connection-description">
        Updates and actions are paused. Any data shown is from the last
        connection.
      </p>
    </dialog>
  );
}
