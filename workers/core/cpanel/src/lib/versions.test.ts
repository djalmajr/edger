import { describe, expect, it } from "vitest";

import { type Worker } from "./api";
import { servingVersion, versionActions } from "./versions";

function v(version: string, overrides: Partial<Worker> = {}): Worker {
  return {
    kind: "fullstack",
    name: "app",
    origin: "user",
    status: "enabled",
    version,
    ...overrides,
  };
}

describe("servingVersion", () => {
  it("serves the highest semver when there is no pointer", () => {
    expect(servingVersion([v("1.0.0"), v("1.0.1")])).toBe("1.0.1");
  });

  it("serves the pointer when it targets a candidate", () => {
    const versions = [
      v("1.0.0", { defaultVersion: "1.0.0" }),
      v("1.0.1", { defaultVersion: "1.0.0" }),
    ];
    expect(servingVersion(versions)).toBe("1.0.0");
  });

  it("skips staged versions when there is no pointer", () => {
    expect(servingVersion([v("1.0.0"), v("1.0.1", { staged: true })])).toBe(
      "1.0.0",
    );
  });

  it("never serves disabled or internal versions", () => {
    expect(
      servingVersion([v("1.0.0", { status: "disabled" })]),
    ).toBeUndefined();
    expect(
      servingVersion([v("1.0.0", { visibility: "internal" })]),
    ).toBeUndefined();
    expect(
      servingVersion([
        v("1.0.0", { status: "disabled" }),
        v("1.0.1", { visibility: "internal" }),
      ]),
    ).toBeUndefined();
  });

  it("falls back to the highest semver when the pointer targets a disabled version", () => {
    const versions = [
      v("1.0.0", { defaultVersion: "1.0.0" }),
      v("1.0.1", { status: "disabled", defaultVersion: "1.0.1" }),
    ];
    expect(servingVersion(versions)).toBe("1.0.0");
  });

  it("returns undefined without candidates", () => {
    expect(servingVersion([])).toBeUndefined();
  });
});

describe("versionActions", () => {
  it("flags the served version as default and withholds set-as-default", () => {
    const versions = [v("1.0.0"), v("1.0.1")];
    expect(versionActions(versions[1], versions)).toEqual({
      canDelete: true,
      canSetDefault: false,
      isDefault: true,
      isOnlyVersion: false,
      nextDefault: "1.0.0",
    });
  });

  it("offers set-as-default on a non-default user version", () => {
    const versions = [v("1.0.0"), v("1.0.1")];
    expect(versionActions(versions[0], versions)).toEqual({
      canDelete: true,
      canSetDefault: true,
      isDefault: false,
      isOnlyVersion: false,
      nextDefault: "1.0.1",
    });
  });

  it("offers set-as-default on a disabled version because promote re-enables it", () => {
    const versions = [v("1.0.0", { status: "disabled" }), v("1.0.1")];
    expect(versionActions(versions[0], versions)).toEqual({
      canDelete: true,
      canSetDefault: true,
      isDefault: false,
      isOnlyVersion: false,
      nextDefault: "1.0.1",
    });
  });

  it("restricts core versions (no set-as-default, no delete)", () => {
    const versions = [v("1.0.0", { origin: "core" }), v("1.0.1")];
    expect(versionActions(versions[0], versions)).toEqual({
      canDelete: false,
      canSetDefault: false,
      isDefault: false,
      isOnlyVersion: false,
      nextDefault: "1.0.1",
    });
  });

  it("hides set-as-default on internal versions", () => {
    const versions = [v("1.0.0", { visibility: "internal" }), v("1.0.1")];
    expect(versionActions(versions[0], versions)).toEqual({
      canDelete: true,
      canSetDefault: false,
      isDefault: false,
      isOnlyVersion: false,
      nextDefault: "1.0.1",
    });
  });

  it("marks the only version and leaves no next default", () => {
    const versions = [v("1.0.0")];
    expect(versionActions(versions[0], versions)).toEqual({
      canDelete: true,
      canSetDefault: false,
      isDefault: true,
      isOnlyVersion: true,
      nextDefault: undefined,
    });
  });

  it("computes nextDefault when deleting the default with a pointer", () => {
    const versions = [
      v("1.0.0", { defaultVersion: "1.0.0" }),
      v("1.0.1", { defaultVersion: "1.0.0" }),
    ];
    // Deleting the pointer's target leaves a stale pointer, so 1.0.1
    // becomes the highest candidate.
    expect(versionActions(versions[0], versions).nextDefault).toBe("1.0.1");
    // Deleting the other version keeps the live pointer on 1.0.0.
    expect(versionActions(versions[1], versions).nextDefault).toBe("1.0.0");
  });

  it("computes nextDefault when deleting the default without a pointer", () => {
    const versions = [v("1.0.0"), v("1.0.1")];
    expect(versionActions(versions[1], versions).nextDefault).toBe("1.0.0");
  });
});
