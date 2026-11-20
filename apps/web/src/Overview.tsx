import {
  Activity as Pulse,
  ArrowDownToLine,
  Boxes,
  Database,
  Folder,
  HardDrive,
  History,
  Layers3,
  ListOrdered,
  Plus,
  Radio,
  TriangleAlert,
} from "lucide-react";
import { useState } from "react";
import { useResource } from "./api";
import {
  bytes,
  date,
  type Activity,
  type Overview as Summary,
  type Project,
} from "./model";
import { Data, Empty, Head, More, Panel, Status } from "./ui";
import { ProjectDialog } from "./Projects";

export function ActivityRows({ rows }: { rows: Activity[] }) {
  return rows.length ? (
    <div className="table-scroll">
      <table>
        <thead>
          <tr>
            <th>Runtime</th>
            <th>Status</th>
            <th>Started</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((a) => (
            <tr key={a.id}>
              <td>
                <strong>{a.runtime || `Job #${a.id}`}</strong>
                {a.project && <small>{a.project}</small>}
                {a.error && <small className="danger-text">{a.error}</small>}
              </td>
              <td>
                <Status value={a.state} />
              </td>
              <td className="mono">{date(a.created_at)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  ) : (
    <Empty
      icon={History}
      title="No sync activity yet"
      desc="Sync a runtime to see its publication history here."
    />
  );
}
export function Overview() {
  const overview = useResource<Summary>("overview", { refetchInterval: 10000 });
  const projects = useResource<Project[]>("projects");
  const [newProject, setNewProject] = useState(false);
  return (
    <>
      <Head
        title="Overview"
        desc="Publishing activity and mirror health across your workspace."
        actions={
          <button className="btn primary" onClick={() => setNewProject(true)}>
            <Plus />
            New project
          </button>
        }
      />
      <Data query={overview}>
        {(o) => (
          <>
            <section className="engine-hero">
              <div className="hero-art" aria-hidden="true" />
              <div className="hero-text">
                <div className="engine-label">
                  <span className="signal">
                    <i />
                    <i />
                    <i />
                    <i />
                  </span>
                  Mirror engine
                </div>
                <h2>
                  Manifests in. <span>Mirrors out.</span>
                </h2>
                <div className="hero-meta">
                  <span>
                    <Radio size={15} />
                    {o.engine === "curl" ? "curl" : "Native"} backend
                  </span>
                  <span>
                    <HardDrive size={15} />
                    {bytes(o.storage_bytes)} stored
                  </span>
                </div>
              </div>
            </section>
            <div className="metrics">
              {(
                [
                  [Folder, "Projects", o.projects, "#projects", "sand"],
                  [Layers3, "Runtimes", o.runtimes, "#projects", "lilac"],
                  [
                    ArrowDownToLine,
                    "Active transfers",
                    o.active,
                    "#transfers",
                    "mint",
                  ],
                  [
                    ListOrdered,
                    "Queued transfers",
                    o.queued,
                    "#transfers",
                    "blue",
                  ],
                ] as const
              ).map(([Icon, label, value, href, color]) => (
                <a key={label} href={href} className="metric">
                  <span className={"metric-icon " + color}>
                    <Icon size={18} />
                  </span>
                  <strong>{value.toLocaleString()}</strong>
                  <span>{label}</span>
                </a>
              ))}
            </div>
            <div className="overview-cols">
              <Panel
                title="Recent syncs"
                icon={Pulse}
                action={<More href="#transfers" label="Transfers" />}
              >
                <ActivityRows rows={o.activity.slice(0, 6)} />
              </Panel>
              <Panel title="Workspace health" icon={Radio}>
                <div className="health-row">
                  <span>
                    <TriangleAlert />
                    Needs attention
                  </span>
                  <a
                    className={o.attention ? "attention" : "quiet"}
                    href="#projects"
                  >
                    {o.attention}
                  </a>
                </div>
                <div className="health-row">
                  <span>
                    <History />
                    Last successful sync
                  </span>
                  <strong>{date(o.last_success, "No syncs yet")}</strong>
                </div>
                <div className="health-row">
                  <span>
                    <Database />
                    Artifact storage
                  </span>
                  <strong>{bytes(o.storage_bytes)}</strong>
                </div>
              </Panel>
            </div>
            <Panel
              title="Projects"
              icon={Boxes}
              action={<More href="#projects" />}
            >
              <Data query={projects}>
                {(ps) =>
                  ps.length ? (
                    <div className="table-scroll">
                      <table>
                        <thead>
                          <tr>
                            <th>Project</th>
                            <th>Runtimes</th>
                            <th>Visibility</th>
                            <th>Status</th>
                          </tr>
                        </thead>
                        <tbody>
                          {ps.slice(0, 5).map((p) => (
                            <tr key={p.id}>
                              <td>
                                <a
                                  className="entity"
                                  href={"#projects/" + p.id}
                                >
                                  <span className="entity-icon">
                                    <Folder size={17} />
                                  </span>
                                  <strong>{p.name}</strong>
                                </a>
                              </td>
                              <td>{p.runtime_count}</td>
                              <td className="capitalize">{p.visibility}</td>
                              <td>
                                <Status value={p.status} />
                              </td>
                            </tr>
                          ))}
                        </tbody>
                      </table>
                    </div>
                  ) : (
                    <Empty
                      icon={Folder}
                      title="No projects yet"
                      desc="Create a project to group runtimes and publish your first mirror."
                    />
                  )
                }
              </Data>
            </Panel>
          </>
        )}
      </Data>
      {newProject && <ProjectDialog onClose={() => setNewProject(false)} />}
    </>
  );
}
