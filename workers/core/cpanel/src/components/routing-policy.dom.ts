import { createRequire } from "node:module";
import { GlobalRegistrator } from "@happy-dom/global-registrator";

const nodeRequire = createRequire(import.meta.url);

function Icon() {
  return null;
}

// Bun's test runner has no Vite icon plugin. Named exports have to be real
// bindings; a proxy does not satisfy the static imports of this barrel.
if ((process.versions as { bun?: string }).bun) {
  const bunTest = nodeRequire("bun:test") as {
    mock: {
      module: (id: string, factory: () => Record<string, unknown>) => void;
    };
  };
  bunTest.mock.module("@edger/ui/icons/lucide", () => ({
    PlusIcon: Icon,
    Trash2Icon: Icon,
    XIcon: Icon,
  }));
}

if (!("happyDOM" in globalThis)) {
  GlobalRegistrator.register({ url: "http://localhost/" });
}
