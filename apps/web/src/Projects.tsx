import { useState } from "react";
import {
  Box,
  CirclePlus,
  Folder,
  Layers3,
  LockKeyhole,
  Plus,
  Search,
  Settings2,
} from "lucide-react";
import { api, useAction, useResource } from "./api";
import { date, slugify, type Project, type Runtime } from "./model";
import {
  Data,
  Dialog,
  Empty,
  Field,
  Form,
  Head,
  Panel,
  Status,
  str,
  useNotice,
} from "./ui";
import { RuntimeView } from "./Runtime";

export function ProjectDialog({
  onClose,
  project,
}: {
  onClose: () => void;
  project?: Project;
}) {
  const [name, setName] = useState(""),
    [customSlug, setCustomSlug] = useState<string | null>(null),
    notice = useNotice();
  const create = useAction(
    (data) =>
      api<Project>(
        project ? `projects/${project.id}` : "projects",
        project ? "PATCH" : "POST",
        project
          ? { visibility: str(data, "visibility") }
          : {
              name,
              slug: str(data, "slug"),
              description: str(data, "description"),
              visibility: str(data, "visibility"),
            },
      ),
    (p) => {
      onClose();
      notice(project ? "Project visibility saved" : "Project created");
      if (!project) location.hash = "projects/" + p.id;
    },
  );
  return (
    <Dialog
      title={project ? "Project visibility" : "New project"}
      onClose={onClose}
      busy={create.isPending}
      desc={
        project
          ? project.name
          : "Group related runtimes under a shared mirror namespace."
      }
    >
      <Form
        submit={project ? "Save changes" : "Create project"}
        onSubmit={create.mutate}
        busy={create.isPending}
        error={create.error}
      >
        {!project && (
          <>
            <Field label="Project name">
              <input
                required
                maxLength={100}
                autoFocus
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder="Runtime distributions"
              />
            </Field>
            <Field label="URL slug">
              <input
                required
                name="slug"
                pattern="[a-z0-9]+(-[a-z0-9]+)*"
                value={customSlug ?? slugify(name)}
                onChange={(e) => setCustomSlug(e.target.value)}
                placeholder="runtime-distributions"
              />
            </Field>
            <Field label="Description">
              <textarea name="description" rows={2} placeholder="Optional" />
            </Field>
          </>
        )}
        <Field
          label="Visibility"
          hint="Private mirrors require a scoped consumer access key."
        >
          <select
            name="visibility"
            defaultValue={project?.visibility ?? "private"}
          >
            <option value="private">Private</option>
            <option value="public">Public</option>
          </select>
        </Field>
      </Form>
    </Dialog>
  );
}
function RuntimeDialog({
  project,
  onClose,
}: {
  project: Project;
  onClose: () => void;
}) {
  const [name, setName] = useState(""),
    [slug, setSlug] = useState<string | null>(null),
    notice = useNotice();
  const create = useAction(
    (data) =>
      api<Runtime>(`projects/${project.id}/runtimes`, "POST", {
        name,
        slug: str(data, "slug"),
        visibility: str(data, "visibility"),
        ...(str(data, "sync_interval")
          ? { sync_interval: Number(str(data, "sync_interval")) }
          : {}),
      }),
    (r) => {
      onClose();
      notice("Runtime created");
      location.hash = `projects/${project.id}/runtimes/${r.id}`;
    },
  );
  return (
    <Dialog
      title="New runtime"
      desc={project.name}
      onClose={onClose}
      busy={create.isPending}
    >
      <Form
        submit="Create runtime"
        onSubmit={create.mutate}
        error={create.error}
        busy={create.isPending}
      >
        <Field label="Runtime name">
          <input
            required
            autoFocus
            maxLength={100}
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="Linux x64"
          />
        </Field>
        <Field label="URL slug">
          <input
            required
            name="slug"
            pattern="[a-z0-9]+(-[a-z0-9]+)*"
            value={slug ?? slugify(name)}
            onChange={(e) => setSlug(e.target.value)}
          />
        </Field>
        <Field label="Visibility">
          <select name="visibility">
            <option value="inherit">Inherit from project</option>
            <option value="private">Private</option>
            <option value="public">Public</option>
          </select>
        </Field>
        <Field label="Sync interval in seconds">
          <input
            type="number"
            min="60"
            name="sync_interval"
            placeholder="Use instance default"
          />
        </Field>
      </Form>
    </Dialog>
  );
}
function ProjectDetail({ project }: { project: Project }) {
  const runtimes = useResource<Runtime[]>(`projects/${project.id}/runtimes`, {
    refetchInterval: 10000,
  });
  const [modal, setModal] = useState<"runtime" | "visibility" | null>(null);
  return (
    <>
      <Head
        title={project.name}
        desc={project.description || `/p/${project.slug}`}
        actions={
          <>
            <button className="btn" onClick={() => setModal("visibility")}>
              <Settings2 />
              Visibility
            </button>
            <button className="btn primary" onClick={() => setModal("runtime")}>
              <Plus />
              New runtime
            </button>
          </>
        }
      />
      <Panel title="Runtimes" icon={Layers3}>
        <Data query={runtimes}>
          {(rs) =>
            rs.length ? (
              <div className="table-scroll">
                <table>
                  <thead>
                    <tr>
                      <th>Name</th>
                      <th>Visibility</th>
                      <th>Status</th>
                      <th>Last successful sync</th>
                    </tr>
                  </thead>
                  <tbody>
                    {rs.map((r) => (
                      <tr key={r.id}>
                        <td>
                          <a
                            className="entity"
                            href={`#projects/${project.id}/runtimes/${r.id}`}
                          >
                            <span className="entity-icon">
                              <Box size={17} />
                            </span>
                            <span>
                              <strong>{r.name}</strong>
                              <small>{r.slug}</small>
                            </span>
                          </a>
                        </td>
                        <td className="capitalize">{r.visibility}</td>
                        <td>
                          <Status value={r.status} />
                        </td>
                        <td>{date(r.last_success, "Never")}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            ) : (
              <Empty
                icon={Layers3}
                title="No runtimes yet"
                desc="Add a runtime to configure an upstream manifest and sync schedule."
              />
            )
          }
        </Data>
      </Panel>
      {modal === "runtime" && (
        <RuntimeDialog project={project} onClose={() => setModal(null)} />
      )}
      {modal === "visibility" && (
        <ProjectDialog project={project} onClose={() => setModal(null)} />
      )}
    </>
  );
}
export function Projects({ route }: { route: string[] }) {
  const projects = useResource<Project[]>("projects", {
    refetchInterval: 15000,
  });
  const [newProject, setNewProject] = useState(false),
    [search, setSearch] = useState("");
  if (route[1])
    return (
      <Data query={projects}>
        {(ps) => {
          const p = ps.find((p) => p.id === Number(route[1]));
          if (!p)
            return (
              <Empty
                icon={Folder}
                title="Project unavailable"
                action={
                  <a href="#projects" className="btn">
                    Back to projects
                  </a>
                }
              />
            );
          return route[2] === "runtimes" && route[3] ? (
            <RuntimeView key={route[3]} id={Number(route[3])} project={p} />
          ) : (
            <ProjectDetail key={p.id} project={p} />
          );
        }}
      </Data>
    );
  return (
    <>
      <Head
        title="Projects"
        desc="Organize runtimes, sources, and published mirrors."
        actions={
          <button className="btn primary" onClick={() => setNewProject(true)}>
            <Plus />
            New project
          </button>
        }
      />
      <Panel
        title="Project directory"
        icon={Folder}
        action={
          <label className="search">
            <Search size={15} />
            <input
              aria-label="Search projects"
              placeholder="Find a project…"
              value={search}
              onChange={(e) => setSearch(e.target.value)}
            />
          </label>
        }
      >
        <Data query={projects}>
          {(ps) => {
            const matches = ps.filter((p) =>
              (p.name + " " + p.slug + " " + p.description)
                .toLowerCase()
                .includes(search.toLowerCase()),
            );
            return matches.length ? (
              <div className="table-scroll">
                <table>
                  <thead>
                    <tr>
                      <th>Project</th>
                      <th>Runtimes</th>
                      <th>Visibility</th>
                      <th>Status</th>
                      <th>Last successful sync</th>
                    </tr>
                  </thead>
                  <tbody>
                    {matches.map((p) => (
                      <tr key={p.id}>
                        <td>
                          <a className="entity" href={"#projects/" + p.id}>
                            <span className="entity-icon">
                              <Folder size={18} />
                            </span>
                            <span>
                              <strong>{p.name}</strong>
                              <small>{p.description || p.slug}</small>
                            </span>
                          </a>
                        </td>
                        <td>{p.runtime_count}</td>
                        <td>
                          <span className="inline capitalize">
                            <LockKeyhole size={13} aria-hidden="true" />
                            {p.visibility}
                          </span>
                        </td>
                        <td>
                          <Status value={p.status} />
                        </td>
                        <td>{date(p.last_success, "Never")}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            ) : (
              <Empty
                icon={CirclePlus}
                title={
                  ps.length
                    ? "No matching projects"
                    : "Create your first project"
                }
                desc={
                  ps.length
                    ? "Try a different name or slug."
                    : "A project gives your mirrors a namespace and a default visibility."
                }
              />
            );
          }}
        </Data>
      </Panel>
      {newProject && <ProjectDialog onClose={() => setNewProject(false)} />}
    </>
  );
}
