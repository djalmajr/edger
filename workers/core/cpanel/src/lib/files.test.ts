import { describe, expect, it } from "vitest";

import {
  deleteDialogText,
  deleteEntriesCacheUpdates,
  deletablePaths,
  type FileEntry,
} from "./files";

const entries: FileEntry[] = [
  { kind: "dir", name: "src", size: 0 },
  { kind: "file", name: ".edger-revision", size: 4 },
  { kind: "file", name: "index.ts", size: 12 },
];

describe("deleteDialogText", () => {
  it("asks for a single file by its relative path", () => {
    expect(deleteDialogText([{ isDir: false, path: "a.txt" }])).toEqual({
      description: 'Delete "a.txt"? This cannot be undone.',
      title: "Delete file",
    });
  });

  it("asks for a single folder and warns about its contents", () => {
    expect(deleteDialogText([{ isDir: true, path: "dir/sub" }])).toEqual({
      description:
        'Delete "dir/sub" and everything inside it? This cannot be undone.',
      title: "Delete folder",
    });
  });

  it("asks for a batch by count, covering folders recursively", () => {
    expect(
      deleteDialogText([
        { isDir: false, path: "a.txt" },
        { isDir: true, path: "src" },
        { isDir: false, path: "z.bin" },
      ]),
    ).toEqual({
      description:
        "Delete 3 selected items? Folders are deleted with everything inside them. This cannot be undone.",
      title: "Delete 3 items",
    });
  });
});

describe("deletablePaths", () => {
  it("keeps the checked entries in listing order", () => {
    expect(deletablePaths(entries, ["index.ts", "src"])).toEqual([
      "src",
      "index.ts",
    ]);
  });

  it("drops .edger-revision even when it is checked", () => {
    expect(deletablePaths(entries, [".edger-revision", "index.ts"])).toEqual([
      "index.ts",
    ]);
  });

  it("ignores selections that are not part of the listing", () => {
    expect(deletablePaths(entries, ["src", "other.txt"])).toEqual(["src"]);
  });
});

describe("deleteEntriesCacheUpdates", () => {
  it("writes the response listing only to the root query when the root is open", () => {
    expect(deleteEntriesCacheUpdates("")).toEqual([
      { action: "set", path: "" },
    ]);
  });

  it("writes the response listing to the root query and refetches the open folder", () => {
    expect(deleteEntriesCacheUpdates("src")).toEqual([
      { action: "set", path: "" },
      { action: "invalidate", path: "src" },
    ]);
  });

  it("never writes the response listing into a subdirectory cache", () => {
    expect(
      deleteEntriesCacheUpdates("src/nested").filter(
        (update) => update.action === "set" && update.path !== "",
      ),
    ).toEqual([]);
  });
});
