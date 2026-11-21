export interface Session {
  authenticated: boolean;
  setupRequired: boolean;
  username?: string;
  csrfToken?: string;
}
export type Visibility = "public" | "private" | "inherit";
export type Id = number;
export type Time = number | null;
export interface Project {
  parked_at?: Time;
  id: Id;
  name: string;
  slug: string;
  description: string;
  visibility: Visibility;
  runtime_count: number;
  last_success: Time;
  status: string;
}
export interface Runtime {
  parked_at?: Time;
  project_parked_at?: Time;
  id: Id;
  name: string;
  slug: string;
  visibility: Visibility;
  sync_interval: number;
  last_success: Time;
  status: string;
}
export interface Source {
  id: Id;
  kind: string;
  url?: string;
  status: string;
  error?: string;
  etag?: string;
  last_modified?: string;
  last_checked: Time;
  last_success: Time;
  mapping?: SourceMapping | string | null;
}
export interface ArtifactGroup {
  path: string;
  keys: string[];
}
export interface SourceIndex extends ArtifactGroup {
  link_path: string;
}
export interface ReleaseCollection {
  channels?: ArtifactGroup;
  path: string;
  version_path: string;
  groups: ArtifactGroup[];
}
export interface Selection extends ReleaseCollection {
  index?: SourceIndex;
}
export interface ReleaseFilter {
  path: string;
  values: string[];
}
interface RuleBase {
  path: string;
  required?: boolean;
  url_path?: string;
  size_path?: string;
  digest_path?: string;
  algorithm?: string;
}
export interface FileRule extends RuleBase {
  kind: "file";
  name_path?: string;
  url_template?: string;
}
export interface DocumentRule extends RuleBase {
  kind: "document";
  rules: NestedRule[];
}
export type NestedRule = FileRule | DocumentRule;
export interface NestedSelection {
  path: string;
  version_path?: string;
  filters?: ReleaseFilter[];
  limit?: number;
  rules: NestedRule[];
}
export interface DocumentCollection {
  path: string;
  version_path?: string;
  count: number;
  fields: ReleaseFilter[];
  samples: { id: string; pointer: string; fields: Record<string, string> }[];
}
export interface DocumentBranch {
  path: string;
  kind: "url" | "digest";
  sample_pointer: string;
  sample: string;
  url_path?: string;
  name_path?: string;
  size_path?: string;
  digest_path?: string;
  algorithm?: string;
}
export interface DocumentInspection {
  collections: DocumentCollection[];
  branches: DocumentBranch[];
  document_bytes: number;
  truncated?: boolean;
}
export type SourceMapping =
  | Record<string, string>
  | { selection: Selection }
  | { nested: NestedSelection };
export interface Discovery {
  indexes: SourceIndex[];
  collections: ReleaseCollection[];
}
export interface ParkedResource {
  kind: "project" | "runtime";
  id: Id;
  name: string;
  slug: string;
  project_id: Id;
  project_name: string;
  parked_at: number;
  parent_parked: boolean;
}
export interface Revision {
  id: Id;
  created_at: number;
  digest: string;
  size: number;
  source_url: string;
  job_id: Id | null;
  current: boolean;
  download_url: string;
}
export interface Artifact {
  filename: string;
  logical_path?: string;
  url: string;
  size?: number;
  sha256?: string;
}
export interface Manifest {
  releases: { version: string; channel?: string; artifacts: Artifact[] }[];
}
export interface SourcePreview extends Manifest {
  selection?: { matched: number; previewed: number };
  warnings?: { version: string; error: string }[];
}
export interface Activity {
  documents_read?: number | null;
  verified_releases?: number | null;
  failed_releases?: number | null;
  id: Id;
  project?: string;
  runtime?: string;
  state: string;
  created_at: Time;
  error?: string;
}
export interface Original {
  id: Id;
  source_id: Id;
  page: number;
  url?: string;
  digest: string;
  size: number;
}
export interface Detail extends Runtime {
  project_slug: string;
  originals?: Original[];
  sources: Source[];
  manifest: Manifest | null;
  history: Activity[];
}
export interface Transfer {
  id: Id;
  project: string;
  runtime: string;
  filename: string;
  host: string;
  backend: string;
  state: string;
  bytes: number;
  total: number | null;
  speed: number | null;
  retries: number;
  retry_at: Time;
  error?: string;
  job_id: Id;
}
export interface ArchiveFile {
  id: Id;
  release: string;
  filename: string;
  size: number;
  source_url: string;
  digest: string;
}
export interface Snapshot {
  originals?: Original[];
  project_id: Id;
  runtime_id: Id;
  id: Id;
  project: string;
  runtime: string;
  created_at: Time;
  files: ArchiveFile[];
  manifest: Manifest;
}
export interface Overview {
  projects: number;
  runtimes: number;
  active: number;
  queued: number;
  last_success: Time;
  attention: number;
  storage_bytes: number;
  engine: string;
  activity: Activity[];
}
export interface Settings {
  public_url: string;
  engine: string;
  concurrency: number;
  per_host: number;
  retries: number;
  sync_interval: number;
  storage_bytes: number;
  curl_available: boolean;
  version: string;
}
export interface AccessKey {
  id: Id;
  name: string;
  scope: "global" | "project" | "runtime";
  projectId?: Id;
  runtimeId?: Id;
  prefix: string;
  createdAt: Time;
  lastUsedAt: Time;
  revokedAt: Time;
  token?: string;
}

