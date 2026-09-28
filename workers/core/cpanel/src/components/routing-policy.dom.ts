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
    ActivityIcon: Icon,
    BoxIcon: Icon,
    CheckIcon: Icon,
    ChevronDownIcon: Icon,
    ChevronLeftIcon: Icon,
    ChevronRightIcon: Icon,
    ChevronsLeftIcon: Icon,
    ChevronsRightIcon: Icon,
    ChevronsUpDown: Icon,
    CircleAlertIcon: Icon,
    CircleCheckIcon: Icon,
    CopyIcon: Icon,
    CpuIcon: Icon,
    ListIcon: Icon,
    PencilIcon: Icon,
    PlusIcon: Icon,
    RouteIcon: Icon,
    Trash2Icon: Icon,
    ChevronUpIcon: Icon,
    XIcon: Icon,
  }));
}

if (!("happyDOM" in globalThis)) {
  GlobalRegistrator.register({ url: "http://localhost/" });
}
