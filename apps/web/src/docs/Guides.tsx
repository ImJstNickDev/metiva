import { ArrowRight, BookOpen, Compass, Fingerprint, ListTree } from "lucide-react";
import { Sample } from "./Sample";
import { referenceLink, type Operation } from "./contract";

export const guides = [
  { id: "first-download", title: "Your first download", intro: "Start with a mirror URL and finish with a verified file on disk.", Icon: BookOpen },
  { id: "find-runtime", title: "Find a runtime", intro: "Discover projects and check which runtimes have published content.", Icon: Compass },
  { id: "read-publication", title: "Read a publication", intro: "Work with release identifiers, artifact URLs and checksums.", Icon: ListTree },
  { id: "private-download", title: "Use a scoped key", intro: "Read private content with a key issued for your project or runtime.", Icon: Fingerprint },
];
export const guideLink = (id: string) => "#/guides/" + id;
export function Related({ links }: { links: [string, string][] }) {
  return <section className="related"><h2>Related reference</h2>{links.map(([id, title]) =>
    <a href={referenceLink(id)} key={id}>{title}<ArrowRight size={15}/></a>
  )}</section>;
}
function FirstDownload({ origin, ops }: {origin: string; ops: Operation[]}) {
  const example = ops.find((op) => op.operationId === "listArtifacts")?.responses["200"]?.content?.["application/json"]?.example;
  return <>
    <p className="prereq"><strong>Before you begin</strong> Use a shell with curl, jq and sha256sum. Choose a public runtime that has published at least one artifact.</p>
    <h2>1. Find the project</h2>
    <p>Set the mirror URL and list its public project slugs. Keep this shell open for the remaining steps.</p>
    <Sample shell label="List projects" text={`mirror="${origin}"\ncurl -fsS "$mirror/api/v1/projects" | jq -r '.[].slug'`}/>
    <h2>2. Choose a runtime</h2>
    <p>Set <code>project</code> to a slug from the list. The example project is <code>toolkit</code>. Copy a runtime slug from the next response.</p>
    <Sample shell label="List runtimes" text={`project=toolkit\ncurl -fsS "$mirror/api/v1/projects/$project/runtimes" | jq -r '.[].slug'`}/>
    <h2>3. Pick an artifact</h2>
    <p>Set <code>runtime</code> to your runtime slug. This example selects the first artifact in its published manifest and saves the metadata to <code>artifact.json</code>.</p>
    <Sample shell label="Read artifact metadata" text={`runtime=portable\ncurl -fsS "$mirror/p/$project/r/$runtime/manifest.json" |\n  jq -e 'first(.releases[].artifacts[])' > artifact.json\ncat artifact.json`}/>
    <p>Use <code>url</code> for the download, <code>size</code> for its byte length and <code>sha256</code> to verify the bytes.</p>
    {Array.isArray(example) && example.length > 0 && <details className="field-details"><summary>Example artifact record</summary><Sample label="Artifact JSON" text={JSON.stringify(example[0], null, 2)}/></details>}
    <h2>4. Download and verify</h2>
    <p>Run these commands in a directory where you can write <code>artifact.bin</code>. A matching checksum prints <code>artifact.bin: OK</code>.</p>
    <Sample shell label="Save and verify the file" text={`curl -fsS --output artifact.bin "$(jq -er '.url' artifact.json)" &&\n  jq -er '.sha256 + "  artifact.bin"' artifact.json | sha256sum -c -`}/>
    <Related links={[["listProjects","List projects"],["getManifest","Normalized manifest"],["downloadArtifact","Artifact download"]]}/>
  </>;
}
export function Guide({ id, origin, ops }: { id: string; origin: string; ops: Operation[] }) {
  const guide = guides.find((g) => g.id === id)!;
  return <><h1>{guide.title}</h1><p className="lede">{guide.intro}</p>
    {id === "first-download" && <FirstDownload origin={origin} ops={ops}/>}
    {id === "find-runtime" && <>
      <h2>Follow the slugs</h2><p>Projects group runtimes. Use the <code>slug</code> fields from discovery responses in subsequent URLs. Each list contains the resources visible to the caller.</p>
      <Sample shell label="Discover runtimes" text={`mirror="${origin}"\ncurl -fsS "$mirror/api/v1/projects"\nproject=toolkit\ncurl -fsS "$mirror/api/v1/projects/$project/runtimes"`}/>
      <h2>Check availability</h2><p>Replace the example slugs with your selections. When <code>available</code> is true, the runtime has a current publication. <code>last_success</code> records the last successful sync in Unix seconds and can be null.</p>
      <Sample shell label="Check a publication" text={`runtime=portable\ncurl -fsS "$mirror/api/v1/projects/$project/runtimes/$runtime/availability"`}/>
      <Related links={[["listProjects","List projects"],["listRuntimes","List runtimes"],["getAvailability","Publication availability"]]}/>
    </>}
    {id === "read-publication" && <>
      <h2>Read the normalized manifest</h2><p>Use your mirror's project and runtime slugs in this path. Each entry in <code>releases</code> has a <code>version</code> and an <code>artifacts</code> array. The manifest includes verified files retained from earlier syncs.</p>
      <Sample shell label="Read release files" text={`mirror="${origin}"\ncurl -fsS "$mirror/p/toolkit/r/portable/manifest.json" |\n  jq '.releases[] | {version, files: [.artifacts[].filename]}'`}/>
      <h2>Identify a file</h2><p>Fetch the <code>url</code> supplied by the artifact record. The <code>sha256</code> identifies the published bytes and <code>size</code> gives their length. A correction can change the content at an existing URL. Nested sources may also provide an original <code>logical_path</code>.</p>
      <h2>Read an upstream-shaped manifest</h2><p>For a compatible source, <code>upstream.json</code> preserves its document layout with rewritten mirror URLs. A runtime can have a normalized publication while this optional representation returns 404.</p>
      <Related links={[["getManifest","Normalized manifest"],["getUpstreamManifest","Upstream-shaped manifest"],["listArtifacts","Release artifacts"]]}/>
    </>}
    {id === "private-download" && <>
      <h2>Get a read-only key</h2><p>Ask the mirror administrator for a key scoped to the runtime or project you need. A private project restricts every child runtime. A public project can contain private runtimes.</p>
      <h2>Read the private manifest</h2><p>Load your key into <code>METIVA_KEY</code> from your secret store. Replace the example slugs with your assigned project and runtime. Use the Authorization header for each private request.</p>
      <Sample shell label="Authenticated manifest request" text={`mirror="${origin}"\ncurl -fsS -H "Authorization: Bearer \${METIVA_KEY:?Set METIVA_KEY}" \\\n  "$mirror/p/toolkit/r/portable/manifest.json" > manifest.json`}/>
      <h2>Fetch the private artifact</h2><p>Choose an artifact from the manifest. This command sends the key only when its URL belongs to the same mirror.</p>
      <Sample shell label="Authenticated download" text={`url=$(jq -er 'first(.releases[].artifacts[]).url' manifest.json)
case "$url" in
  "$mirror"/*) curl -fsS -H "Authorization: Bearer $METIVA_KEY" \\
                 --output artifact.bin "$url" ;;
  *) printf '%s\\n' "Unexpected artifact origin" >&2 ;;
esac`}/>
      <p>A 401 response means credentials are missing, invalid or revoked. A 403 means this key cannot read the requested private resource.</p>
      <Related links={[["getManifest","Normalized manifest"],["downloadArtifact","Artifact download"]]}/>
    </>}
  </>;
}
