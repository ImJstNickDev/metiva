import { useState } from "react";
import { useIsMutating, useMutation } from "@tanstack/react-query";
import {
  ChevronRight,
  File,
  FileJson,
  FolderTree,
  Link2,
  X,
} from "lucide-react";
import { api } from "./api";
import {
  bytes,
  type DocumentBranch,
  type DocumentCollection,
  type DocumentInspection,
  type FileRule,
  type NestedRule,
  type NestedSelection,
  type ReleaseFilter,
} from "./model";
import type { SourceInput } from "./SourcePicker";
import { Empty, Field, Form, Problem } from "./ui";

const displayPath = (path: string) => path || "(root)";
const rulePath = (rule: { path: string; url_path?: string }) =>
  displayPath(rule.path || rule.url_path || "");
type Sample = DocumentCollection["samples"][number];
type Inspect = (request: {
  collection?: string;
  filters?: ReleaseFilter[];
  row?: string;
  trail?: string[];
}) => Promise<DocumentInspection & { sample?: Sample }>;
type EditorProps = {
  busy: boolean;
  error: unknown;
  onReview: (selection: NestedSelection) => void;
};

export function DocumentPicker({
  id,
  input,
  inspection,
  initial,
  ...editor
}: EditorProps & {
  id: number;
  input: SourceInput;
  inspection: DocumentInspection;
  initial?: NestedSelection;
}) {
  const collections = inspection.collections;
  const [path, setPath] = useState(
    initial?.path ?? (collections.length === 1 ? collections[0].path : null),
  );
  const collection = collections.find((c) => c.path === path);
  const inspect: Inspect = (request) =>
    api(`runtimes/${id}/inspect-document`, "POST", { ...input, ...request });
  if (!collections.length)
    return (
      <Empty
        title="No release collections found"
        desc="If this document links to release lists, inspect its linked indexes."
      />
    );
  return (
    <div className="form document-picker">
      <Field label="Release collection">
        <select
          value={path === null ? "" : JSON.stringify(path)}
          disabled={editor.busy}
          onChange={(e) => setPath(JSON.parse(e.target.value))}
        >
          <option value="" disabled>
            Choose a collection
          </option>
          {collections.map((c) => (
            <option key={c.path} value={JSON.stringify(c.path)}>
              {displayPath(c.path)} · {c.count.toLocaleString()}{" "}
              {c.count === 1 ? "record" : "records"}
            </option>
          ))}
        </select>
      </Field>
      {collection && (
        <CollectionEditor
          key={collection.path}
          collection={collection}
          initial={initial?.path === path ? initial : undefined}
          inspect={inspect}
          {...editor}
        />
      )}
    </div>
  );
}

function FilterEditor({
  filters,
  fields,
  onChange,
}: {
  filters: ReleaseFilter[];
  fields: ReleaseFilter[];
  onChange: (value: ReleaseFilter[]) => void;
}) {
  const available = fields.filter(
    (field) => !filters.some((f) => f.path === field.path),
  );
  const update = (path: string, values: string[]) =>
    onChange(filters.map((f) => (f.path === path ? { path, values } : f)));
  return (
    <>
      {filters.map((filter) => {
        const field = fields.find((f) => f.path === filter.path);
        const choices = field?.values.length
          ? [...new Set([...field.values, ...filter.values])]
          : [];
        return (
          <div className="release-filter" key={filter.path}>
            <div className="branch-heading">
              <strong>
                Filter <code>{filter.path}</code>
              </strong>
              <button
                type="button"
                className="icon-btn"
                aria-label={`Remove filter ${filter.path}`}
                onClick={() =>
                  onChange(filters.filter((f) => f.path !== filter.path))
                }
              >
                <X size={15} />
              </button>
            </div>
            {choices.length > 0 && choices.length <= 24 ? (
              <fieldset
                className="picker-keys"
                aria-label={`Values for ${filter.path}`}
              >
                <div>
                  {choices.map((value) => (
                    <label className="check" key={value}>
                      <input
                        type="checkbox"
                        checked={filter.values.includes(value)}
                        onChange={(e) =>
                          update(
                            filter.path,
                            e.target.checked
                              ? [...filter.values, value]
                              : filter.values.filter((v) => v !== value),
                          )
                        }
                      />
                      {value}
                    </label>
                  ))}
                </div>
              </fieldset>
            ) : (
              <Field
                label={`Exact values for ${filter.path}`}
                hint="Enter one value per line."
              >
                <textarea
                  rows={2}
                  value={filter.values.join("\n")}
                  onChange={(e) =>
                    update(filter.path, e.target.value.split("\n"))
                  }
                />
              </Field>
            )}
          </div>
        );
      })}
      {available.length > 0 && (
        <Field label="Add release filter">
          <select
            value=""
            onChange={(e) =>
              onChange([...filters, { path: e.target.value, values: [] }])
            }
          >
            <option value="">Choose a field…</option>
            {available.map((f) => (
              <option key={f.path} value={f.path}>
                {f.path}
              </option>
            ))}
          </select>
        </Field>
      )}
    </>
  );
}

