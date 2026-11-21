import { createHash } from "node:crypto";
import { readdir, readFile, writeFile } from "node:fs/promises";

// Vite has finished emitting the static build. Only these build artifacts are
// eligible for Cache Storage. Dashboard reads and downloads never enter it.
const files = (await readdir("dist", { recursive: true, withFileTypes: true }))
  .filter((entry) => entry.isFile())
  .map((entry) => (entry.parentPath + "/" + entry.name).replace(/^dist\//, "/"))
  .filter((file) => !file.startsWith("/docs/") && !file.startsWith("/assets/docs-"))
  .sort();
const version = createHash("sha256");
for (const file of files)
  version.update(file).update(await readFile("dist" + file));
const worker = await readFile("dashboard-worker.js", "utf8");
await writeFile(
  "dist/dashboard-worker.js",
  "const shellCache = " +
    JSON.stringify("metiva-dashboard-" + version.digest("hex")) +
    ";\n" +
    "const shellFiles = " +
    JSON.stringify(files) +
    ";\n" +
    worker,
);
