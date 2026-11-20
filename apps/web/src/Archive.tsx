import { useInfiniteQuery } from "@tanstack/react-query";
import { useState } from "react";
import {
  Archive as ArchiveIcon,
  Braces,
  Download,
  FileJson,
  GitCompareArrows,
  Search,
} from "lucide-react";
import { api, useResource } from "./api";
import {
  bytes,
  compareFiles,
  date,
  type ArchiveFile,
  type Detail,
  type Original,
  type Snapshot,
} from "./model";
import { Data, Dialog, Empty, Field, Head, Panel, Tabs } from "./ui";

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
function SnapshotDialog({
  snapshot: s,
  all,
  onClose,
}: {
  snapshot: Snapshot;
  all: Snapshot[];
  onClose: () => void;
}) {
  const [tab, setTab] = useState<
      "Files" | "Manifest" | "Originals" | "Compare"
    >("Files"),
    [compare, setCompare] = useState("current");
  const runtime = useResource<Detail & { current_files: ArchiveFile[] }>(
    "runtimes/" + s.runtime_id,
    { enabled: tab === "Compare" },
  );
  const same = all.filter(
    (v) => v.runtime_id === s.runtime_id && v.id !== s.id,
  );
  return (
    <Dialog
      title={`Publication #${s.id}`}
      desc={s.project + " / " + s.runtime + " · " + date(s.created_at)}
      onClose={onClose}
    >
      <Tabs
        items={["Files", "Manifest", "Originals", "Compare"] as const}
        current={tab}
        onChange={setTab}
      />
      {tab === "Files" && (
        <div className="archived-files">
          {s.files.map((f) => (
            <div key={f.id} className="archive-file">
              <div className="inline space">
                <strong>{f.filename}</strong>
                <a
                  className="btn small"
                  href={`/api/admin/archive/files/${f.id}`}
                  download
                >
                  <Download size={14} />
                  {bytes(f.size)}
                </a>
              </div>
              <small>{f.release}</small>
              <dl className="stack-dl">
                <dt>Source</dt>
                <dd className="wrap">{f.source_url}</dd>
                <dt>SHA-256</dt>
                <dd className="mono wrap">{f.digest}</dd>
              </dl>
            </div>
          ))}
        </div>
      )}
      {tab === "Manifest" && <pre>{JSON.stringify(s.manifest, null, 2)}</pre>}
      {tab === "Originals" &&
        (s.originals?.length ? (
          <Originals items={s.originals} />
        ) : (
          <Empty icon={Braces} title="No stored source documents" />
        ))}
      {tab === "Compare" && (
        <>
          <Field label="Compare this snapshot to">
            <select
              value={compare}
              onChange={(e) => setCompare(e.target.value)}
            >
              <option value="current">Current publication</option>
              {same.map((v) => (
                <option
                  key={v.id}
                  value={v.id}
                >{`Publication #${v.id} · ${date(v.created_at)}`}</option>
              ))}
            </select>
          </Field>
          <Data query={runtime}>
            {(r) => {
              const target =
                compare === "current"
                  ? r.current_files
                  : same.find((v) => v.id === Number(compare))?.files;
              if (!target)
                return (
                  <Empty
                    icon={GitCompareArrows}
                    title="Comparison unavailable"
                  />
                );
              const diff = compareFiles(target, s.files);
              return diff.length ? (
                <div className="diff-list">
                  {diff.map((d) => (
                    <div
                      key={JSON.stringify([d.file.release, d.file.filename])}
                    >
                      <span className={"change " + d.change.toLowerCase()}>
                        {d.change}
                      </span>
                      <span className="wrap">
                        {d.file.release} / {d.file.filename}
                      </span>
                    </div>
                  ))}
                </div>
              ) : (
                <Empty
                  icon={GitCompareArrows}
                  title="Identical file inventories"
                  desc="Releases, filenames, source URLs, sizes, and digests match."
                />
              );
            }}
          </Data>
        </>
      )}
    </Dialog>
  );
}
export function Archive() {
  const query = useInfiniteQuery({
      queryKey: ["archive"],
      queryFn: ({ pageParam }) =>
        api<Snapshot[]>("archive" + (pageParam ? "?before=" + pageParam : "")),
      initialPageParam: 0,
      getNextPageParam: (page) =>
        page.length === 100 ? page[page.length - 1].id : undefined,
    }),
    [selected, setSelected] = useState<number | null>(null),
    [term, setTerm] = useState("");
  return (
    <>
      <Head
        title="Archive"
        desc="Inspect earlier publications, compare snapshots, and retrieve archived files."
      />
      <Panel
        title="Archived publications"
        icon={ArchiveIcon}
        action={
          <label className="search">
            <Search size={15} />
            <input
              aria-label="Search archive"
              value={term}
              onChange={(e) => setTerm(e.target.value)}
              placeholder="Find a publication…"
            />
          </label>
        }
      >
        <Data query={query}>
          {(data) => {
            const rows = data.pages.flat();
            const s = rows.find((row) => row.id === selected),
              found = rows.filter((r) =>
                (r.project + " " + r.runtime + " " + r.id)
                  .toLowerCase()
                  .includes(term.toLowerCase()),
              );
            return (
              <>
                {found.length ? (
                  <div className="table-scroll">
                    <table>
                      <thead>
                        <tr>
                          <th>Publication</th>
                          <th>Runtime</th>
                          <th>Published</th>
                          <th>Files</th>
                          <th>Size</th>
                        </tr>
                      </thead>
                      <tbody>
                        {found.map((r) => (
                          <tr key={r.id}>
                            <td>
                              <button
                                className="text-button"
                                onClick={() => setSelected(r.id)}
                              >
                                <strong>Publication #{r.id}</strong>
                              </button>
                            </td>
                            <td>
                              {r.runtime}
                              <small>{r.project}</small>
                            </td>
                            <td>{date(r.created_at)}</td>
                            <td>{r.files.length}</td>
                            <td>
                              {bytes(r.files.reduce((n, f) => n + f.size, 0))}
                            </td>
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  </div>
                ) : (
                  <Empty
                    icon={ArchiveIcon}
                    title={
                      rows.length
                        ? "No matching publications"
                        : "No archived publications"
                    }
                    desc={
                      rows.length
                        ? "Try a project name or runtime."
                        : "When a new publication replaces a mirror, its previous snapshot appears here."
                    }
                  />
                )}
                {query.hasNextPage && (
                  <div className="table-foot">
                    <button
                      className="btn small"
                      disabled={query.isFetchingNextPage}
                      onClick={() => void query.fetchNextPage()}
                    >
                      Load older publications
                    </button>
                  </div>
                )}
                {s && (
                  <SnapshotDialog
                    key={s.id}
                    snapshot={s}
                    all={rows}
                    onClose={() => setSelected(null)}
                  />
                )}
              </>
            );
          }}
        </Data>
      </Panel>
    </>
  );
}
