import { useState } from "react";
import { useInfiniteQuery } from "@tanstack/react-query";
import { api, useResource } from "./api";
import { Originals } from "./Archive";
import { date, type Manifest, type Original } from "./model";
import { Data, Dialog, Empty, Field } from "./ui";

interface Publication {
  id: number;
  created_at: number;
  current: boolean;
}
export function PublicationHistory({
  runtimeId,
  onClose,
}: {
  runtimeId: number;
  onClose: () => void;
}) {
  const path = `runtimes/${runtimeId}/publications`;
  const history = useInfiniteQuery({
    queryKey: [path],
    initialPageParam: null as number | null,
    queryFn: ({ pageParam, signal }) =>
      api<{ publications: Publication[]; next_before: number | null }>(
        path + (pageParam == null ? "" : "?before=" + pageParam),
        "GET",
        undefined,
        signal,
      ),
    getNextPageParam: (page) => page.next_before ?? undefined,
  });
  const [selected, setSelected] = useState<number | null>(null);
  const selectedId = selected ?? history.data?.pages[0].publications[0]?.id;
  const snapshot = useResource<{ manifest: Manifest; originals: Original[] }>(
    path + "/" + selectedId,
    { enabled: selectedId != null },
  );
  return (
    <Dialog title="Publication history" onClose={onClose}>
      <Data query={history}>
        {(data) => {
          const entries = data.pages.flatMap((p) => p.publications);
          return entries.length ? (
            <div className="form publication-history">
              <Field label="Publication">
                <select
                  value={selectedId ?? ""}
                  onChange={(e) => setSelected(Number(e.target.value))}
                >
                  {entries.map((p) => (
                    <option
                      key={p.id}
                      value={p.id}
                    >{`#${p.id} · ${date(p.created_at)}${p.current ? " · Current" : ""}`}</option>
                  ))}
                </select>
              </Field>
              {history.hasNextPage && (
                <button
                  className="btn small"
                  disabled={history.isFetchingNextPage}
                  onClick={() => void history.fetchNextPage()}
                >
                  Load older publications
                </button>
              )}
              <Data query={snapshot}>
                {(value) => (
                  <>
                    <details open>
                      <summary>Published manifest JSON</summary>
                      <pre>{JSON.stringify(value.manifest, null, 2)}</pre>
                    </details>
                    <section>
                      <h3>Original source documents</h3>
                      {value.originals.length ? (
                        <Originals items={value.originals} />
                      ) : (
                        <small>
                          No original documents for this publication.
                        </small>
                      )}
                    </section>
                  </>
                )}
              </Data>
            </div>
          ) : (
            <Empty title="No publications" />
          );
        }}
      </Data>
    </Dialog>
  );
}
