import { compareSemver, type Worker } from "./api";

// Mirrors the backend serving rule for the versions of one worker name:
// a candidate is enabled (not disabled, not staged, not internal). When
// the group's defaultVersion pointer references a candidate it wins —
// that is what promote sets; otherwise the highest semver candidate is
// served. Returns undefined when nothing is servable.
export function servingVersion(versions: Worker[]): string | undefined {
  const candidates = versions.filter(
    (worker) =>
      worker.status !== "disabled" &&
      worker.staged !== true &&
      worker.visibility !== "internal",
  );
  const pointer = versions.find((worker) => worker.defaultVersion)?.defaultVersion;
  if (pointer && candidates.some((worker) => worker.version === pointer))
    return pointer;
  let served: string | undefined;
  for (const worker of candidates)
    if (served === undefined || compareSemver(worker.version, served) > 0)
      served = worker.version;
  return served;
}

export type VersionActions = {
  canDelete: boolean;
  canSetDefault: boolean;
  isDefault: boolean;
  isOnlyVersion: boolean;
  nextDefault: string | undefined;
};

// Per-version row actions. Promote re-enables a disabled version, so
// set-as-default stays available on it. Deleting the pointer's target
// leaves the remaining candidates without a usable pointer, which
// servingVersion already resolves to the highest semver.
export function versionActions(
  worker: Worker,
  versions: Worker[],
): VersionActions {
  const isDefault = worker.version === servingVersion(versions);
  return {
    canDelete: worker.origin === "user",
    canSetDefault:
      worker.origin === "user" &&
      worker.visibility !== "internal" &&
      !isDefault,
    isDefault,
    isOnlyVersion: versions.length === 1,
    nextDefault: servingVersion(
      versions.filter((candidate) => candidate.version !== worker.version),
    ),
  };
}