function incompleteRule(rules: NestedRule[]): string | undefined {
  for (const rule of rules) {
    if (rule.kind === "document") {
      if (!rule.rules.length)
        return `Choose a file in the linked document at ${rulePath(rule)}.`;
      const nested = incompleteRule(rule.rules);
      if (nested) return nested;
    } else {
      if (!!rule.digest_path !== !!rule.algorithm)
        return `Choose both a digest field and an algorithm for ${rulePath(rule)}.`;
      if (rule.url_template !== undefined) {
        const literal = rule.url_template
          .replaceAll("{digest}", "")
          .replaceAll("{prefix}", "");
        if (!rule.url_template.includes("{digest}") || /[{}]/.test(literal))
          return `Complete the URL template for ${rulePath(rule)} using {digest} and an optional {prefix}.`;
      }
    }
  }
}

function CollectionEditor({
  collection,
  initial,
  inspect,
  onReview,
  busy,
  error,
}: EditorProps & {
  collection: DocumentCollection;
  initial?: NestedSelection;
  inspect: Inspect;
}) {
  const inspecting = useIsMutating({ mutationKey: ["sample-document"] }) > 0;
  const [limit, setLimit] = useState(String(initial?.limit ?? ""));
  const [filters, setFilters] = useState<ReleaseFilter[]>(
    initial?.filters?.map((f) => ({ ...f, values: f.values.map(String) })) ??
      [],
  );
  const [rules, setRules] = useState<NestedRule[]>(initial?.rules ?? []);
  const version =
    (initial ? initial.version_path : collection.version_path) ?? "@key";
  const fields = collection.fields.some((f) => f.path === version)
    ? collection.fields
    : [...collection.fields, { path: version, values: [] }];
  const effectiveFilters = filters.map((f) => ({
    path: f.path,
    values: [...new Set(f.values.filter(Boolean))],
  }));
  const valid =
    effectiveFilters.every((f) => f.values.length) &&
    (!limit || (Number.isInteger(+limit) && +limit >= 1 && +limit <= 100000));
  const incomplete = incompleteRule(rules);
  function review() {
    onReview({
      path: collection.path,
      ...(version !== "@key" && { version_path: version }),
      ...(effectiveFilters.length > 0 && { filters: effectiveFilters }),
      ...(limit && { limit: Number(limit) }),
      rules,
    });
  }
  return (
    <Form
      submit="Preview selected files"
      busy={busy}
      error={error}
      disabled={inspecting || !valid || !rules.length || !!incomplete}
      onSubmit={review}
    >
      <fieldset disabled={busy || inspecting}>
        <div className="scope-settings">
          <Field
            label="Sync release limit"
            hint="Applies after filtering, in collection order. Leave blank for all matching releases."
          >
            <input
              type="number"
              min="1"
              max="100000"
              placeholder="All matching releases"
              value={limit}
              onChange={(e) => setLimit(e.target.value)}
            />
          </Field>
          <FilterEditor
            filters={filters}
            fields={fields}
            onChange={setFilters}
          />
        </div>
        <SampleEditor
          key={JSON.stringify([collection.path, effectiveFilters, limit])}
          inspect={inspect}
          collection={collection}
          filters={effectiveFilters}
          limited={!!limit}
          enabled={valid}
          rules={rules}
          onChange={setRules}
        />
        <small className="hint">The preview checks up to 5 releases.</small>
        {incomplete && (
          <small role="status" className="hint">
            {incomplete}
          </small>
        )}
      </fieldset>
    </Form>
  );
}

