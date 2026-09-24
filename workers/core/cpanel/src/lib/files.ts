// Pure logic behind the Files tab delete flow. The server is the source of
// truth (POST /api/admin/workers/{name}/files/delete); this module only
// shapes the dialog copy and the path list, which keeps it testable without
// the runtime.

export type FileEntry = {
  kind: "dir" | "file";
  name: string;
  size: number;
};

// Bookkeeping file the runtime bumps at the version root. It shows up in
// the root listing but is never a deletion target (the route refuses it).
export const REVISION_FILE = ".edger-revision";

export type FileDeleteError = {
  code: string;
  message: string;
  path: string;
};

export type FilesDeleteResponse = {
  deleted: string[];
  entries: FileEntry[];
  errors: FileDeleteError[];
  revision: string;
};

// One deletion target, with its path relative to the version root.
export type DeleteTarget = {
  isDir: boolean;
  path: string;
};

// Confirmation copy for the delete dialog, mirroring the "Delete key"
// dialog: one file, one folder, or a batch.
export function deleteDialogText(
  targets: DeleteTarget[],
): { description: string; title: string } {
  const single = targets.length === 1 ? targets[0] : undefined;
  if (single) {
    return single.isDir
      ? {
          description: `Delete "${single.path}" and everything inside it? This cannot be undone.`,
          title: "Delete folder",
        }
      : {
          description: `Delete "${single.path}"? This cannot be undone.`,
          title: "Delete file",
        };
  }
  const count = targets.length;
  return {
    description: `Delete ${count} selected items? Folders are deleted with everything inside them. This cannot be undone.`,
    title: `Delete ${count} items`,
  };
}

// What a checked selection may actually delete: only names present in the
// current listing and never the revision file. Order follows the listing.
export function deletablePaths(
  entries: FileEntry[],
  selected: Iterable<string>,
): string[] {
  const chosen = new Set(selected);
  const paths: string[] = [];
  for (const entry of entries)
    if (entry.name !== REVISION_FILE && chosen.has(entry.name))
      paths.push(entry.name);
  return paths;
}

// The delete route answers with the ROOT listing of the version, so its
// `entries` may only be written to the root query (path ""). The open
// directory is refetched with its own GET when it is not the root — never
// overwritten with the root listing.
export type FileListCacheUpdate =
  | { action: "invalidate"; path: string }
  | { action: "set"; path: string };

export function deleteEntriesCacheUpdates(
  openPath: string,
): FileListCacheUpdate[] {
  const updates: FileListCacheUpdate[] = [{ action: "set", path: "" }];
  if (openPath !== "") updates.push({ action: "invalidate", path: openPath });
  return updates;
}
