import { useEffect, useState } from "react";
import {
  ArrowDownToLine,
  ChevronsUpDown,
  Filter,
  Pause,
  Play,
  Radio,
  RotateCcw,
  Search,
  X,
} from "lucide-react";
import { useMutation } from "@tanstack/react-query";
import { api, cache, isDisconnected, useDisconnected, useResource } from "./api";
import {
  actions,
  bytes,
  date,
  display,
  eta,
  percent,
  transferStatusHref,
  type Transfer,
} from "./model";
import {
  Data,
  Dialog,
  Empty,
  Head,
  Panel,
  Problem,
  Status,
  useNotice,
} from "./ui";

export function useStream() {
  const [connected, setConnected] = useState(false);
  const disconnected = useDisconnected();
  useEffect(() => {
    if (disconnected || typeof EventSource === "undefined") return;
    const stream = new EventSource("/api/admin/events");
    stream.addEventListener("transfers", (event) => {
      if (isDisconnected()) return;
      try {
        const rows: unknown = JSON.parse(event.data);
        if (Array.isArray(rows)) {
          cache.setQueryData(["transfers"], rows);
          setConnected(true);
        }
      } catch {
        setConnected(false);
      }
    });
    stream.onopen = () => setConnected(true);
    stream.onerror = () => {
      setConnected(false);
      if (!isDisconnected())
        void cache.invalidateQueries({ queryKey: ["session"] });
    };
    return () => {
      stream.close();
      setConnected(false);
    };
  }, [disconnected]);
  return connected && !disconnected;
}
const icons = { pause: Pause, resume: Play, retry: RotateCcw, cancel: X };
type Command = keyof typeof icons;
// Match the states counted as active by the Overview API.
const activeStates = ["connecting", "downloading", "verifying"];
const statusOptions = [
  "active",
  "queued",
  ...activeStates,
  "paused",
  "retrying",
  "failed",
  "cancelled",
  "completed",
];
function readStatus(segment: string) {
  try {
    return decodeURIComponent(segment);
  } catch {
    return "";
  }
}
function Progress({ t }: { t: Transfer }) {
  const p = percent(t);
  return (
    <div className="progress">
      <span>
        {bytes(t.bytes)}
        {t.total != null && <> / {bytes(t.total)}</>}
      </span>
      {p == null ? (
        <small>Size unknown</small>
      ) : (
        <div
          className="progress-track"
          role="progressbar"
          aria-label={t.filename + " progress"}
          aria-valuenow={Math.floor(p)}
          aria-valuemin={0}
          aria-valuemax={100}
        >
          <i style={{ width: p + "%" }} />
        </div>
      )}
    </div>
  );
}
function History({ id }: { id: number }) {
  const query = useResource<{
    history: {
      created_at: number;
      state: string;
      bytes: number;
      retries: number;
      error?: string;
    }[];
  }>("transfers/" + id, { refetchInterval: 5000 });
  return (
    <Data query={query}>
      {({ history }) =>
        history.length ? (
          <div className="transfer-history">
            <h3>Transition history</h3>
            <ol>
              {history.map((e, i) => (
                <li key={e.created_at + "-" + i}>
                  <time>{date(e.created_at)}</time>
                  <Status value={e.state} />
                  <span>
                    {bytes(e.bytes)} · {e.retries} retries
                  </span>
                  {e.error && <small className="danger-text">{e.error}</small>}
                </li>
              ))}
            </ol>
          </div>
        ) : (
          <p className="hint">No transitions recorded.</p>
        )
      }
    </Data>
  );
}
export function Transfers({ statusSegment = "" }: { statusSegment?: string }) {
  const status = readStatus(statusSegment);
  const states = status === "active" ? activeStates : [status];
  const connected = useStream();
  const disconnected = useDisconnected();
  const query = useResource<Transfer[]>("transfers", {
    refetchInterval: connected ? 30000 : 4000,
  });
  const [search, setSearch] = useState(""),
    [project, setProject] = useState(""),
    [engine, setEngine] = useState(""),
    [expanded, setExpanded] = useState<number | null>(null),
    [cancel, setCancel] = useState<Transfer | null>(null);
  const notice = useNotice();
  const control = useMutation({
    mutationFn: ({ id, action }: { id: number; action: Command }) =>
      api("transfers/" + id + "/" + action, "POST", {}),
    onSuccess: (_, args) => {
      setCancel(null);
      notice(display(args.action) + " requested");
      void cache.invalidateQueries({ queryKey: ["transfers"] });
    },
  });
  return (
    <>
      <Head
        title="Transfers"
        desc="Inspect artifact downloads and control work in flight."
        actions={
          <span className={"connection " + (connected ? "online" : "")}>
            <Radio size={15} />
            {disconnected
              ? "Updates paused"
              : connected
                ? "Live updates"
                : "Polling updates"}
          </span>
        }
      />
      <Panel title="Transfer queue" icon={ArrowDownToLine}>
        <Data query={query}>
          {(rows) => {
            const statuses = [
              ...new Set([...statusOptions, ...rows.map((t) => t.state), status]),
            ].filter(Boolean);
            const items = rows.filter(
              (t) =>
                (!status || states.includes(t.state)) &&
                (!project || t.project === project) &&
                (!engine || t.backend === engine) &&
                [t.filename, t.host, t.runtime]
                  .join(" ")
                  .toLowerCase()
                  .includes(search.toLowerCase()),
            );
            return (
              <>
                <div className="filter-bar">
                  <label className="search">
                    <Search size={15} />
                    <input
                      aria-label="Search transfers"
                      value={search}
                      onChange={(e) => setSearch(e.target.value)}
                      placeholder="Find a file, host, or runtime…"
                    />
                  </label>
                  <div className="filters">
                    <Filter size={15} />
                    {(
                      [
                        [
                          "status",
                          status,
                          (value: string) => {
                            location.hash = transferStatusHref(value);
                          },
                          statuses,
                        ],
                        [
                          "project",
                          project,
                          setProject,
                          Array.from(new Set(rows.map((t) => t.project))),
                        ],
                        [
                          "engine",
                          engine,
                          setEngine,
                          Array.from(new Set(rows.map((t) => t.backend))),
                        ],
                      ] as const
                    ).map(([label, value, change, values]) => (
                      <select
                        aria-label={"Filter by " + label}
                        key={label}
                        value={value}
                        onChange={(e) => change(e.target.value)}
                      >
                        <option value="">
                          {label === "status"
                            ? "All states"
                            : label === "project"
                              ? "All projects"
                              : "All engines"}
                        </option>
                        {values.map((v) => (
                          <option key={v} value={v}>
                            {display(v)}
                          </option>
                        ))}
                      </select>
                    ))}
                  </div>
                </div>
                {!cancel && <Problem error={control.error} />}
                {items.length ? (
                  <div className="queue">
                    {items.map((t) => (
                      <article className="queue-row" key={t.id}>
                        <div className="queue-main">
                          <div className="queue-name">
                            <button
                              className="text-button"
                              aria-expanded={expanded === t.id}
                              onClick={() =>
                                setExpanded(expanded === t.id ? null : t.id)
                              }
                            >
                              <strong>{t.filename}</strong>
                              <ChevronsUpDown size={13} />
                            </button>
                            <small>
                              {t.project} / {t.runtime}
                            </small>
                          </div>
                          <div>
                            <Status value={t.state} />
                          </div>
                          <Progress t={t} />
                          <span className="rate">
                            {t.speed != null && t.state === "downloading"
                              ? bytes(t.speed) + "/s"
                              : "—"}
                            {eta(t) && <small>ETA {eta(t)}</small>}
                          </span>
                          <div className="queue-controls">
                            {actions(t.state).map((action) => {
                              const Icon = icons[action];
                              return (
                                <button
                                  className="icon-btn"
                                  disabled={control.isPending}
                                  aria-label={
                                    display(action) + " " + t.filename
                                  }
                                  title={display(action)}
                                  key={action}
                                  onClick={() =>
                                    action === "cancel"
                                      ? setCancel(t)
                                      : control.mutate({ id: t.id, action })
                                  }
                                >
                                  <Icon size={15} />
                                </button>
                              );
                            })}
                          </div>
                        </div>
                        {expanded === t.id && (
                          <>
                            <dl className="metadata transfer-meta">
                              <div>
                                <dt>Origin host</dt>
                                <dd>{t.host}</dd>
                              </div>
                              <div>
                                <dt>Backend</dt>
                                <dd>{t.backend}</dd>
                              </div>
                              <div>
                                <dt>Retries</dt>
                                <dd>{t.retries}</dd>
                              </div>
                              <div>
                                <dt>Next retry</dt>
                                <dd>{date(t.retry_at)}</dd>
                              </div>
                              <div>
                                <dt>Job</dt>
                                <dd>#{t.job_id}</dd>
                              </div>
                            </dl>
                            <History id={t.id} />
                          </>
                        )}
                        {t.error && (
                          <small className="transfer-error">{t.error}</small>
                        )}
                      </article>
                    ))}
                  </div>
                ) : (
                  <Empty
                    icon={ArrowDownToLine}
                    title={
                      rows.length ? "No matching transfers" : "No transfers yet"
                    }
                    desc={
                      rows.length
                        ? "Adjust the filters to see other transfers."
                        : "Downloads appear here when a runtime sync starts."
                    }
                  />
                )}
                <div className="table-foot">
                  {items.length} {items.length === 1 ? "transfer" : "transfers"}
                  {items.length !== rows.length && " of " + rows.length}
                </div>
              </>
            );
          }}
        </Data>
      </Panel>
      {cancel && (
        <Dialog
          title="Cancel transfer?"
          desc={cancel.filename}
          onClose={() => setCancel(null)}
          busy={control.isPending}
        >
          <p className="dialog-body">
            This stops the download. You can retry the transfer later.
          </p>
          <Problem error={control.error} />
          <div className="form-foot">
            <button className="btn" onClick={() => setCancel(null)}>
              Keep transfer
            </button>
            <button
              className="btn danger"
              disabled={control.isPending}
              onClick={() =>
                control.mutate({ id: cancel.id, action: "cancel" })
              }
            >
              Cancel transfer
            </button>
          </div>
        </Dialog>
      )}
    </>
  );
}