function SampleEditor({
  inspect,
  collection,
  filters,
  limited,
  enabled,
  rules,
  onChange,
}: {
  inspect: Inspect;
  collection: DocumentCollection;
  filters: ReleaseFilter[];
  limited: boolean;
  enabled: boolean;
  rules: NestedRule[];
  onChange: (rules: NestedRule[]) => void;
}) {
  const [row, setRow] = useState("");
  const matches = collection.samples.filter((s) =>
    filters.every((f) =>
      f.values.includes(
        f.path === (collection.version_path || "@key")
          ? s.id
          : s.fields[f.path],
      ),
    ),
  );
  const sample = useMutation({
    mutationKey: ["sample-document"],
    mutationFn: () =>
      inspect(row ? { row } : { collection: collection.path, filters }),
  });
  const inspected = row
    ? matches.find((s) => s.pointer === row)
    : sample.data?.sample;
  return (
    <div className="sample-editor">
      <div className="branch-heading">
        <h3>
          <FolderTree size={17} /> File selection
        </h3>
        {rules.length > 0 && (
          <small>
            {countFiles(rules)} file{" "}
            {countFiles(rules) === 1 ? "rule" : "rules"}
          </small>
        )}
      </div>
      {!limited && matches.length > 0 && (
        <Field label="Sample release">
          <select
            value={row}
            onChange={(e) => {
              setRow(e.target.value);
              sample.reset();
            }}
          >
            <option value="">First matching release</option>
            {matches.map((s) => (
              <option key={s.pointer} value={s.pointer}>
                {s.id}
              </option>
            ))}
          </select>
        </Field>
      )}
      {!sample.data && (
        <>
          {rules.length > 0 && <RuleSummary rules={rules} />}
          <small className="hint">
            Inspect one matching release to choose the files and linked
            documents to read on each sync.
          </small>
          <div>
            <button
              type="button"
              className="btn"
              disabled={!enabled || sample.isPending}
              onClick={() => sample.mutate()}
            >
              <FileJson size={15} />
              {sample.isPending ? "Inspecting…" : "Inspect sample release"}
            </button>
          </div>
        </>
      )}
      <Problem error={sample.error} />
      {sample.data &&
        (inspected ? (
          <BranchEditor
            key={inspected.pointer}
            inspect={inspect}
            sample={inspected}
            root={sample.data}
            rules={rules}
            onChange={onChange}
          />
        ) : (
          <p role="status" className="hint">
            No release matches these filters. Change the filter values to
            inspect a release.
          </p>
        ))}
    </div>
  );
}

const countFiles = (rules: NestedRule[]): number =>
  rules.reduce((n, r) => n + (r.kind === "file" ? 1 : countFiles(r.rules)), 0);
function RuleSummary({ rules }: { rules: NestedRule[] }) {
  return (
    <ul className="rule-summary">
      {rules.map((r, i) => (
        <li key={i}>
          <code>{rulePath(r)}</code> ·{" "}
          {r.kind === "document"
            ? "Linked JSON"
            : r.url_template
              ? "File from digest"
              : "File"}
          {r.kind === "document" && r.rules.length > 0 && (
            <RuleSummary rules={r.rules} />
          )}
        </li>
      ))}
    </ul>
  );
}

type Step = {
  index: number;
  path: string;
  pointer: string;
  inspection: DocumentInspection;
};
function changeRules(
  tree: NestedRule[],
  steps: Step[],
  rules: NestedRule[],
): NestedRule[] {
  if (!steps.length) return rules;
  const [step, ...rest] = steps;
  return tree.map((r, i) =>
    i === step.index && r.kind === "document"
      ? { ...r, rules: changeRules(r.rules, rest, rules) }
      : r,
  );
}

const branchMatches = (b: DocumentBranch, r: NestedRule) =>
  b.path === r.path &&
  (r.url_path === undefined || b.url_path === r.url_path) &&
  (b.kind === "digest") === (r.kind === "file" && r.url_template !== undefined);
