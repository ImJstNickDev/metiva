import { useState } from "react";
import {
  Bolt,
  KeyRound,
  Server,
  ShieldCheck,
  SlidersHorizontal,
} from "lucide-react";
import { useMutation } from "@tanstack/react-query";
import { api, signedOut, useAction, useResource } from "./api";
import { bytes, type Session, type Settings as Configuration } from "./model";
import { Data, Dialog, Field, Form, Head, Panel, str, useNotice } from "./ui";

function EngineForm({ s }: { s: Configuration }) {
  const notice = useNotice(),
    [global, setGlobal] = useState(s.concurrency);
  const save = useAction(
    (d) =>
      api<Configuration>("settings", "PATCH", {
        engine: str(d, "engine"),
        concurrency: global,
        per_host: Number(str(d, "per_host")),
        retries: Number(str(d, "retries")),
        sync_interval: Number(str(d, "sync_interval")),
      }),
    () => notice("Instance settings saved"),
  );
  return (
    <Form
      submit="Save settings"
      busy={save.isPending}
      error={save.error}
      onSubmit={save.mutate}
    >
      <Field label="Download engine">
        <select name="engine" defaultValue={s.engine}>
          <option value="native">Native HTTP client</option>
          <option value="curl" disabled={!s.curl_available}>
            curl{s.curl_available ? "" : " (unavailable)"}
          </option>
        </select>
      </Field>
      <div className="field-grid">
        <Field label="Concurrent transfers">
          <input
            min={1}
            max={32}
            required
            name="concurrency"
            value={global || ""}
            onChange={(e) => setGlobal(Number(e.target.value))}
            type="number"
          />
        </Field>
        <Field label="Connections per host">
          <input
            min={1}
            max={global}
            required
            name="per_host"
            defaultValue={s.per_host}
            type="number"
          />
        </Field>
        <Field label="Retry limit">
          <input
            min={0}
            max={12}
            required
            name="retries"
            defaultValue={s.retries}
            type="number"
          />
        </Field>
        <Field label="Default sync interval (seconds)">
          <input
            min={60}
            max={31536000}
            required
            name="sync_interval"
            defaultValue={s.sync_interval}
            type="number"
          />
        </Field>
      </div>
    </Form>
  );
}
function PasswordForm({ onClose }: { onClose: () => void }) {
  const save = useMutation({
    mutationFn: async (d: FormData) => {
      if (str(d, "confirm") !== str(d, "newPassword"))
        throw new Error("New passwords do not match.");
      await api("password", "POST", {
        currentPassword: str(d, "currentPassword"),
        newPassword: str(d, "newPassword"),
      });
    },
    onSuccess: () => {
      onClose();
      signedOut();
    },
  });
  return (
    <Dialog
      title="Change password"
      desc="Changing the administrator password signs out all sessions."
      onClose={onClose}
      busy={save.isPending}
    >
      <Form
        submit="Change password"
        onSubmit={save.mutate}
        error={save.error}
        busy={save.isPending}
      >
        <Field label="Current password">
          <input
            autoFocus
            required
            name="currentPassword"
            type="password"
            autoComplete="current-password"
          />
        </Field>
        <Field label="New password" hint="Use at least 12 characters.">
          <input
            required
            name="newPassword"
            minLength={12}
            maxLength={256}
            type="password"
            autoComplete="new-password"
          />
        </Field>
        <Field label="Confirm new password">
          <input
            required
            name="confirm"
            type="password"
            autoComplete="new-password"
          />
        </Field>
      </Form>
    </Dialog>
  );
}
export function Settings({ session }: { session: Session }) {
  const settings = useResource<Configuration>("settings"),
    [password, setPassword] = useState(false);
  return (
    <>
      <Head
        title="Settings"
        desc="Configure the mirror engine and manage administrator security."
      />
      <Data query={settings}>
        {(s) => (
          <div className="settings-layout">
            <Panel title="Download settings" icon={SlidersHorizontal}>
              <div className="panel-body">
                <EngineForm s={s} />
              </div>
            </Panel>
            <div className="settings-side">
              <Panel title="Instance" icon={Server}>
                <dl className="fact-list">
                  <div>
                    <dt>Version</dt>
                    <dd>{s.version}</dd>
                  </div>
                  <div>
                    <dt>Public origin</dt>
                    <dd>{s.public_url}</dd>
                  </div>
                  <div>
                    <dt>Artifact storage</dt>
                    <dd>{bytes(s.storage_bytes)}</dd>
                  </div>
                  <div>
                    <dt>curl support</dt>
                    <dd>{s.curl_available ? "Available" : "Unavailable"}</dd>
                  </div>
                </dl>
              </Panel>
              <Panel title="Administrator" icon={ShieldCheck}>
                <div className="panel-body">
                  <div className="account-line">
                    <span className="avatar">
                      {session.username?.slice(0, 2).toUpperCase()}
                    </span>
                    <div>
                      <strong>{session.username}</strong>
                      <small>Instance administrator</small>
                    </div>
                  </div>
                  <button className="btn" onClick={() => setPassword(true)}>
                    <KeyRound />
                    Change password
                  </button>
                </div>
              </Panel>
            </div>
          </div>
        )}
      </Data>
      {password && <PasswordForm onClose={() => setPassword(false)} />}
    </>
  );
}
