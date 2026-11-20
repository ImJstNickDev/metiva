import { useState } from "react";
import { useMutation } from "@tanstack/react-query";
import { api } from "./api";
import type {
  ArtifactGroup,
  Discovery,
  ReleaseCollection,
  Selection,
  SourceIndex,
} from "./model";
import { Empty, Field, Form, Problem } from "./ui";

export type SourceInput = { url?: string; upload?: string };
const pointer = (path: string) => path || "(root)";

function KeyChoices({
  label,
  available,
  selected,
  onChange,
}: {
  label: string;
  available: string[];
  selected: string[];
  onChange: (keys: string[]) => void;
}) {
  return (
    <fieldset className="picker-keys">
      <legend>{label}</legend>
      <div>
        {available.map((key) => (
          <label key={key} className="check">
            <input
              type="checkbox"
              checked={selected.includes(key)}
              onChange={(e) =>
                onChange(
                  e.target.checked
                    ? [...selected, key]
                    : selected.filter((k) => k !== key),
                )
              }
            />
            {key}
          </label>
        ))}
      </div>
    </fieldset>
  );
}

function CollectionFields({
  collection,
  index,
  initial,
  onReview,
  busy,
  error,
}: {
  collection: ReleaseCollection;
  index?: SourceIndex;
  initial?: Selection;
  onReview: (rule: Selection) => void;
  busy: boolean;
  error: unknown;
}) {
  const [groups, setGroups] = useState<ArtifactGroup[]>(() =>
    collection.groups.flatMap((group) => {
      const saved = initial?.groups.find((g) => g.path === group.path);
      if (saved) {
        const keys = group.keys.filter((key) => saved.keys.includes(key));
        if (!group.keys.length || keys.length) return [{ ...group, keys }];
      }
      if (!initial && collection.groups.length === 1 && !group.keys.length)
        return [group];
      return [];
    }),
  );
  const channelOptions = !index && collection.channels;
  const [channels, setChannels] = useState<string[]>(() =>
    channelOptions
      ? channelOptions.keys.filter((k) => initial?.channels?.keys.includes(k))
      : [],
  );
  function updateGroup(path: string, value?: ArtifactGroup) {
    setGroups((old) => [
      ...old.filter((g) => g.path !== path),
      ...(value ? [value] : []),
    ]);
  }
  function preview() {
    onReview({
      ...(index && { index }),
      path: collection.path,
      version_path: collection.version_path,
      ...(channelOptions && {
        channels: { path: channelOptions.path, keys: channels },
      }),
      groups,
    });
  }
  return (
    <Form
      submit="Preview selected files"
      busy={busy}
      error={error}
      disabled={!groups.length || (!!channelOptions && !channels.length)}
      onSubmit={preview}
    >
      <fieldset disabled={busy}>
        {channelOptions && (
          <KeyChoices
            label="Release channels"
            available={channelOptions.keys}
            selected={channels}
            onChange={setChannels}
          />
        )}
        <fieldset className="picker-groups">
          <legend>Artifact groups and platforms</legend>
          {collection.groups.map((group) => {
            const selected = groups.find((g) => g.path === group.path);
            const label = pointer(group.path);
            return group.keys.length ? (
              <KeyChoices
                key={group.path}
                label={label}
                available={group.keys}
                selected={selected?.keys ?? []}
                onChange={(keys) =>
                  updateGroup(
                    group.path,
                    keys.length ? { path: group.path, keys } : undefined,
                  )
                }
              />
            ) : (
              <label className="check" key={group.path}>
                <input
                  type="checkbox"
                  checked={!!selected}
                  onChange={(e) =>
                    updateGroup(
                      group.path,
                      e.target.checked ? group : undefined,
                    )
                  }
                />
                {label}
              </label>
            );
          })}
        </fieldset>
      </fieldset>
    </Form>
  );
}