function fileRule(branch: DocumentBranch): FileRule {
  const { path, url_path, name_path, size_path, digest_path, algorithm } =
    branch;
  return {
    path,
    kind: "file",
    ...(url_path !== undefined && { url_path }),
    ...(name_path && { name_path }),
    ...(size_path && { size_path }),
    ...(digest_path && { digest_path }),
    ...(algorithm && { algorithm }),
    ...(path.split("/").includes("*") && { required: false }),
    ...(branch.kind === "digest" && { url_template: "" }),
  };
}
function BranchEditor({
  inspect,
  sample,
  root,
  rules: tree,
  onChange,
}: {
  inspect: Inspect;
  sample: Sample;
  root: DocumentInspection;
  rules: NestedRule[];
  onChange: (rules: NestedRule[]) => void;
}) {
  const [steps, setSteps] = useState<Step[]>([]);
  const [choice, setChoice] = useState("");
  const inspection = steps.at(-1)?.inspection ?? root;
  const rules = steps.reduce((rs, step) => {
    const parent = rs[step.index];
    return parent?.kind === "document" ? parent.rules : [];
  }, tree);
  const update = (next: NestedRule[]) =>
    onChange(changeRules(tree, steps, next));
  const candidates = inspection.branches.filter(
    (b) => !rules.some((r) => branchMatches(b, r)),
  );
  const branch =
    choice !== ""
      ? candidates[Number(choice)]
      : candidates.length === 1
        ? candidates[0]
        : undefined;
  const follow = useMutation({
    mutationKey: ["sample-document"],
    mutationFn: ({ branch }: { branch: DocumentBranch; index: number }) =>
      inspect({
        trail: [...steps.map((s) => s.pointer), branch.sample_pointer],
      }),
    onSuccess: (result, { branch, index }) => {
      if (index === rules.length)
        update([
          ...rules,
          {
            path: branch.path,
            kind: "document",
            ...(branch.url_path !== undefined && { url_path: branch.url_path }),
            ...(branch.digest_path && {
              digest_path: branch.digest_path,
              algorithm: branch.algorithm,
            }),
            ...(branch.size_path && { size_path: branch.size_path }),
            rules: [],
            ...(branch.path.split("/").includes("*") && { required: false }),
          },
        ]);
      setSteps([
        ...steps,
        {
          index,
          path: rulePath(branch),
          pointer: branch.sample_pointer,
          inspection: result,
        },
      ]);
      setChoice("");
    },
  });
  const selectStep = (length: number) => {
    setSteps(steps.slice(0, length));
    setChoice("");
    follow.reset();
  };
  function addFile() {
    if (!branch) return;
    update([...rules, fileRule(branch)]);
    setChoice("");
    follow.reset();
  }
  return (
    <fieldset className="branch-browser" disabled={follow.isPending}>
      <nav className="document-trail" aria-label="Sample document path">
        <button
          type="button"
          disabled={!steps.length}
          onClick={() => selectStep(0)}
        >
          {sample.id}
        </button>
        {steps.map((s, i) => (
          <span key={i}>
            <ChevronRight size={13} />
            <button
              type="button"
              disabled={i === steps.length - 1}
              onClick={() => selectStep(i + 1)}
            >
              {s.path}
            </button>
          </span>
        ))}
        <small>{bytes(inspection.document_bytes)}</small>
      </nav>
      {rules.length > 0 && (
        <ul className="chosen-branches">
          {rules.map((rule, i) => {
            const matched = inspection.branches.find((b) =>
              branchMatches(b, rule),
            );
            return (
              <li
                key={JSON.stringify([rule.path, rule.url_path, rule.kind, i])}
              >
                <div className="branch-heading">
                  <span>
                    {rule.kind === "document" ? (
                      <Link2 size={15} />
                    ) : (
                      <File size={15} />
                    )}
                    <code>{rulePath(rule)}</code>
                  </span>
                  <button
                    type="button"
                    className="icon-btn"
                    aria-label={`Remove ${rulePath(rule)}`}
                    onClick={() => {
                      update(rules.filter((_, n) => n !== i));
                      setChoice("");
                    }}
                  >
                    <X size={15} />
                  </button>
                </div>
                {rule.kind === "document" ? (
                  <div className="linked-rule">
                    <small>
                      Linked JSON · {countFiles(rule.rules)} file rules
                    </small>
                    {matched ? (
                      <button
                        className="text-button"
                        type="button"
                        onClick={() =>
                          follow.mutate({ branch: matched, index: i })
                        }
                      >
                        Explore document
                      </button>
                    ) : (
                      <small>Unavailable in this sample</small>
                    )}
                  </div>
                ) : (
                  <FileOptions
                    rule={rule}
                    update={(next) =>
                      update(rules.map((r, n) => (n === i ? next : r)))
                    }
                  />
                )}
                <label className="check optional-rule">
                  <input
                    type="checkbox"
                    checked={rule.required !== false}
                    onChange={(e) =>
                      update(
                        rules.map((r, n) =>
                          i === n ? { ...r, required: e.target.checked } : r,
                        ),
                      )
                    }
                  />
                  Require this branch in every match
                </label>
              </li>
            );
          })}
        </ul>
      )}
      {candidates.length > 0 && (
        <div className="branch-choice">
          <Field label="Document field">
            <select
              value={branch ? candidates.indexOf(branch) : ""}
              onChange={(e) => {
                setChoice(e.target.value);
                follow.reset();
              }}
            >
              <option value="" disabled>
                Choose a field…
              </option>
              {candidates.map((b, i) => (
                <option value={i} key={i}>
                  {rulePath(b)}
                  {b.path && b.url_path ? " · " + b.url_path : ""}
                  {b.kind === "digest" ? " · digest" : ""}
                </option>
              ))}
            </select>
          </Field>
          {branch && (
            <>
              <small className="sample-value">
                <span>
                  Sample {branch.kind === "digest" ? "digest" : "URL"}
                </span>
                <code>{branch.sample}</code>
              </small>
              <div className="branch-actions">
                <button className="btn small" type="button" onClick={addFile}>
                  <File size={14} />
                  {branch.kind === "url" ? "Use as file" : "Map digest to file"}
                </button>
                {branch.kind === "url" && steps.length < 7 && (
                  <button
                    type="button"
                    className="btn small"
                    onClick={() =>
                      follow.mutate({ branch, index: rules.length })
                    }
                  >
                    <Link2 size={14} />
                    Follow JSON link
                  </button>
                )}
              </div>
            </>
          )}
        </div>
      )}
      {inspection.truncated && (
        <small className="hint">
          Showing fields from a limited sample of this document.
        </small>
      )}
      {!inspection.branches.length && (
        <p className="hint">
          This sample contains no recognized file URLs or digests.
        </p>
      )}
      {follow.isPending && (
        <small role="status">Reading linked document…</small>
      )}
      <Problem error={follow.error} />
      {steps.length > 0 && (
        <button
          type="button"
          className="text-button parent-document"
          onClick={() => selectStep(steps.length - 1)}
        >
          Back to parent document
        </button>
      )}
    </fieldset>
  );
}

