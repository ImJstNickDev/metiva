import { describe, expect, it } from "vitest";
import {
  actions,
  bytes,
  compareFiles,
  eta,
  percent,
  slugify,
  type ArchiveFile,
} from "./model";

describe("reported transfer measurements", () => {
  it("separates an unknown size from a confirmed empty file", () => {
    expect(bytes(null)).toBe("—");
    expect(bytes(0)).toBe("0 B");
    expect(percent({ bytes: 0, total: null, state: "downloading" })).toBeNull();
    expect(percent({ bytes: 0, total: 0, state: "connecting" })).toBeNull();
    expect(percent({ bytes: 0, total: 0, state: "completed" })).toBe(100);
  });
  it("uses persisted bytes for determinate progress", () => {
    expect(percent({ bytes: 10, total: 40, state: "downloading" })).toBe(25);
    expect(bytes(1024)).toBe("1 KiB");
  });
  it("offers controls for the worker's current state", () => {
    expect(actions("paused")).toEqual(["resume", "cancel"]);
    expect(actions("retrying")).toContain("retry");
    expect(actions("completed")).toHaveLength(0);
    expect(actions("failed")).toEqual(["retry"]);
  });
});
describe("publication comparison", () => {
  const f: ArchiveFile = {
    id: 1,
    release: "1.0",
    filename: "pkg.tgz",
    source_url: "https://upstream.example/pkg",
    size: 4,
    digest: "a",
  };
  it("identifies provenance changes for identical bytes", () => {
    expect(
      compareFiles([{ ...f, source_url: f.source_url + "2" }], [f]).map(
        (x) => x.change,
      ),
    ).toEqual(["Changed"]);
  });
  it("distinguishes releases even when the filename is identical", () => {
    expect(
      compareFiles([{ ...f, release: "2.0" }], [f]).map((x) => x.change),
    ).toEqual(["Added", "Removed"]);
    expect(compareFiles([{ ...f, id: 99 }], [f])).toEqual([]);
  });
});
it("suggests a URL slug from the name", () =>
  expect(slugify("  Linux / ARM64  ")).toBe("linux-arm64"));

it("estimates time only for a measured live transfer", () => {
  const t = { state: "downloading", total: 220, bytes: 100, speed: 2 };
  expect(eta(t)).toBe("1m");
  expect(eta({ ...t, speed: 0 })).toBeNull();
  expect(eta({ ...t, state: "paused" })).toBeNull();
  expect(eta({ ...t, total: null })).toBeNull();
});
