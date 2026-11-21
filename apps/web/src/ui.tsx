import {
  cloneElement,
  isValidElement,
  type ReactElement,
  createContext,
  useContext,
  useEffect,
  useId,
  useRef,
  useState,
  type FormEvent,
  type ReactNode,
} from "react";
import {
  AlertCircle,
  Box,
  Check,
  ChevronRight,
  Copy,
  LoaderCircle,
  X,
  type LucideIcon,
} from "lucide-react";
import { ConnectionError } from "./api";
import { display } from "./model";

export const NoticeContext = createContext<(message: string) => void>(() => {});
export const useNotice = () => useContext(NoticeContext);
export function Brand() {
  return (
    <span className="brand">
      <svg aria-hidden="true" viewBox="0 0 60 70">
        <path
          fill="currentColor"
          d="M6 45V13l9-5 15 16L44 8l9 5v9L32 45 17 29v10z M40 40l13-14v7L40 47z M40 50l13-14v6L40 56z M40 59l13-14v6l-13 14z"
        />
      </svg>
      <span>metiva</span>
    </span>
  );
}
export function Head({
  title,
  desc,
  actions,
}: {
  title: string;
  desc?: string;
  actions?: ReactNode;
}) {
  return (
    <header className="page-head">
      <div>
        <h1>{title}</h1>
        {desc && <p>{desc}</p>}
      </div>
      {actions && <div className="head-actions">{actions}</div>}
    </header>
  );
}
export function Panel({
  title,
  icon: Icon,
  action,
  children,
  sub,
}: {
  title: string;
  icon?: LucideIcon;
  action?: ReactNode;
  children: ReactNode;
  sub?: string;
}) {
  return (
    <section className="panel">
      <div className="panel-head">
        <div className="panel-title">
          {Icon && <Icon size={18} />}
          <div>
            <h2>{title}</h2>
            {sub && <p>{sub}</p>}
          </div>
        </div>
        {action}
      </div>
      {children}
    </section>
  );
}
export function More({
  href,
  label = "View all",
}: {
  href: string;
  label?: string;
}) {
  return (
    <a className="more" href={href}>
      {label}
      <ChevronRight size={15} />
    </a>
  );
}
export function Empty({
  title,
  desc,
  icon: Icon = Box,
  action,
}: {
  title: string;
  desc?: string;
  icon?: LucideIcon;
  action?: ReactNode;
}) {
  return (
    <div className="empty">
      <span className="empty-icon">
        <Icon size={24} strokeWidth={1.45} />
      </span>
      <h3>{title}</h3>
      {desc && <p>{desc}</p>}
      {action}
    </div>
  );
}
export function Status({ value }: { value: string }) {
  const type =
    /^(success|ready|complete|completed|published|healthy|unchanged|synced|active|succeeded|ok)$/i.test(
      value,
    )
      ? "good"
      : /^(failed|error|revoked|cancelled)$/i.test(value)
        ? "bad"
        : /^(running|downloading|pending|queued|retrying|paused)$/i.test(value)
          ? "warm"
          : "muted";
  return (
    <span className={"status " + type}>
      <i />
      {display(value)}
    </span>
  );
}
export function Problem({ error }: { error: unknown }) {
  if (!error) return null;
  return (
    <p role="alert" className="problem">
      <AlertCircle size={17} />
      {error instanceof Error ? error.message : String(error)}
    </p>
  );
}
export function Data<T>({
  query,
  children,
}: {
  query: {
    isPending: boolean;
    error: unknown;
    data?: T;
    refetch: () => unknown;
  };
  children: (data: T) => ReactNode;
}) {
  if (query.error instanceof ConnectionError && query.data === undefined) return null;
  if (query.error && !(query.error instanceof ConnectionError))
    return (
      <div className="query-error">
        <Problem error={query.error} />
        <button className="btn small" onClick={() => query.refetch()}>
          Try again
        </button>
      </div>
    );
  if (query.isPending || query.data === undefined)
    return (
      <div role="status" className="loading">
        <LoaderCircle size={18} />
        Loading…
      </div>
    );
  return <>{children(query.data)}</>;
}
export function Dialog({
  title,
  desc,
  onClose,
  children,
  busy = false,
}: {
  title: string;
  desc?: string;
  onClose: () => void;
  children: ReactNode;
  busy?: boolean;
}) {
  const ref = useRef<HTMLDialogElement>(null),
    id = useId();
  useEffect(() => {
    const el = ref.current;
    el?.showModal();
    return () => el?.close();
  }, []);
  return (
    <dialog
      ref={ref}
      aria-labelledby={id}
      onCancel={(event) => {
        event.preventDefault();
        if (!busy) onClose();
      }}
      onClick={(event) => {
        if (!busy && event.target === ref.current) {
          const r = ref.current!.getBoundingClientRect();
          if (
            event.clientX < r.left ||
            event.clientX > r.right ||
            event.clientY < r.top ||
            event.clientY > r.bottom
          )
            onClose();
        }
      }}
    >
      <div className="dialog-head">
        <div>
          <h2 id={id}>{title}</h2>
          {desc && <p>{desc}</p>}
        </div>
        <button
          aria-label="Close dialog"
          className="icon-btn"
          disabled={busy}
          onClick={onClose}
        >
          <X size={19} />
        </button>
      </div>
      {children}
    </dialog>
  );
}
export function Form({
  children,
  submit,
  onSubmit,
  error,
  busy,
  secondary,
  disabled,
}: {
  children: ReactNode;
  submit: string;
  onSubmit: (data: FormData) => void;
  error?: unknown;
  busy?: boolean;
  secondary?: ReactNode;
  disabled?: boolean;
}) {
  function handle(e: FormEvent<HTMLFormElement>) {
    e.preventDefault();
    if (!busy && !disabled) onSubmit(new FormData(e.currentTarget));
  }
  return (
    <form className="form" onSubmit={handle}>
      {children}
      <Problem error={error} />
      <div className="form-foot">
        {secondary}
        <button
          type="submit"
          className="btn primary"
          disabled={busy || disabled}
        >
          {busy && <LoaderCircle size={16} className="spin" />}
          {submit}
        </button>
      </div>
    </form>
  );
}
export function Field({
  label,
  hint,
  children,
}: {
  label: string;
  hint?: string;
  children: ReactNode;
}) {
  const id = useId();
  return (
    <div className="field">
      <label htmlFor={id}>{label}</label>
      {isValidElement(children)
        ? cloneElement(children as ReactElement<Record<string, unknown>>, {
            id,
            "aria-describedby": hint ? id + "-hint" : undefined,
          })
        : children}
      {hint && <small id={id + "-hint"}>{hint}</small>}
    </div>
  );
}
export function CopyButton({
  value,
  label = "Copy",
}: {
  value: string;
  label?: string;
}) {
  const [done, setDone] = useState(false),
    notice = useNotice();
  return (
    <button
      type="button"
      className="btn small"
      onClick={async () => {
        try {
          await navigator.clipboard.writeText(value);
          setDone(true);
          setTimeout(() => setDone(false), 1500);
        } catch {
          notice("Clipboard unavailable. Select the text to copy it.");
        }
      }}
    >
      {done ? <Check size={14} /> : <Copy size={14} />}{" "}
      {done ? "Copied" : label}
    </button>
  );
}
export const str = (d: FormData, key: string) => String(d.get(key) ?? "");
export function Tabs<T extends string>({
  items,
  current,
  onChange,
}: {
  items: readonly T[];
  current: T;
  onChange: (tab: T) => void;
}) {
  return (
    <div className="tabs" aria-label="Sections">
      {items.map((item) => (
        <button
          key={item}
          type="button"
          className={item === current ? "selected" : ""}
          aria-pressed={item === current}
          onClick={() => onChange(item)}
        >
          {item}
        </button>
      ))}
    </div>
  );
}
