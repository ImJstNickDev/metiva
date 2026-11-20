import { useState } from "react";
import { useMutation } from "@tanstack/react-query";
import { FileJson, Layers3 } from "lucide-react";
import { api, useAction } from "./api";
import {
  bytes,
  type ArchiveFile,
  type Discovery,
  type Manifest,
  type Source,
  type SourceMapping,
} from "./model";
import { SourcePicker, type SourceInput } from "./SourcePicker";
import { Dialog, Field, Form, Tabs, str, useNotice } from "./ui";

const fields = [
  ["releases_path", "Release list", "/releases"],
  ["version_path", "Release version", "/version"],
  ["artifacts_path", "Artifact list", "/artifacts"],
  ["url_path", "Artifact URL", "/url"],
  ["filename_path", "Filename", "/filename"],
  ["size_path", "Size in bytes", "/size"],
  ["sha256_path", "SHA-256", "/sha256"],
  ["next_path", "Next page URL", "/next"],
] as const;
type Input = SourceInput & { mapping?: SourceMapping };
type Review = { input: Input; manifest: Manifest };

export function ImportSource({
  id,
  onClose,
  source,
}: {
  id: number;
  onClose: () => void;
  source?: Source;
}) {
  const mapping: SourceMapping =
    typeof source?.mapping === "string"
      ? (JSON.parse(source.mapping) ?? {})
      : (source?.mapping ?? {});
  const saved =
    "selection" in mapping && typeof mapping.selection === "object"
      ? mapping.selection
      : undefined;
  const manual = mapping as Record<string, string>;
  const [kind, setKind] = useState<"Remote URL" | "Upload JSON">(
    source?.kind === "upload" ? "Upload JSON" : "Remote URL",
  );
  const [mode, setMode] = useState(
    source && !saved
      ? Object.keys(mapping).length
        ? "manual"
        : "auto"
      : "pick",
  );
  const [discovery, setDiscovery] = useState<{
    input: SourceInput;
    result: Discovery;
  } | null>(null);
  const [review, setReview] = useState<Review | null>(null);
  const notice = useNotice();
  async function preview(input: Input) {
    const manifest = await api<Manifest>(
      `runtimes/${id}/preview`,
      "POST",
      input,
    );
    setReview({ input, manifest });
  }
  const inspect = useMutation({
    mutationFn: async (data: FormData) => {
      let input: SourceInput;
      if (kind === "Remote URL") input = { url: str(data, "url").trim() };
      else {
        const file = data.get("file");
        if (!(file instanceof File) || !file.size)
          throw new Error("Select a JSON file.");
        input = { upload: await file.text() };
      }
      if (mode === "pick") {
        const result = await api<Discovery>(
          `runtimes/${id}/discover`,
          "POST",
          input,
        );
        setDiscovery({ input, result });
      } else if (mode === "manual") {
        const custom: Record<string, string> = {};
        for (const [key] of fields) {
          const value = str(data, key).trim();
          if (value) custom[key] = value === "$" ? "" : value;
        }
        await preview({ ...input, mapping: custom });
      } else {
        await preview(input);
      }
    },
  });
  const selectedPreview = useMutation({ mutationFn: preview });
  const save = useAction(
    () =>
      api(
        source ? `sources/${source.id}` : `runtimes/${id}/sources`,
        source ? "PATCH" : "POST",
        review!.input,
      ),
    () => {
      notice(source ? "Source updated" : "Source imported");
      onClose();
    },
  );
  const busy = inspect.isPending || selectedPreview.isPending || save.isPending;
  return (
    <Dialog
      title={
        review
          ? "Review source"
          : discovery
            ? "Choose files to mirror"
            : source
              ? "Edit source"
              : "Import source"
      }
      desc={
        review
          ? "Review the files included by this source before saving."
          : discovery
            ? discovery.input.url || "Uploaded JSON manifest"
            : "Import an upstream manifest from a URL or a JSON file."
      }
      onClose={onClose}
      busy={busy}
    >
      <div hidden={!!discovery || !!review}>
        <Form
          submit={mode === "pick" ? "Inspect manifest" : "Preview manifest"}
          onSubmit={inspect.mutate}
          busy={inspect.isPending}
          error={inspect.error}
        >
          <fieldset disabled={busy}>
            <Tabs
              items={["Remote URL", "Upload JSON"] as const}
              current={kind}
              onChange={setKind}
            />
            {kind === "Remote URL" ? (
              <Field label="Manifest URL">
                <input
                  name="url"
                  type="url"
                  defaultValue={source?.url || ""}
                  required
                  placeholder="https://releases.example.org/manifest.json"
                />
              </Field>
            ) : (
              <Field label="Manifest file">
                <input
                  name="file"
                  type="file"
                  accept=".json,application/json"
                  required
                />
              </Field>
            )}
            <Field label="File selection">
              <select
                value={mode}
                onChange={(e) => {
                  setMode(e.target.value);
                  inspect.reset();
                }}
              >
                <option value="pick">
                  Choose release lists, channels and platforms
                </option>
                <option value="auto">Automatic detection</option>
                <option value="manual">Manual field mapping</option>
              </select>
            </Field>
            {mode === "manual" && (
              <>
                <p className="hint">
                  Use JSON pointers or dotted paths. Leave a field blank for
                  automatic detection, or enter $ to select the current value.
                </p>
                <div className="field-grid">
                  {fields.map(([key, label, placeholder]) => (
                    <Field key={key} label={label}>
                      <input
                        name={key}
                        defaultValue={manual[key] === "" ? "$" : manual[key]}
                        placeholder={placeholder}
                      />
                    </Field>
                  ))}
                </div>
              </>
            )}
          </fieldset>
        </Form>
      </div>
      {discovery && (
        <div hidden={!!review}>
          <SourcePicker
            id={id}
            input={discovery.input}
            discovery={discovery.result}
            initial={saved}
            busy={selectedPreview.isPending}
            error={selectedPreview.error}
            onReview={(selection) =>
              selectedPreview.mutate({
                ...discovery.input,
                mapping: { selection },
              })
            }
          />
          <button
            type="button"
            className="text-button back-link"
            disabled={busy}
            onClick={() => {
              setDiscovery(null);
              selectedPreview.reset();
            }}
          >
            Back to source
          </button>
        </div>
      )}
      {review && (
        <Form
          submit={source ? "Save source" : "Import source"}
          onSubmit={save.mutate}
          busy={save.isPending}
          error={save.error}
          secondary={
            <button
              className="btn"
              type="button"
              disabled={save.isPending}
              onClick={() => {
                setReview(null);
                save.reset();
              }}
            >
              {discovery ? "Change selection" : "Back to source"}
            </button>
          }
        >
          <ManifestView manifest={review.manifest} review />
        </Form>
      )}
    </Dialog>
  );
}

