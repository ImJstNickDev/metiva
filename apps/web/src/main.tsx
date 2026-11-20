import { StrictMode, useEffect, useState, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { QueryClientProvider, useMutation } from "@tanstack/react-query";
import {
  Archive as ArchiveIcon,
  ArrowDownUp,
  BookOpen,
  Boxes,
  Check,
  ChevronRight,
  Folder,
  KeyRound,
  LayoutDashboard,
  LogOut,
  Menu,
  Moon,
  Server,
  Settings2,
  Sun,
  X,
} from "lucide-react";
import "@fontsource-variable/dm-sans/wght.css";
import "./styles.css";
import { api, cache, signedOut, useResource } from "./api";
import { Archive } from "./Archive";
import { Auth } from "./Auth";
import { Keys } from "./Keys";
import { Overview } from "./Overview";
import { Projects } from "./Projects";
import { Settings } from "./Settings";
import { Transfers } from "./Transfers";
import { Brand, Data, Dialog, Empty, NoticeContext, Problem } from "./ui";
import type {
  Detail,
  Project,
  Session,
  Settings as Configuration,
} from "./model";

const links = [
  ["overview", "Overview", LayoutDashboard],
  ["projects", "Projects", Folder],
  ["transfers", "Transfers", ArrowDownUp],
  ["archive", "Archive", ArchiveIcon],
  ["keys", "Access keys", KeyRound],
  ["settings", "Settings", Settings2],
] as const;
function readRoute() {
  return location.hash.replace(/^#\/?/, "").split("/").filter(Boolean);
}
function useRoute() {
  const [route, setRoute] = useState(readRoute);
  useEffect(() => {
    const change = () => {
      setRoute(readRoute());
      window.scrollTo(0, 0);
    };
    window.addEventListener("hashchange", change);
    return () => window.removeEventListener("hashchange", change);
  }, []);
  return route;
}
function Side({
  section,
  username,
  version,
  logout,
}: {
  section: string;
  username: string;
  version?: string;
  logout: () => void;
}) {
  function group(from: number, to: number) {
    return links.slice(from, to).map(([id, label, Icon]) => (
      <a
        key={id}
        href={"#" + id}
        className={section === id ? "selected" : ""}
        aria-current={section === id ? "page" : undefined}
      >
        <Icon size={18} />
        <span>{label}</span>
      </a>
    ));
  }
  return (
    <>
      <a className="brand-link" href="#overview" aria-label="Metiva overview">
        <Brand />
      </a>
      <nav aria-label="Main navigation">
        <p>Workspace</p>
        {group(0, 4)}
        <p>Administration</p>
        {group(4, 6)}
      </nav>
      <div className="side-bottom">
        <div className="instance-version">
          <Server size={16} />
          <span>Self-hosted</span>
          {version && <code>v{version}</code>}
        </div>
        <div className="side-account">
          <span className="avatar">{username.slice(0, 2).toUpperCase()}</span>
          <div>
            <strong>{username}</strong>
            <small>Administrator</small>
          </div>
          <button
            className="icon-btn"
            onClick={logout}
            aria-label="Sign out"
            title="Sign out"
          >
            <LogOut size={16} />
          </button>
        </div>
      </div>
    </>
  );
}
function Workspace({
  session,
  theme,
  toggle,
}: {
  session: Session;
  theme: string;
  toggle: () => void;
}) {
  const route = useRoute(),
    section = route[0] || "overview",
    title = links.find((x) => x[0] === section)?.[1] || "Page unavailable";
  const [drawer, setDrawer] = useState(false),
    [toast, setToast] = useState(""),
    [logout, setLogout] = useState(false);
  const settings = useResource<Configuration>("settings");
  const project = useResource<Project>("projects/" + route[1], {
    enabled: section === "projects" && !!route[1],
  });
  const runtime = useResource<Detail>("runtimes/" + route[3], {
    enabled: section === "projects" && route[2] === "runtimes" && !!route[3],
  });
  const leave = useMutation({
    mutationFn: () => api("logout", "POST", {}),
    onSuccess: signedOut,
  });
  useEffect(() => {
    setDrawer(false);
    document.title = title + " · Metiva";
  }, [route, title]);
  useEffect(() => {
    if (!toast) return;
    const timer = setTimeout(() => setToast(""), 3800);
    return () => clearTimeout(timer);
  }, [toast]);
  const side = (
    <Side
      section={section}
      username={session.username || "Administrator"}
      version={settings.data?.version}
      logout={() => {
        setDrawer(false);
        setLogout(true);
      }}
    />
  );
  let content: ReactNode;
  switch (section) {
    case "overview":
      content = <Overview />;
      break;
    case "projects":
      content = <Projects route={route} />;
      break;
    case "transfers":
      content = <Transfers />;
      break;
    case "archive":
      content = <Archive />;
      break;
    case "keys":
      content = <Keys />;
      break;
    case "settings":
      content = <Settings session={session} />;
      break;
    default:
      content = (
        <Empty
          title="Page unavailable"
          action={
            <a className="btn" href="#overview">
              Back to overview
            </a>
          }
        />
      );
  }
  return (
    <NoticeContext.Provider value={setToast}>
      <a
        href="#main-content"
        className="skip"
        onClick={(e) => {
          e.preventDefault();
          document.getElementById("main-content")?.focus();
        }}
      >
        Skip to content
      </a>
      <div className="shell">
        <aside className="sidebar">{side}</aside>
        <div className="workspace">
          <header className="topbar">
            <div className="crumbs">
              <button
                aria-label="Open navigation"
                className="icon-btn mobile-menu"
                onClick={() => setDrawer(true)}
              >
                <Menu size={20} />
              </button>
              <Boxes size={17} className="workspace-symbol" />
              <span className="workspace-label">Workspace</span>
              <span className="slash">/</span>
              <a href={"#" + section}>{title}</a>
              {section === "projects" && route[1] && (
                <>
                  <ChevronRight size={12} />
                  <a href={"#projects/" + route[1]}>
                    {project.data?.name || "Project"}
                  </a>
                </>
              )}
              {route[2] === "runtimes" && route[3] && (
                <>
                  <ChevronRight size={12} />
                  <span>{runtime.data?.name || "Runtime"}</span>
                </>
              )}
            </div>
            <div className="top-actions">
              <a
                href="/api/v1/openapi.json"
                className="doc-link"
                target="_blank"
                rel="noreferrer"
              >
                <BookOpen size={16} />
                <span>API reference</span>
              </a>
              <i />
              <button
                className="icon-btn"
                title={
                  "Switch to " +
                  (theme === "dark" ? "light" : "dark") +
                  " theme"
                }
                aria-label={
                  "Switch to " +
                  (theme === "dark" ? "light" : "dark") +
                  " theme"
                }
                onClick={toggle}
              >
                {theme === "dark" ? <Sun size={17} /> : <Moon size={17} />}
              </button>
            </div>
          </header>
          <main id="main-content" tabIndex={-1} className="content">
            {content}
          </main>
        </div>
      </div>
      {drawer && (
        <Dialog title="Navigation" onClose={() => setDrawer(false)}>
          <div
            className="drawer-links"
            onClick={(e) => {
              if ((e.target as HTMLElement).closest("a")) setDrawer(false);
            }}
          >
            {side}
          </div>
        </Dialog>
      )}
      {logout && (
        <Dialog
          title="Sign out of Metiva?"
          onClose={() => setLogout(false)}
          busy={leave.isPending}
        >
          <Problem error={leave.error} />
          <div className="form-foot">
            <button className="btn" onClick={() => setLogout(false)}>
              Stay signed in
            </button>
            <button
              className="btn primary"
              disabled={leave.isPending}
              onClick={() => leave.mutate()}
            >
              Sign out
            </button>
          </div>
        </Dialog>
      )}
      {toast && (
        <div className="toast" role="status">
          <Check size={16} />
          {toast}
          <button
            aria-label="Dismiss notification"
            className="icon-btn"
            onClick={() => setToast("")}
          >
            <X size={14} />
          </button>
        </div>
      )}
    </NoticeContext.Provider>
  );
}
function App() {
  const [theme, setTheme] = useState(() => {
    try {
      return localStorage.getItem("metiva-theme") === "light"
        ? "light"
        : "dark";
    } catch {
      return "dark";
    }
  });
  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    try {
      localStorage.setItem("metiva-theme", theme);
    } catch {
      /* Theme remains available for this visit. */
    }
  }, [theme]);
  const session = useResource<Session>("session", { refetchInterval: 60000 });
  return (
    <Data query={session}>
      {(s) =>
        s.authenticated ? (
          <Workspace
            session={s}
            theme={theme}
            toggle={() => setTheme(theme === "dark" ? "light" : "dark")}
          />
        ) : (
          <Auth setup={s.setupRequired} />
        )
      }
    </Data>
  );
}
createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <QueryClientProvider client={cache}>
      <App />
    </QueryClientProvider>
  </StrictMode>,
);
