import { useState } from "react";
import {
  Braces,
  Clock,
  ExternalLink,
  FileJson,
  FileUp,
  GitFork,
  History,
  Link2,
  Pencil,
  Plus,
  Trash2,
  RefreshCw,
  Settings2,
} from "lucide-react";
import { api, useAction, useResource } from "./api";
import {
  date,
  type ArchiveFile,
  type Detail,
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
import { Originals, ParkingAction } from "./Archive";
import { ImportSource, ManifestView } from "./SourceImport";
import { PublicationHistory } from "./PublicationHistory";
import { FileHistory, PastFiles } from "./FileHistory";
import { ActivityRows } from "./Overview";

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
  has_past_files?: boolean;
  has_upstream?: boolean;
  active_publication?: number | null;
};
export function RuntimeView({ id, project }: { id: number; project: Project }) {
  const query = useResource<Extended>("runtimes/" + id, {
      refetchInterval: 5000,
    }),
    settings = useResource<Settings>("settings");
  const [tab, setTab] = useState<(typeof runtimeTabs)[number]>("Sources"),
    [modal, setModal] = useState<
      "import" | "settings" | "past-files" | "publications" | null
    >(null),
    notice = useNotice();
  const [editing, setEditing] = useState<Source | null>(null),
    [deleting, setDeleting] = useState<Source | null>(null);
  const [fileHistory, setFileHistory] = useState<ArchiveFile | null>(null);
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
        const parked = !!(
          r.parked_at ||
          r.project_parked_at ||
          project.parked_at
        );
        const path = `/p/${encodeURIComponent(project.slug)}/r/${encodeURIComponent(r.slug)}`,
          publicOrigin = settings.data?.public_url || window.location.origin;
        return (
          <>
            <Head
              title={r.name}
              desc={project.name + " / " + r.slug}
              actions={
                <>
                  {r.project_parked_at || project.parked_at ? (
                    <a className="btn" href={"#projects/" + project.id}>
                      View parked project
                    </a>
                  ) : (
                    <ParkingAction
                      kind="runtime"
                      id={id}
                      name={r.name}
                      restore={!!r.parked_at}
                    />
                  )}
                  {!parked && (
                    <button
                      className="btn"
                      onClick={() => setModal("settings")}
                    >
                      <Settings2 />
                      Settings
                    </button>
                  )}
                  {!parked && (
                    <button
                      className="btn primary"
                      disabled={sync.isPending || !r.sources.length}
                      onClick={() => sync.mutate(new FormData())}
                    >
                      <RefreshCw />
                      Sync now
                    </button>
                  )}
                </>
              }
            />
            <Problem error={sync.error} />
            <div className="runtime-strip">
              <Status value={parked ? "parked" : r.status} />
              {parked && (
                <span>
                  {r.project_parked_at || project.parked_at
                    ? "Project parked"
                    : "Runtime parked"}{" "}
                  {date(
                    r.project_parked_at || project.parked_at || r.parked_at,
                  )}
                </span>
              )}
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
                  !parked && (
                    <button
                      className="btn small"
                      onClick={() => setModal("import")}
                    >
                      <Plus size={14} />
                      Import source
                    </button>
                  )
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
                          {!parked && (
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
                          )}
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
                  {parked ? (
                    <Empty
                      icon={Link2}
                      title="Publication parked"
                      desc="Restore this resource to make its mirror endpoints available again."
                    />
                  ) : r.manifest ? (
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
                    <ManifestView
                      manifest={r.manifest}
                      files={r.current_files}
                      onInspect={setFileHistory}
                    />
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
                {(!!r.current_files?.length || r.has_past_files) && (
                  <Panel
                    title="Published files"
                    icon={GitFork}
                    sub="Select a filename to inspect its revisions or download an earlier copy."
                    action={
                      r.has_past_files && (
                        <button
                          className="btn small"
                          onClick={() => setModal("past-files")}
                        >
                          <History size={14} />
                          Past files
                        </button>
                      )
                    }
                  >
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
                          {r.current_files?.map((f) => (
                            <tr key={f.id}>
                              <td>
                                <button
                                  className="text-button file-link"
                                  onClick={() => setFileHistory(f)}
                                >
                                  <History size={14} />
                                  {f.filename}
                                </button>
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
              <Panel
                title="Sync history"
                icon={History}
                action={
                  r.active_publication ? (
                    <button
                      className="btn small"
                      onClick={() => setModal("publications")}
                    >
                      <History size={14} />
                      Publication history
                    </button>
                  ) : undefined
                }
              >
                <ActivityRows rows={r.history} />
              </Panel>
            )}
            {modal === "publications" && (
              <PublicationHistory
                runtimeId={id}
                onClose={() => setModal(null)}
              />
            )}
            {modal === "past-files" && (
              <PastFiles runtimeId={id} onClose={() => setModal(null)} />
            )}
            {fileHistory && (
              <FileHistory
                file={fileHistory}
                onClose={() => setFileHistory(null)}
              />
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
                    Future syncs will exclude this source. Files already
                    mirrored keep their published URLs.
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
