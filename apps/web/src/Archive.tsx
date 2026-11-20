import { useState } from "react";
import {
  Archive as ArchiveIcon,
  ArchiveRestore,
  Box,
  Download,
  FileJson,
  Folder,
  Search,
} from "lucide-react";
import { api, useAction, useResource } from "./api";
import { bytes, date, type Original, type ParkedResource } from "./model";
import { Data, Dialog, Empty, Form, Head, Panel, useNotice } from "./ui";

export function Originals({ items }: { items: Original[] }) {
  return (
    <div className="originals">
      {items.map((o) => (
        <div className="artifact" key={o.id}>
          <FileJson size={16} />
          <div>
            <strong>
              Source #{o.source_id} · Page {o.page}
            </strong>
            <small className="wrap">{o.url || "Uploaded JSON"}</small>
            <small className="mono wrap">{o.digest}</small>
          </div>
          <a
            className="btn small"
            href={`/api/admin/originals/${o.id}`}
            download
          >
            <Download size={14} />
            {bytes(o.size)}
          </a>
        </div>
      ))}
    </div>
  );
}
export function ParkingAction({
  kind,
  id,
  name,
  restore = false,
}: {
  kind: ParkedResource["kind"];
  id: number;
  name: string;
  restore?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const notice = useNotice();
  const action = restore ? "Restore" : "Park";
  const Icon = restore ? ArchiveRestore : ArchiveIcon;
  const change = useAction(
    () => api(`${kind}s/${id}/${restore ? "restore" : "park"}`, "POST", {}),
    () => {
      setOpen(false);
      notice(name + (restore ? " restored" : " parked"));
    },
  );
  return (
    <>
      <button
        className="btn"
        onClick={() => {
          change.reset();
          setOpen(true);
        }}
      >
        <Icon size={15} /> {action} {kind}
      </button>
      {open && (
        <Dialog
          title={`${action} ${name}?`}
          onClose={() => setOpen(false)}
          busy={change.isPending}
        >
          <Form
            submit={`${action} ${kind}`}
            onSubmit={change.mutate}
            busy={change.isPending}
            error={change.error}
            secondary={
              <button
                className="btn"
                type="button"
                disabled={change.isPending}
                onClick={() => setOpen(false)}
              >
                Cancel
              </button>
            }
          >
            <p>
              {restore
                ? kind === "project"
                  ? "This project returns to the active directory. Its active runtimes can sync and serve files again. Individually parked runtimes stay in the archive."
                  : "This runtime returns to its project and can sync and serve files again."
                : kind === "project"
                  ? "All runtimes in this project stop syncing and serving public requests. Their files, history, and source settings stay available here for inspection."
                  : "This runtime stops syncing and serving public requests. You can inspect its files and history or restore it from the archive."}
            </p>
          </Form>
        </Dialog>
      )}
    </>
  );
}
export function Archive() {
  const query = useResource<ParkedResource[]>("archive", {
    refetchInterval: 15000,
  });
  const [term, setTerm] = useState("");
  return (
    <>
      <Head
        title="Archive"
        desc="Inspect and restore parked projects and runtimes."
      />
      <Panel
        title="Parked resources"
        icon={ArchiveIcon}
        action={
          <label className="search">
            <Search size={15} />
            <input
              aria-label="Search archive"
              value={term}
              onChange={(e) => setTerm(e.target.value)}
              placeholder="Find a project or runtime…"
            />
          </label>
        }
      >
        <Data query={query}>
          {(rows) => {
            const found = rows.filter((r) =>
              [r.name, r.slug, r.project_name, r.kind]
                .join(" ")
                .toLowerCase()
                .includes(term.trim().toLowerCase()),
            );
            return found.length ? (
              <div className="table-scroll">
                <table>
                  <thead>
                    <tr>
                      <th>Resource</th>
                      <th>Type</th>
                      <th>Parked</th>
                      <th>Actions</th>
                    </tr>
                  </thead>
                  <tbody>
                    {found.map((r) => {
                      const isProject = r.kind === "project";
                      const Icon = isProject ? Folder : Box;
                      return (
                        <tr key={r.kind + r.id}>
                          <td>
                            <a
                              className="entity"
                              href={
                                isProject
                                  ? "#projects/" + r.id
                                  : `#projects/${r.project_id}/runtimes/${r.id}`
                              }
                            >
                              <span className="entity-icon">
                                <Icon size={17} />
                              </span>
                              <span>
                                <strong>{r.name}</strong>
                                <small>
                                  {isProject
                                    ? r.slug
                                    : r.project_name + " / " + r.slug}
                                </small>
                              </span>
                            </a>
                          </td>
                          <td className="capitalize">{r.kind}</td>
                          <td>{date(r.parked_at)}</td>
                          <td>
                            {r.parent_parked ? (
                              <a
                                className="more"
                                href={"#projects/" + r.project_id}
                              >
                                Restore parent project first
                              </a>
                            ) : (
                              <ParkingAction
                                kind={r.kind}
                                id={r.id}
                                name={r.name}
                                restore
                              />
                            )}
                          </td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>
              </div>
            ) : (
              <Empty
                icon={ArchiveIcon}
                title={
                  rows.length ? "No matching resources" : "No parked resources"
                }
                desc={
                  rows.length
                    ? "Try another name or slug."
                    : "Park a project or runtime from its detail page to keep its files here."
                }
              />
            );
          }}
        </Data>
      </Panel>
    </>
  );
}