export function bytes(n: number | null | undefined): string {
  if (n == null || !Number.isFinite(n)) return "—";
  if (n === 0) return "0 B";
  const unit = Math.max(
    0,
    Math.min(4, Math.floor(Math.log(Math.max(1, n)) / Math.log(1024))),
  );
  return (
    (n / 1024 ** unit).toLocaleString(undefined, {
      maximumFractionDigits: unit ? 1 : 0,
    }) +
    " " +
    ["B", "KiB", "MiB", "GiB", "TiB"][unit]
  );
}
export function date(n: Time | undefined, fallback = "—") {
  return n
    ? new Date(n * 1000).toLocaleString(undefined, {
        dateStyle: "medium",
        timeStyle: "short",
      })
    : fallback;
}
export function slugify(value: string) {
  return value
    .toLowerCase()
    .normalize("NFKD")
    .replace(/[\u0300-\u036f]/g, "")
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-|-$/g, "");
}
export function display(value: string) {
  return value
    ? value[0].toUpperCase() + value.slice(1).replaceAll("_", " ")
    : "Unknown";
}
export function percent(t: Pick<Transfer, "bytes" | "total" | "state">) {
  if (t.total == null) return null;
  if (t.total === 0) return t.state === "completed" ? 100 : null;
  return Math.max(0, Math.min(100, (100 * t.bytes) / t.total));
}
export function transferStatusHref(status: string) {
  return status ? "#transfers/" + encodeURIComponent(status) : "#transfers";
}
export function actions(state: string) {
  switch (state) {
    case "retrying":
      return ["pause", "retry", "cancel"] as const;
    case "queued":
    case "connecting":
    case "downloading":
      return ["pause", "cancel"] as const;
    case "paused":
      return ["resume", "cancel"] as const;
    case "failed":
    case "cancelled":
      return ["retry"] as const;
    default:
      return [];
  }
}
export function compareFiles(current: ArchiveFile[], previous: ArchiveFile[]) {
  const byName = (files: ArchiveFile[]) =>
    new Map(files.map((f) => [JSON.stringify([f.release, f.filename]), f]));
  const now = byName(current),
    before = byName(previous);
  return [
    ...current.flatMap((file) => {
      const old = before.get(JSON.stringify([file.release, file.filename]));
      if (!old) return [{ file, change: "Added" }];
      if (
        old.digest !== file.digest ||
        old.source_url !== file.source_url ||
        old.size !== file.size
      )
        return [{ file, change: "Changed" }];
      return [];
    }),
    ...previous
      .filter((f) => !now.has(JSON.stringify([f.release, f.filename])))
      .map((file) => ({ file, change: "Removed" })),
  ];
}

export function eta(t: Pick<Transfer, "state" | "total" | "bytes" | "speed">) {
  if (
    t.state !== "downloading" ||
    t.total == null ||
    t.speed == null ||
    t.speed <= 0 ||
    t.bytes >= t.total
  )
    return null;
  const seconds = Math.ceil((t.total - t.bytes) / t.speed);
  if (seconds < 60) return seconds + "s";
  if (seconds < 3600) return Math.ceil(seconds / 60) + "m";
  return (
    Math.floor(seconds / 3600) + "h " + Math.ceil((seconds % 3600) / 60) + "m"
  );
}