function FileOptions({
  rule,
  update,
}: {
  rule: FileRule;
  update: (r: FileRule) => void;
}) {
  return (
    <>
      {rule.url_template !== undefined && (
        <Field
          label="File URL template"
          hint="Use {digest} for the full digest and {prefix} for its first two characters."
        >
          <input
            type="text"
            required
            placeholder="https://files.example.org/{prefix}/{digest}"
            value={rule.url_template}
            onChange={(e) => update({ ...rule, url_template: e.target.value })}
          />
        </Field>
      )}
      <details className="file-options">
        <summary>File naming and integrity</summary>
        <Field
          label="Filename field"
          hint="Use a field path or @key for a map key. Leave blank to infer the filename."
        >
          <input
            value={rule.name_path ?? ""}
            placeholder="/name"
            onChange={(e) =>
              update({ ...rule, name_path: e.target.value || undefined })
            }
          />
        </Field>
        <Field label="Size field">
          <input
            value={rule.size_path ?? ""}
            placeholder="Automatic"
            onChange={(e) =>
              update({ ...rule, size_path: e.target.value || undefined })
            }
          />
        </Field>
        <div className="field-grid">
          <Field label="Digest field">
            <input
              value={rule.digest_path ?? ""}
              placeholder="Automatic"
              onChange={(e) =>
                update({ ...rule, digest_path: e.target.value || undefined })
              }
            />
          </Field>
          <Field label="Digest algorithm">
            <select
              value={rule.algorithm ?? ""}
              onChange={(e) =>
                update({ ...rule, algorithm: e.target.value || undefined })
              }
            >
              <option value="">Automatic</option>
              <option value="sha1">SHA-1</option>
              <option value="sha256">SHA-256</option>
              <option value="sha384">SHA-384</option>
              <option value="sha512">SHA-512</option>
              <option value="md5">MD5</option>
            </select>
          </Field>
        </div>
      </details>
    </>
  );
}