export function SourcePicker({
  id,
  input,
  discovery,
  initial,
  busy,
  error,
  onReview,
}: {
  id: number;
  input: SourceInput;
  discovery: Discovery;
  initial?: Selection;
  busy: boolean;
  error: unknown;
  onReview: (rule: Selection) => void;
}) {
  const { indexes, collections } = discovery;
  const options = [
    ...indexes.map((index, i) => ({
      value: "index-" + i,
      label: "Linked index · " + pointer(index.path),
      matches:
        !!initial?.index &&
        initial.index.path === index.path &&
        initial.index.link_path === index.link_path,
    })),
    ...collections.map((c, i) => ({
      value: "direct-" + i,
      label: "Direct release list · " + pointer(c.path),
      matches:
        initial &&
        !initial.index &&
        initial.path === c.path &&
        initial.version_path === c.version_path,
    })),
  ];
  const [choice, setChoice] = useState(
    options.find((o) => o.matches)?.value ??
      (options.length === 1 ? options[0].value : ""),
  );
  const index = choice.startsWith("index-")
    ? indexes[Number(choice.slice(6))]
    : undefined;
  const direct = choice.startsWith("direct-")
    ? collections[Number(choice.slice(7))]
    : undefined;
  const [keys, setKeys] = useState(
    () => index?.keys.filter((k) => initial?.index?.keys.includes(k)) ?? [],
  );
  const [remotePath, setRemotePath] = useState("");
  const linked = useMutation({
    mutationFn: () =>
      api<Discovery>(`runtimes/${id}/discover`, "POST", {
        ...input,
        index: { ...index, keys },
      }),
    onSuccess: (result) => {
      const savedIndex = result.collections.findIndex(
        (c) =>
          c.path === initial?.path && c.version_path === initial.version_path,
      );
      setRemotePath(
        savedIndex >= 0
          ? String(savedIndex)
          : result.collections.length === 1
            ? "0"
            : "",
      );
    },
  });
  if (!options.length)
    return (
      <Empty
        title="No selectable release lists"
        desc="Return to the source and choose automatic detection or manual field mapping."
      />
    );
  const collection =
    direct ??
    (remotePath !== ""
      ? linked.data?.collections[Number(remotePath)]
      : undefined);
  return (
    <div className="form">
      <fieldset disabled={linked.isPending || busy}>
        <Field
          label="Release inventory"
          hint="A linked index follows the selected links on every sync. A direct list reads releases from this document."
        >
          <select
            value={choice}
            onChange={(e) => {
              setChoice(e.target.value);
              setKeys([]);
              setRemotePath("");
              linked.reset();
            }}
          >
            <option value="" disabled>
              Choose an inventory
            </option>
            {options.map((o) => (
              <option key={o.value} value={o.value}>
                {o.label}
              </option>
            ))}
          </select>
        </Field>
        {index && (
          <>
            {index.keys.length > 0 && (
              <KeyChoices
                label="Linked channels"
                available={index.keys}
                selected={keys}
                onChange={(value) => {
                  setKeys(value);
                  setRemotePath("");
                  linked.reset();
                }}
              />
            )}
            <small className="hint">
              Follow manifest links at <code>{pointer(index.link_path)}</code>.
            </small>
            <div>
              <button
                className="btn"
                type="button"
                disabled={!!index.keys.length && !keys.length}
                onClick={() => linked.mutate()}
              >
                {linked.isPending ? "Inspecting…" : "Inspect linked manifests"}
              </button>
            </div>
            <Problem error={linked.error} />
            {linked.data &&
              (linked.data.collections.length ? (
                <Field label="Linked release list">
                  <select
                    value={remotePath}
                    onChange={(e) => setRemotePath(e.target.value)}
                  >
                    <option value="" disabled>
                      Choose a release list
                    </option>
                    {linked.data.collections.map((c, i) => (
                      <option key={i} value={i}>
                        {pointer(c.path)} · version at {pointer(c.version_path)}
                      </option>
                    ))}
                  </select>
                </Field>
              ) : (
                <Empty title="No release lists found in these manifests" />
              ))}
          </>
        )}
      </fieldset>
      {collection && (
        <CollectionFields
          key={choice + "/" + remotePath + "/" + JSON.stringify(keys)}
          collection={collection}
          index={index && { ...index, keys }}
          initial={
            initial?.path === collection.path &&
            initial.version_path === collection.version_path
              ? initial
              : undefined
          }
          onReview={onReview}
          busy={busy}
          error={error}
        />
      )}
    </div>
  );
}
