import { useEffect, useLayoutEffect, useRef } from "react";
import { Radio } from "lucide-react";
import { connectionLost, recoverConnection, useDisconnected } from "./api";

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
    window.addEventListener("online", check);
    return () => {
      controller.abort();
      clearTimeout(timer);
      window.removeEventListener("online", check);
    };
  }, [disconnected]);

  useLayoutEffect(() => {
    const el = dialog.current;
    if (!disconnected || !el) return;
    el.showModal();
    return () => el.close();
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
