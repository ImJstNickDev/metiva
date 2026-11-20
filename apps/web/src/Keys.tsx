import { useState } from "react";
import {
  Ban,
  KeyRound,
  Pencil,
  Plus,
  RotateCcw,
  ShieldCheck,
} from "lucide-react";
import { api, useAction, useResource } from "./api";
import {
  date,
  display,
  type AccessKey,
  type Project,
  type Runtime,
} from "./model";
import {
  CopyButton,
  Data,
  Dialog,
  Empty,
  Field,
  Form,
  Head,
  Panel,
  Status,
  str,
  useNotice,
} from "./ui";

type Mode =
  { name: "new" } | { name: "rename" | "rotate" | "revoke"; key: AccessKey };
function ManageKey({
  mode,
  onClose,
  onIssued,
}: {
  mode: Mode;
  onClose: () => void;
  onIssued: (k: AccessKey) => void;
}) {
  const [scope, setScope] = useState("project"),
    [project, setProject] = useState(""),
    notice = useNotice();
  const projects = useResource<Project[]>("projects", {
    enabled: mode.name === "new",
  });
  const runtimes = useResource<Runtime[]>("projects/" + project + "/runtimes", {
    enabled: !!project && scope === "runtime",
  });
  const action = useAction<AccessKey | undefined>(
    (data) => {
      if (mode.name === "new")
        return api<AccessKey>("keys", "POST", {
          name: str(data, "name"),
          scope,
          ...(scope !== "global" ? { projectId: Number(project) } : {}),
          ...(scope === "runtime"
            ? { runtimeId: Number(str(data, "runtime")) }
            : {}),
        });
      const path = "keys/" + mode.key.id;
      if (mode.name === "rotate")
        return api<AccessKey>(path + "/rotate", "POST", {});
      if (mode.name === "revoke") return api<undefined>(path, "DELETE");
      return api<AccessKey>(path, "PATCH", { name: str(data, "name") });
    },
    (k) => {
      if (k?.token) onIssued(k);
      else {
        notice(mode.name === "rename" ? "Key renamed" : "Key revoked");
        onClose();
      }
    },
  );
  const title = {
    new: "Create access key",
    rename: "Rename key",
    rotate: "Rotate key",
    revoke: "Revoke key",
  }[mode.name];
  return (
    <Dialog
      title={title}
      desc={
        mode.name === "new"
          ? "Choose the resources this bearer token can read."
          : mode.key.prefix + "…"
      }
      onClose={onClose}
      busy={action.isPending}
    >
      <Form
        submit={title}
        onSubmit={action.mutate}
        error={action.error}
        busy={action.isPending}
      >
        {(mode.name === "new" || mode.name === "rename") && (
          <Field label="Key name">
            <input
              name="name"
              required
              maxLength={120}
              autoFocus
              defaultValue={mode.name === "rename" ? mode.key.name : ""}
              placeholder="Release consumer"
            />
          </Field>
        )}
        {mode.name === "new" && (
          <>
            <Field label="Scope">
              <select value={scope} onChange={(e) => setScope(e.target.value)}>
                <option value="project">One project</option>
                <option value="runtime">One runtime</option>
                <option value="global">All projects and runtimes</option>
              </select>
            </Field>
            {scope !== "global" && (
              <Data query={projects}>
                {(ps) => (
                  <Field label="Project">
                    <select
                      required
                      value={project}
                      onChange={(e) => setProject(e.target.value)}
                    >
                      <option value="">Select a project</option>
                      {ps.map((p) => (
                        <option key={p.id} value={p.id}>
                          {p.name}
                        </option>
                      ))}
                    </select>
                  </Field>
                )}
              </Data>
            )}
            {scope === "runtime" && project && (
              <Data query={runtimes}>
                {(rs) => (
                  <Field label="Runtime">
                    <select
                      key={project}
                      required
                      name="runtime"
                      defaultValue=""
                    >
                      <option value="">Select a runtime</option>
                      {rs.map((r) => (
                        <option key={r.id} value={r.id}>
                          {r.name}
                        </option>
                      ))}
                    </select>
                  </Field>
                )}
              </Data>
            )}
          </>
        )}
        {mode.name === "rotate" && (
          <p>
            A new token will be issued for <strong>{mode.key.name}</strong> with
            the same scope. After updating your consumers, revoke the previous
            key to finish rotation.
          </p>
        )}
        {mode.name === "revoke" && (
          <p>
            Consumers using <strong>{mode.key.name}</strong> will immediately
            lose access to protected mirrors. Revocation is permanent.
          </p>
        )}
      </Form>
    </Dialog>
  );
}
export function Keys() {
  const keys = useResource<AccessKey[]>("keys"),
    projects = useResource<Project[]>("projects");
  const [mode, setMode] = useState<Mode | null>(null),
    [issued, setIssued] = useState<AccessKey | null>(null);
  return (
    <>
      <Head
        title="Access keys"
        desc="Manage scoped bearer credentials for private mirror consumers."
        actions={
          <button
            className="btn primary"
            onClick={() => setMode({ name: "new" })}
          >
            <Plus />
            Create key
          </button>
        }
      />
      <Panel title="Consumer keys" icon={KeyRound}>
        <Data query={keys}>
          {(rows) =>
            rows.length ? (
              <div className="table-scroll">
                <table>
                  <thead>
                    <tr>
                      <th>Name</th>
                      <th>Scope</th>
                      <th>Status</th>
                      <th>Created</th>
                      <th>Last used</th>
                      <th>Actions</th>
                    </tr>
                  </thead>
                  <tbody>
                    {rows.map((k) => (
                      <tr key={k.id}>
                        <td>
                          <strong>{k.name}</strong>
                          <small className="mono">{k.prefix}…</small>
                        </td>
                        <td>
                          {display(k.scope)}
                          {k.scope !== "global" && (
                            <small>
                              {projects.data?.find((p) => p.id === k.projectId)
                                ?.name || "Project #" + k.projectId}
                              {k.runtimeId ? " / Runtime #" + k.runtimeId : ""}
                            </small>
                          )}
                        </td>
                        <td>
                          <Status value={k.revokedAt ? "revoked" : "active"} />
                        </td>
                        <td>{date(k.createdAt)}</td>
                        <td>{date(k.lastUsedAt, "Never")}</td>
                        <td>
                          <div className="row-actions">
                            {(
                              [
                                ["rename", Pencil],
                                ["rotate", RotateCcw],
                                ["revoke", Ban],
                              ] as const
                            ).map(([m, Icon]) => (
                              <button
                                key={m}
                                className="icon-btn"
                                aria-label={display(m) + " " + k.name}
                                title={display(m)}
                                disabled={!!k.revokedAt}
                                onClick={() => setMode({ name: m, key: k })}
                              >
                                <Icon size={15} />
                              </button>
                            ))}
                          </div>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            ) : (
              <Empty
                icon={ShieldCheck}
                title="No consumer keys"
                desc="Create a scoped key for an application that reads private manifests or artifacts."
              />
            )
          }
        </Data>
      </Panel>
      {mode && (
        <ManageKey
          mode={mode}
          onClose={() => setMode(null)}
          onIssued={(k) => {
            setMode(null);
            setIssued(k);
          }}
        />
      )}
      {issued?.token && (
        <Dialog
          title="Save your access key"
          desc="This is the only time the full token is displayed. Store it with your consumer’s credentials."
          onClose={() => setIssued(null)}
        >
          <div className="token-block">
            <code>{issued.token}</code>
            <CopyButton value={issued.token} label="Copy key" />
          </div>
          <div className="form-foot">
            <button className="btn primary" onClick={() => setIssued(null)}>
              Done
            </button>
          </div>
        </Dialog>
      )}
    </>
  );
}
