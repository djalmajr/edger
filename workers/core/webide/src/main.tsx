import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import {
  createRootRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";
import { NuqsAdapter } from "nuqs/adapters/react";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { TooltipProvider } from "@edger/ui/components/ui/tooltip";
import { ThemeProvider } from "@edger/ui/lib/theme";
import "./app.css";
import { Dashboard } from "./components/dashboard";
import { Workbench } from "./components/workbench";

function WebIdeApp() {
  const projectId = new URLSearchParams(window.location.search).get("project");
  if (!projectId) {
    return (
      <Dashboard
        onOpenProject={(id) =>
          window.location.assign(
            `${window.location.pathname}?project=${encodeURIComponent(id)}`,
          )
        }
      />
    );
  }
  return (
    <Workbench
      projectId={projectId}
      onHome={() => window.location.assign(window.location.pathname)}
    />
  );
}

const rootRoute = createRootRoute({ component: WebIdeApp });
const router = createRouter({ routeTree: rootRoute, basepath: "/webide" });

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 10_000,
      refetchOnWindowFocus: true,
      refetchIntervalInBackground: false,
      retry: 1,
    },
  },
});

const root = document.getElementById("root");
if (!root) throw new Error("root element not found");

createRoot(root).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <NuqsAdapter>
        <ThemeProvider>
          <TooltipProvider delay={200}>
            <RouterProvider router={router} />
          </TooltipProvider>
        </ThemeProvider>
      </NuqsAdapter>
    </QueryClientProvider>
  </StrictMode>,
);
