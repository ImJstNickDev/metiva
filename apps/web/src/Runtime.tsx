import { useState } from "react";
import { useMutation } from "@tanstack/react-query";
import {
  Braces,
  Clock,
  ExternalLink,
  FileJson,
  FileUp,
  GitFork,
  History,
  Layers3,
  Link2,
  Pencil,
  Plus,
  Trash2,
  RefreshCw,
  Settings2,
} from "lucide-react";
import { api, useAction, useResource } from "./api";
import {
  bytes,
  date,
  type ArchiveFile,
  type Detail,
  type Manifest,
  type Project,
  type Settings,
  type Source,
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
  Problem,
  Status,
  Tabs,
  str,
  useNotice,
} from "./ui";
import { Originals } from "./Archive";
import { ActivityRows } from "./Overview";

const fields = [
  ["releases_path", "Release list", "/releases"],
  ["version_path", "Release version", "/version"],
  ["artifacts_path", "Artifact list", "/artifacts"],
  ["url_path", "Artifact URL", "/url"],
  ["filename_path", "Filename", "/filename"],
  ["size_path", "Size in bytes", "/size"],
  ["sha256_path", "SHA-256", "/sha256"],
  ["next_path", "Next page URL", "/next"],
] as const;
type Input = {
  url?: string;
  upload?: string;
  mapping?: Record<string, string>;
};
function ImportSource({
  id,
  onClose,
  source,
}: {
  id: number;
  onClose: () => void;
  source?: Source;
}) {
  const existingMapping: Record<string, string> =
    typeof source?.mapping === "string"
      ? JSON.parse(source.mapping)
      : source?.mapping || {};
  const [kind, setKind] = useState<"Remote URL" | "Upload JSON">(
      source?.kind === "upload" ? "Upload JSON" : "Remote URL",
    ),
    [useMapping, setUseMapping] = useState(
      Object.keys(existingMapping).length > 0,
    ),
    [preview, setPreview] = useState<{
      input: Input;
      manifest: Manifest;
    } | null>(null);
  const notice = useNotice();
  const check = useMutation({
    mutationFn: async (data: FormData) => {
      let input: Input;
      if (kind === "Remote URL") input = { url: str(data, "url").trim() };
      else {
        const file = data.get("file");
        if (!(file instanceof File) || !file.size)
          throw new Error("Select a JSON file.");
        input = { upload: await file.text() };
      }
      if (useMapping) {
        const mapping: Record<string, string> = {};
        for (const [key] of fields) {
          const value = str(data, key).trim();
          if (value) mapping[key] = value === "$" ? "" : value;
        }
        input.mapping = mapping;
      }
      const manifest = await api<Manifest>(
        `runtimes/${id}/preview`,
        "POST",
        input,
      );
      return { input, manifest };
    },
    onSuccess: setPreview,
  });
  const save = useAction(
    () =>
      api(
        source ? `sources/${source.id}` : `runtimes/${id}/sources`,
        source ? "PATCH" : "POST",
        preview!.input,
      ),
    () => {
      notice(source ? "Source updated" : "Source imported");
      onClose();
    },
  );
  return (
    <Dialog
      title={
        preview ? "Review source" : source ? "Edit source" : "Import source"
      }
      desc={
        preview
          ? "Review the normalized release inventory before importing."
          : "Import an upstream manifest from a URL or a JSON file."
      }
      onClose={onClose}
      busy={check.isPending || save.isPending}
    >
      <div hidden={!!preview}>
        <Form
          submit="Preview manifest"
          onSubmit={check.mutate}
          busy={check.isPending}
          error={check.error}
        >
          <fieldset disabled={check.isPending}>
            <Tabs
              items={["Remote URL", "Upload JSON"] as const}
              current={kind}
              onChange={setKind}
            />
            {kind === "Remote URL" ? (
              <Field label="Manifest URL">
                <input
                  name="url"
                  type="url"
                  defaultValue={source?.url || ""}
                  required
                  placeholder="https://releases.example.org/manifest.json"
                />
              </Field>
            ) : (
              <Field label="Manifest file">
                <input
                  name="file"
                  type="file"
                  accept=".json,application/json"
                  required
                />
              </Field>
            )}
            <label className="check">
              <input
                type="checkbox"
                checked={useMapping}
                onChange={(e) => setUseMapping(e.target.checked)}
              />{" "}
              Customize JSON field mapping
            </label>
            {useMapping && (
              <>
                <p className="hint">
                  Use JSON pointers or dotted paths. Leave a field blank for
                  automatic detection, or enter $ to select the current value.
                </p>
                <div className="field-grid">
                  {fields.map(([k, label, placeholder]) => (
                    <Field key={k} label={label}>
                      <input
                        name={k}
                        defaultValue={
                          existingMapping[k] === "" ? "$" : existingMapping[k]
                        }
                        placeholder={placeholder}
                      />
                    </Field>
                  ))}
                </div>
              </>
            )}
          </fieldset>
        </Form>
      </div>
      {preview && (
        <Form
          submit={source ? "Save source" : "Import source"}
          onSubmit={save.mutate}
          busy={save.isPending}
          error={save.error}
          secondary={
            <button
              className="btn"
              type="button"
              onClick={() => setPreview(null)}
            >
              Back to source
            </button>
          }
        >
          <ManifestView manifest={preview.manifest} />
        </Form>
      )}
    </Dialog>
  );
}
function ManifestView({ manifest }: { manifest: Manifest }) {
  const releases = manifest.releases;
  return (
    <div className="manifest-list">
      <div className="inventory-summary">
        <Layers3 size={16} />
        {releases.length} releases <span>·</span>{" "}
        {releases.reduce((n, r) => n + r.artifacts.length, 0)} artifacts
      </div>
      {releases.map((r, i) => (
        <details key={r.version + i} open={releases.length === 1}>
          <summary>
            {r.version}
            <span>{r.artifacts.length} files</span>
          </summary>
          {r.artifacts.map((a, j) => (
            <div className="artifact" key={j}>
              <FileJson size={16} />
              <div>
                <strong>{a.filename}</strong>
                <small className="mono wrap">{a.url}</small>
                {a.sha256 && (
                  <small className="mono wrap">SHA-256 {a.sha256}</small>
                )}
              </div>
              <small>{bytes(a.size)}</small>
            </div>
          ))}
        </details>
      ))}
    </div>
  );
}
function RuntimeSettings({ r, onClose }: { r: Detail; onClose: () => void }) {
  const notice = useNotice();
  const save = useAction(
    (data) =>
      api(`runtimes/${r.id}`, "PATCH", {
        visibility: str(data, "visibility"),
        sync_interval: Number(str(data, "sync_interval")),
      }),
    () => {
      notice("Runtime settings saved");
      onClose();
    },
  );
  return (
    <Dialog
      title="Runtime settings"
      desc={r.name}
      onClose={onClose}
      busy={save.isPending}
    >
      <Form
        submit="Save changes"
        onSubmit={save.mutate}
        error={save.error}
        busy={save.isPending}
      >
        <Field label="Visibility">
          <select name="visibility" defaultValue={r.visibility}>
            <option value="inherit">Inherit from project</option>
            <option value="private">Private</option>
            <option value="public">Public</option>
          </select>
        </Field>
        <Field label="Sync interval in seconds">
          <input
            name="sync_interval"
            type="number"
            defaultValue={r.sync_interval}
            min="60"
            max="31536000"
            required
          />
        </Field>
      </Form>
    </Dialog>
  );
}
const runtimeTabs = ["Sources", "Publication", "Sync history"] as const;
type Extended = Detail & {
  current_files?: ArchiveFile[];
  has_upstream?: boolean;
};
export function RuntimeView({ id, project }: { id: number; project: Project }) {
  const query = useResource<Extended>("runtimes/" + id, {
      refetchInterval: 5000,
    }),
    settings = useResource<Settings>("settings");
  const [tab, setTab] = useState<(typeof runtimeTabs)[number]>("Sources"),
    [modal, setModal] = useState<"import" | "settings" | null>(null),
    notice = useNotice();
  const [editing, setEditing] = useState<Source | null>(null),
    [deleting, setDeleting] = useState<Source | null>(null);
  const remove = useAction(
    () => api("sources/" + deleting!.id, "DELETE"),
    () => {
      notice("Source removed");
      setDeleting(null);
    },
  );
  const sync = useAction(
    () => api<{ id: number }>(`runtimes/${id}/sync`, "POST", {}),
    (j) => notice("Sync job #" + j.id + " queued"),
  );
  return (
    <Data query={query}>
      {(r) => {
        if (r.project_slug !== project.slug)
          return <Empty title="Runtime unavailable" />;
        const path = `/p/${encodeURIComponent(project.slug)}/r/${encodeURIComponent(r.slug)}`,
          publicOrigin = settings.data?.public_url || window.location.origin;
        return (
          <>
            <Head
              title={r.name}
              desc={project.name + " / " + r.slug}
              actions={
                <>
                  <button className="btn" onClick={() => setModal("settings")}>
                    <Settings2 />
                    Settings
                  </button>
                  <button
                    className="btn primary"
                    disabled={sync.isPending || !r.sources.length}
                    onClick={() => sync.mutate(new FormData())}
                  >
                    <RefreshCw />
                    Sync now
                  </button>
                </>
              }
            />
            <Problem error={sync.error} />
            <div className="runtime-strip">
              <Status value={r.status} />
              <span>
                <Clock />
                Every {r.sync_interval.toLocaleString()}s
              </span>
              <span>
                Last success <strong>{date(r.last_success, "Never")}</strong>
              </span>
            </div>
            <Tabs items={runtimeTabs} current={tab} onChange={setTab} />
            {tab === "Sources" && (
              <Panel
                title="Manifest sources"
                icon={GitFork}
                action={
                  <button
                    className="btn small"
                    onClick={() => setModal("import")}
                  >
                    <Plus size={14} />
                    Import source
                  </button>
                }
              >
                {r.sources.length ? (
                  <div className="source-list">
                    {r.sources.map((s) => (
                      <article key={s.id} className="source-item">
                        <div className="source-title">
                          <span className="entity-icon">
                            {s.kind === "url" ? (
                              <Link2 size={17} />
                            ) : (
                              <FileUp size={17} />
                            )}
                          </span>
                          <div>
                            <strong className="wrap">
                              {s.url || "Uploaded JSON manifest"}
                            </strong>
                            <small>
                              Source #{s.id} ·{" "}
                              {s.kind === "url" ? "Remote URL" : "File upload"}
                            </small>
                          </div>
                          <Status value={s.status} />
                          <div className="row-actions">
                            <button
                              className="icon-btn"
                              aria-label={"Edit source " + s.id}
                              title="Edit source"
                              onClick={() => setEditing(s)}
                            >
                              <Pencil size={15} />
                            </button>
                            <button
                              className="icon-btn"
                              aria-label={"Remove source " + s.id}
                              title="Remove source"
                              onClick={() => {
                                remove.reset();
                                setDeleting(s);
                              }}
                            >
                              <Trash2 size={15} />
                            </button>
                          </div>
                        </div>
                        <dl className="metadata">
                          <div>
                            <dt>Last checked</dt>
                            <dd>{date(s.last_checked, "Never")}</dd>
                          </div>
                          <div>
                            <dt>Last success</dt>
                            <dd>{date(s.last_success, "Never")}</dd>
                          </div>
                          {s.etag && (
                            <div>
                              <dt>ETag</dt>
                              <dd className="mono">{s.etag}</dd>
                            </div>
                          )}
                          {s.last_modified && (
                            <div>
                              <dt>Last modified</dt>
                              <dd>{s.last_modified}</dd>
                            </div>
                          )}
                        </dl>
                        {s.mapping && (
                          <details>
                            <summary>Field mapping</summary>
                            <pre>
                              {typeof s.mapping === "string"
                                ? s.mapping
                                : JSON.stringify(s.mapping, null, 2)}
                            </pre>
                          </details>
                        )}
                        <Problem error={s.error} />
                      </article>
                    ))}
                  </div>
                ) : (
                  <Empty
                    icon={FileJson}
                    title="No manifest sources"
                    desc="Import a source to preview releases and start syncing artifacts."
                  />
                )}
              </Panel>
            )}
            {tab === "Publication" && (
              <>
                <Panel title="Published endpoints" icon={Link2}>
                  {r.manifest ? (
                    <div className="endpoints">
                      {[
                        "manifest.json",
                        ...(r.has_upstream ? ["upstream.json"] : []),
                      ].map((file) => {
                        const url =
                          publicOrigin.replace(/\/$/, "") + path + "/" + file;
                        return (
                          <div className="endpoint" key={file}>
                            <div>
                              <strong>
                                {file === "manifest.json"
                                  ? "Normalized manifest"
                                  : "Upstream-shaped manifest"}
                              </strong>
                              <code className="wrap">{url}</code>
                            </div>
                            <CopyButton value={url} />
                            {(r.visibility === "public" ||
                              (r.visibility === "inherit" &&
                                project.visibility === "public")) && (
                              <a
                                className="icon-btn"
                                aria-label={"Open " + file}
                                target="_blank"
                                rel="noreferrer"
                                href={url}
                              >
                                <ExternalLink size={15} />
                              </a>
                            )}
                          </div>
                        );
                      })}
                    </div>
                  ) : (
                    <Empty
                      icon={Link2}
                      title="No publication yet"
                      desc="A successful sync publishes this runtime’s mirror endpoints."
                    />
                  )}
                </Panel>
                {r.manifest && (
                  <Panel title="Published manifest" icon={Braces}>
                    <ManifestView manifest={r.manifest} />
                    <details className="json">
                      <summary>View manifest JSON</summary>
                      <pre>{JSON.stringify(r.manifest, null, 2)}</pre>
                    </details>
                  </Panel>
                )}
                {!!r.originals?.length && (
                  <Panel title="Source snapshots" icon={FileJson}>
                    <Originals items={r.originals} />
                  </Panel>
                )}
                {!!r.current_files?.length && (
                  <Panel title="Artifact provenance" icon={GitFork}>
                    <div className="table-scroll">
                      <table>
                        <thead>
                          <tr>
                            <th>Artifact</th>
                            <th>Upstream source</th>
                            <th>SHA-256</th>
                          </tr>
                        </thead>
                        <tbody>
                          {r.current_files.map((f) => (
                            <tr key={f.id}>
                              <td>
                                <strong>{f.filename}</strong>
                                <small>{f.release}</small>
                              </td>
                              <td className="wrap">{f.source_url}</td>
                              <td className="mono wrap">{f.digest}</td>
                            </tr>
                          ))}
                        </tbody>
                      </table>
                    </div>
                  </Panel>
                )}
              </>
            )}
            {tab === "Sync history" && (
              <Panel title="Sync history" icon={History}>
                <ActivityRows rows={r.history} />
              </Panel>
            )}
            {modal === "import" && (
              <ImportSource id={id} onClose={() => setModal(null)} />
            )}
            {editing && (
              <ImportSource
                id={id}
                source={editing}
                onClose={() => setEditing(null)}
              />
            )}
            {deleting && (
              <Dialog
                title="Remove source?"
                desc={deleting.url || "Uploaded JSON source #" + deleting.id}
                onClose={() => setDeleting(null)}
                busy={remove.isPending}
              >
                <Form
                  submit="Remove source"
                  onSubmit={remove.mutate}
                  busy={remove.isPending}
                  error={remove.error}
                >
                  <p>
                    Future syncs will exclude this source. Its published
                    snapshots stay in the archive.
                  </p>
                </Form>
              </Dialog>
            )}
            {modal === "settings" && (
              <RuntimeSettings r={r} onClose={() => setModal(null)} />
            )}
          </>
        );
      }}
    </Data>
  );
}
