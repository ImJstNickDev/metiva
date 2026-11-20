import { useInfiniteQuery } from "@tanstack/react-query";
import { Download } from "lucide-react";
import { api } from "./api";
import { bytes, date, type ArchiveFile, type Revision } from "./model";
import { Data, Dialog, Empty, Status } from "./ui";

export function FileHistory({
  file,
  onClose,
}: {
  file: ArchiveFile;
  onClose: () => void;
}) {
  const history = useInfiniteQuery({
    queryKey: ["file-history", file.id],
    initialPageParam: null as number | null,
    queryFn: ({ pageParam }) =>
      api<{ revisions: Revision[]; next_before: number | null }>(
        `files/${file.id}/history` +
          (pageParam == null ? "" : "?before=" + pageParam),
      ),
    getNextPageParam: (page) => page.next_before ?? undefined,
  });
  return (
    <Dialog
      title={file.filename}
      desc={file.release + " · File history"}
      onClose={onClose}
    >
      <Data query={history}>
        {(data) => {
          const revisions = data.pages.flatMap((page) => page.revisions);
          return revisions.length ? (
            <>
              <ol className="revision-list">
                {revisions.map((r) => (
                  <li key={r.id}>
                    <div className="inline space">
                      <strong>{date(r.created_at)}</strong>
                      {r.current && <Status value="current" />}
                    </div>
                    <small>
                      {r.job_id != null ? "Sync #" + r.job_id + " · " : ""}
                      {bytes(r.size)}
                    </small>
                    <dl className="stack-dl">
                      <dt>SHA-256</dt>
                      <dd className="mono wrap">{r.digest}</dd>
                      <dt>Upstream source</dt>
                      <dd className="wrap">{r.source_url}</dd>
                    </dl>
                    <a
                      href={r.download_url}
                      download
                      className="btn small"
                      aria-label={`Download ${file.filename} revision ${r.id}`}
                    >
                      <Download size={14} />
                      Download revision
                    </a>
                  </li>
                ))}
              </ol>
              {history.hasNextPage && (
                <button
                  className="btn"
                  disabled={history.isFetchingNextPage}
                  onClick={() => void history.fetchNextPage()}
                >
                  Load older revisions
                </button>
              )}
            </>
          ) : (
            <Empty title="No file revisions" />
          );
        }}
      </Data>
    </Dialog>
  );
}

export function PastFiles({
  runtimeId,
  onClose,
}: {
  runtimeId: number;
  onClose: () => void;
}) {
  const files = useInfiniteQuery({
    queryKey: ["past-files", runtimeId],
    initialPageParam: null as number | null,
    queryFn: ({ pageParam }) =>
      api<{
        files: (ArchiveFile & { created_at: number })[];
        next_before: number | null;
      }>(
        `runtimes/${runtimeId}/past-files` +
          (pageParam == null ? "" : "?before=" + pageParam),
      ),
    getNextPageParam: (page) => page.next_before ?? undefined,
  });
  return (
    <Dialog
      title="Past files"
      desc="Files from earlier publications that are absent from the current runtime."
      onClose={onClose}
    >
      <Data query={files}>
        {(data) => (
          <>
            {data.pages[0].files.length ? (
              <div className="archived-files">
                {data.pages
                  .flatMap((page) => page.files)
                  .map((f) => (
                    <article className="archive-file" key={f.id}>
                      <div className="inline space">
                        <strong className="wrap">{f.filename}</strong>
                        <a
                          className="btn small"
                          href={`/api/admin/archive/files/${f.id}`}
                          download
                          aria-label={"Download " + f.filename}
                        >
                          <Download size={14} />
                          {bytes(f.size)}
                        </a>
                      </div>
                      <small>
                        {f.release} · {date(f.created_at)}
                      </small>
                      <dl className="stack-dl">
                        <dt>SHA-256</dt>
                        <dd className="mono wrap">{f.digest}</dd>
                        <dt>Upstream source</dt>
                        <dd className="wrap">{f.source_url}</dd>
                      </dl>
                    </article>
                  ))}
              </div>
            ) : (
              <Empty title="No past files" />
            )}
            {files.hasNextPage && (
              <button
                className="btn"
                disabled={files.isFetchingNextPage}
                onClick={() => void files.fetchNextPage()}
              >
                Load older files
              </button>
            )}
          </>
        )}
      </Data>
    </Dialog>
  );
}