export function ManifestView({
  manifest,
  review = false,
  files,
  onInspect,
}: {
  manifest: Manifest;
  review?: boolean;
  files?: ArchiveFile[];
  onInspect?: (file: ArchiveFile) => void;
}) {
  const [shown, setShown] = useState(25);
  const releases = manifest.releases;
  const published = new Map(
    files?.map((f) => [JSON.stringify([f.release, f.filename]), f]),
  );
  const artifacts = releases.flatMap((release) => release.artifacts);
  const unknown = artifacts.filter((a) => a.size == null).length;
  const total = artifacts.reduce((sum, a) => sum + (a.size ?? 0), 0);
  return (
    <div className="manifest-list">
      <div className="inventory-summary">
        <Layers3 size={16} />
        <span>
          {releases.length} releases · {artifacts.length} files ·{" "}
          {unknown ? bytes(total) + " known" : bytes(total) + " total"}
        </span>
      </div>
      {unknown > 0 && (
        <small>
          {unknown} {unknown === 1 ? "file has" : "files have"} no upstream
          size.
        </small>
      )}
      {releases.slice(0, shown).map((r, i) => (
        <details key={r.version + i} open={releases.length === 1 || review}>
          <summary>
            {r.version}
            {typeof r.channel === "string" && r.channel && (
              <small className="channel-tag">{r.channel}</small>
            )}
            <span>{r.artifacts.length} files</span>
          </summary>
          {r.artifacts.map((a, j) => {
            const file = published.get(JSON.stringify([r.version, a.filename]));
            return (
              <div className="artifact" key={j}>
                <FileJson size={16} />
                <div>
                  {file && onInspect ? (
                    <button
                      type="button"
                      className="text-button file-link"
                      onClick={() => onInspect(file)}
                    >
                      {a.filename}
                    </button>
                  ) : (
                    <strong>{a.filename}</strong>
                  )}
                  <small className="mono wrap">{a.url}</small>
                  {a.sha256 && (
                    <small className="mono wrap">SHA-256 {a.sha256}</small>
                  )}
                </div>
                <small>{bytes(a.size)}</small>
              </div>
            );
          })}
        </details>
      ))}
      {shown < releases.length && (
        <button
          type="button"
          className="btn small"
          onClick={() => setShown((n) => n + 25)}
        >
          Show more releases
        </button>
      )}
    </div>
  );
}
