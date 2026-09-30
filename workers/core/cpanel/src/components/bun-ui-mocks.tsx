import * as React from "react";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";

// Shared bun-only mock registrations for the cPanel slice's test files.
//
// Bun's test runner has no Vite plugins: the virtual `~icons` modules and
// the base-ui menu primitives only resolve under the vite-backed runner, and
// a mocked module's export surface is frozen process-wide — one
// registration answers every test file in the workers set. The registrations
// below therefore expose a superset surface: every icon the cPanel sources
// (including main.tsx) and the shared UI components import from the icon
// barrel, and every dropdown-menu export any cPanel consumer uses. Each
// consumer test file imports this module at the top so the union is
// registered before the first consumer loads; the individual fixtures keep
// only their own module-specific stubs (dialog, combobox, tooltip,
// virtual icons).
//
// Under vitest (vite) nothing is registered: the guard below is inert and
// the real components resolve as usual.
if ((process.versions as { bun?: string }).bun) {
  const bunTest = createRequire(import.meta.url)("bun:test") as {
    mock: { module(id: string, factory: () => Record<string, unknown>): void };
  };
  const icon = () => React.createElement("span", { "aria-hidden": true });
  const icons = {
    ActivityIcon: icon,
    ArrowLeftIcon: icon,
    BoxIcon: icon,
    Check: icon,
    CheckIcon: icon,
    ChevronDownIcon: icon,
    ChevronLeftIcon: icon,
    ChevronRightIcon: icon,
    ChevronUpIcon: icon,
    ChevronsLeftIcon: icon,
    ChevronsRightIcon: icon,
    ChevronsUpDown: icon,
    CircleAlertIcon: icon,
    CircleCheckIcon: icon,
    CopyIcon: icon,
    CpuIcon: icon,
    DownloadIcon: icon,
    ExternalLinkIcon: icon,
    FileArchiveIcon: icon,
    FileIcon: icon,
    FolderIcon: icon,
    FolderOpenIcon: icon,
    GaugeIcon: icon,
    HeartPulseIcon: icon,
    InfoIcon: icon,
    KeyRoundIcon: icon,
    Layers3Icon: icon,
    ListIcon: icon,
    Loader2Icon: icon,
    LogOutIcon: icon,
    MoreVerticalIcon: icon,
    OctagonXIcon: icon,
    PanelLeftIcon: icon,
    PanelsTopLeftIcon: icon,
    PencilIcon: icon,
    PlusIcon: icon,
    PowerOffIcon: icon,
    RefreshCwIcon: icon,
    RocketIcon: icon,
    RotateCcwIcon: icon,
    RouteIcon: icon,
    ScrollTextIcon: icon,
    SearchIcon: icon,
    SettingsIcon: icon,
    ShieldCheckIcon: icon,
    Trash2Icon: icon,
    TriangleAlertIcon: icon,
    UploadCloudIcon: icon,
    UploadIcon: icon,
    WebhookIcon: icon,
    X: icon,
    XIcon: icon,
  };
  bunTest.mock.module("@edger/ui/icons/lucide", () => icons);
  bunTest.mock.module(
    fileURLToPath(
      new URL("../../../../ui/src/icons/lucide.ts", import.meta.url),
    ),
    () => icons,
  );

  // The stateful DOM stub the slice's screen tests rely on (content appears
  // inline once the trigger is clicked, checkbox items stay operable), plus
  // the radio group/item exports the preference menus need to resolve.
  const menuContext = React.createContext<{
    open: boolean;
    toggle(): void;
  }>({ open: false, toggle: () => undefined });
  // Concrete props for the trigger element: cloneElement merges its props
  // into Partial<P>, and P must not be unknown for that to typecheck.
  type MenuTriggerProps = {
    "aria-expanded"?: boolean;
    onClick?: (event: React.MouseEvent) => void;
  };
  const DropdownMenu = ({ children }: { children?: React.ReactNode }) => {
    const [open, setOpen] = React.useState(false);
    const value = React.useMemo(
      () => ({ open, toggle: () => setOpen((current) => !current) }),
      [open],
    );
    return (
      <menuContext.Provider value={value}>
        {children}
      </menuContext.Provider>
    );
  };
  const DropdownMenuTrigger = ({
    children,
    render,
  }: {
    children?: React.ReactNode;
    render?: React.ReactElement<MenuTriggerProps>;
  }) => {
    const { open, toggle } = React.useContext(menuContext);
    if (!render) return null;
    return React.cloneElement(
      render,
      {
        "aria-expanded": open || undefined,
        onClick: (event: React.MouseEvent) => {
          event.stopPropagation();
          toggle();
        },
      },
      children,
    );
  };
  const DropdownMenuContent = ({
    children,
    ...rest
  }: { children?: React.ReactNode } & Record<string, unknown>) => {
    const { open } = React.useContext(menuContext);
    if (!open) return null;
    return (
      <div role="menu" {...rest}>
        {children}
      </div>
    );
  };
  // Transparent group container: the real component enforces the base-ui
  // group context, the stub only keeps the label/items renderable.
  const DropdownMenuGroup = ({ children }: { children?: React.ReactNode }) => (
    <div role="group">{children}</div>
  );
  const DropdownMenuLabel = ({ children }: { children?: React.ReactNode }) => (
    <div>{children}</div>
  );
  const DropdownMenuCheckboxItem = ({
    children,
    checked,
    onCheckedChange,
  }: {
    children?: React.ReactNode;
    checked?: boolean;
    onCheckedChange?: (checked: boolean) => void;
  }) => (
    <button
      aria-checked={Boolean(checked)}
      onClick={() => onCheckedChange?.(!Boolean(checked))}
      role="menuitemcheckbox"
      type="button"
    >
      {children}
    </button>
  );
  const DropdownMenuRadioGroup = ({
    children,
    value: _value,
    onValueChange: _onValueChange,
  }: {
    children?: React.ReactNode;
    value?: string;
    onValueChange?: (value: string) => void;
  }) => <div role="radiogroup">{children}</div>;
  const DropdownMenuRadioItem = ({
    children,
    checked,
    onCheckedChange,
  }: {
    children?: React.ReactNode;
    checked?: boolean;
    onCheckedChange?: (checked: boolean) => void;
  }) => (
    <button
      aria-checked={Boolean(checked)}
      onClick={() => onCheckedChange?.(!Boolean(checked))}
      role="radio"
      type="button"
    >
      {children}
    </button>
  );
  bunTest.mock.module("@edger/ui/components/ui/dropdown-menu", () => ({
    DropdownMenu,
    DropdownMenuCheckboxItem,
    DropdownMenuContent,
    DropdownMenuGroup,
    DropdownMenuLabel,
    DropdownMenuRadioGroup,
    DropdownMenuRadioItem,
    DropdownMenuTrigger,
  }));
}

export {};
