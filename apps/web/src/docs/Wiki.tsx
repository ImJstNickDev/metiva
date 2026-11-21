import { useEffect, useMemo, useRef, useState } from "react";
import {
  ArrowRight,
  ArrowUpRight,
  Book,
  Braces,
  ChevronRight,
  Code2,
  FileStack,
  FolderTree,
  Home,
  LayoutDashboard,
  Menu,
  Moon,
  Package,
  Sun,
  X,
} from "lucide-react";
import mark from "../../../../assets/brand/logo-dark.svg";
import { useTheme } from "../theme";
import {
  contractUrl,
  operations,
  referenceLink,
  type Contract,
  type Operation,
} from "./contract";
import { Guide, guideLink, guides } from "./Guides";
import { Endpoint, ReferenceIndex, groupOperations } from "./Reference";

const indexLink = "#/reference";
const icons = {
  Discovery: FolderTree,
  Publications: FileStack,
  Artifacts: Package,
  Specification: Braces,
};
const tagOrder = Object.keys(icons);
function TopicNav({
  route,
  groups,
  current,
}: {
  route: string;
  groups: [string, Operation[]][];
  current?: Operation;
}) {
  const page = (path: string) => (route === path.slice(2) ? "page" : undefined);
  return (
    <nav className="topics" aria-label="Documentation topics">
      <a href="#/" aria-current={page("#/")}>
        <Home size={16} />
        Overview
      </a>
      <h2>Guides</h2>
      {guides.map(({ id, title, Icon }) => (
        <a key={id} href={guideLink(id)} aria-current={page(guideLink(id))}>
          <Icon size={16} />
          {title}
        </a>
      ))}
      <h2>Endpoint reference</h2>
      <a href={indexLink} aria-current={page(indexLink)}>
        <Code2 size={16} />
        All endpoints
      </a>
      {groups.map(([tag, items]) => {
        const Icon = icons[tag as keyof typeof icons] ?? Braces;
        return (
          <details key={tag} open={current?.tags.includes(tag) || undefined}>
            <summary>
              <Icon size={16} />
              {tag}
              <ChevronRight className="caret" size={14} />
            </summary>
            <div>
              {items.map((op) => (
                <a
                  key={op.operationId}
                  href={referenceLink(
                    current?.path === op.path
                      ? current.operationId
                      : op.operationId,
                  )}
                  aria-current={current?.path === op.path ? "page" : undefined}
                >
                  {op.summary}
                </a>
              ))}
            </div>
          </details>
        );
      })}
      <a className="raw-link" href={contractUrl}>
        <Braces size={16} />
        OpenAPI JSON
        <ArrowUpRight size={13} />
      </a>
    </nav>
  );
}
function Landing() {
  return (
    <>
      <div className="docs-hero">
        <div>
          <h1>Consumer API</h1>
          <p>
            Discover what your mirror publishes and download verified artifacts
            from any client.
          </p>
          <a className="start-button" href={guideLink("first-download")}>
            Start here <ArrowRight size={17} />
          </a>
        </div>
        <img src="/manifest-plates.svg" alt="" />
      </div>
      <section className="landing-guides">
        <h2>Guides</h2>
        {guides.slice(1).map(({ id, title, intro, Icon }) => (
          <a key={id} href={guideLink(id)}>
            <span className="guide-icon">
              <Icon size={21} />
            </span>
            <div>
              <h3>{title}</h3>
              <p>{intro}</p>
            </div>
            <ChevronRight size={17} />
          </a>
        ))}
      </section>
      <a className="reference-callout" href={indexLink}>
        <Code2 size={22} />
        <div>
          <h2>Endpoint reference</h2>
          <p>
            Browse methods, parameters, response bodies and HTTP status codes.
          </p>
        </div>
        <ArrowRight size={18} />
      </a>
    </>
  );
}
const readRoute = () =>
  window.location.hash.replace(/^#\/?/, "").replace(/\/$/, "");
export function Wiki() {
  const [route, setRoute] = useState(readRoute);
  const [spec, setSpec] = useState<Contract>();
  const [failure, setFailure] = useState(false);
  const [attempt, setAttempt] = useState(0);
  const [theme, toggle] = useTheme();
  const main = useRef<HTMLElement>(null);
  const menu = useRef<HTMLDialogElement>(null);
  const [kind, id, extra] = route.split("/");
  const guide =
    kind === "guides" && !extra
      ? guides.find((item) => item.id === id)
      : undefined;
  const ops = useMemo(() => (spec ? operations(spec) : []), [spec]);
  const groups = useMemo(
    () =>
      spec
        ? groupOperations(ops, spec).sort(([a], [b]) => {
            const rank = (tag: string) =>
              tagOrder.includes(tag) ? tagOrder.indexOf(tag) : tagOrder.length;
            return rank(a) - rank(b);
          })
        : [],
    [ops, spec],
  );
  const op =
    kind === "reference" && !extra
      ? ops.find((item) => item.operationId === id)
      : undefined;
  useEffect(() => {
    const controller = new AbortController();
    fetch(contractUrl, { credentials: "omit", signal: controller.signal })
      .then(async (result) => {
        if (!result.ok) throw Error();
        const contract: Contract = await result.json();
        if (
          !contract.paths ||
          !contract.info ||
          !contract.components ||
          !Object.values(contract.paths).some((path) => path.get?.operationId)
        )
          throw Error();
        if (!controller.signal.aborted) setSpec(contract);
      })
      .catch(() => {
        if (!controller.signal.aborted) setFailure(true);
      });
    return () => controller.abort();
  }, [attempt]);
  useEffect(() => {
    const navigate = () => {
      menu.current?.close();
      setRoute(readRoute());
      main.current?.focus({ preventScroll: true });
      window.scrollTo(0, 0);
    };
    window.addEventListener("hashchange", navigate);
    return () => window.removeEventListener("hashchange", navigate);
  }, []);
  const pageTitle =
    guide?.title ??
    op?.summary ??
    (!route
      ? "Consumer API"
      : kind === "reference" && (!id || !spec)
        ? "Endpoint reference"
        : "Page unavailable");
  useEffect(() => {
    document.title = pageTitle + " · Metiva Documentation";
  }, [pageTitle]);
  const nav = <TopicNav route={route} groups={groups} current={op} />;
  return (
    <>
      <a
        className="docs-skip"
        href="#article"
        onClick={(e) => {
          e.preventDefault();
          main.current?.focus();
        }}
      >
        Skip to article
      </a>
      <header className="docs-header">
        <div>
          <a
            className="docs-brand"
            href="#/"
            aria-label="Metiva documentation home"
          >
            <img src={mark} alt="" />
            <strong>metiva</strong>
            <i />
            <span className="brand-long">Documentation</span>
            <span className="brand-short">Docs</span>
          </a>
          <div className="header-actions">
            <a
              href="/"
              className="workspace-link"
              aria-label="Dashboard"
              title="Dashboard"
            >
              <span>
                Dashboard <ArrowUpRight size={15} />
              </span>
              <LayoutDashboard size={17} />
            </a>
            <button
              className="icon-button"
              type="button"
              onClick={toggle}
              aria-label={`Switch to ${theme === "dark" ? "light" : "dark"} theme`}
            >
              {theme === "dark" ? <Sun size={18} /> : <Moon size={18} />}
            </button>
          </div>
        </div>
      </header>
      <div className="docs-layout">
        <aside className="side-nav">{nav}</aside>
        <main
          id="article"
          className={route ? "docs-article" : "docs-landing"}
          ref={main}
          tabIndex={-1}
        >
          <div className="crumbs">
            <button
              className="contents-button"
              onClick={() => menu.current?.showModal()}
            >
              <Menu size={16} />
              Contents
            </button>
            <a href="#/">Consumer API</a>
            <ChevronRight size={13} />
            <span>
              {guide
                ? "Guides"
                : (op?.tags?.[0] ??
                  (kind === "reference"
                    ? "Endpoint reference"
                    : route
                      ? "Documentation"
                      : "Overview"))}
            </span>
          </div>
          {!route ? (
            <Landing />
          ) : guide ? (
            <Guide
              key={id}
              id={guide.id}
              origin={window.location.origin}
              ops={ops}
            />
          ) : kind === "reference" && !spec ? (
            <>
              <h1>Endpoint reference</h1>
              {failure ? (
                <div role="alert">
                  <p>
                    The mirror could not supply its API contract. You can still
                    read the guides.
                  </p>
                  <button
                    className="retry"
                    onClick={() => {
                      setFailure(false);
                      setAttempt(attempt + 1);
                    }}
                  >
                    Retry loading reference
                  </button>
                </div>
              ) : (
                <p role="status">Loading the mirror’s API contract…</p>
              )}
            </>
          ) : op && spec ? (
            <Endpoint
              key={op.operationId}
              op={op}
              ops={ops}
              spec={spec}
              origin={window.location.origin}
            />
          ) : route === "reference" ? (
            <ReferenceIndex groups={groups} ops={ops} />
          ) : (
            <>
              <h1>Page unavailable</h1>
              <p>This installation has no article at this address.</p>
              <a className="text-link" href="#/">
                Return to overview <ArrowRight size={15} />
              </a>
            </>
          )}
          <footer className="docs-footer">
            <span>
              <Book size={14} />
              Metiva documentation
            </span>
            {spec && <span>API v{spec.info.version}</span>}
          </footer>
        </main>
      </div>
      <dialog
        ref={menu}
        className="mobile-nav"
        aria-label="Documentation contents"
        onClick={(event) => {
          if (
            event.target === menu.current ||
            (event.target as Element).closest("a")
          )
            menu.current?.close();
        }}
      >
        <div>
          <div className="menu-head">
            <strong>Contents</strong>
            <button
              className="icon-button"
              aria-label="Close contents"
              onClick={() => menu.current?.close()}
            >
              <X size={18} />
            </button>
          </div>
          {nav}
        </div>
      </dialog>
    </>
  );
}
