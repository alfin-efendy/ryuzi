import { useEffect } from "react";
import { useStore } from "./store";
import { useRuntimes } from "./store-runtimes";
import { useNav } from "./store-nav";
import { useDisableContextMenu } from "./lib/contextMenu";
import { TitleBar } from "./components/shell/TitleBar";
import { Sidebar } from "./components/shell/Sidebar";
import { ProjectSettingsModal } from "./components/modals/ProjectSettingsModal";
import { HomeView } from "./views/HomeView";
import { SessionView } from "./views/SessionView";
import { ModelsView } from "./views/ModelsView";
import { ProviderDetailView } from "./views/ProviderDetailView";
import { ConnectionDetailView } from "./views/ConnectionDetailView";
import { RuntimeView } from "./views/RuntimeView";
import { RuntimeDetailView } from "./views/RuntimeDetailView";
import { SchedulerView } from "./views/SchedulerView";
import { JobDetailView } from "./views/JobDetailView";
import { JobNewView } from "./views/JobNewView";
import { PluginsView } from "./views/PluginsView";
import { AppDetailView } from "./views/AppDetailView";
import { GatewaysView } from "./views/GatewaysView";
import { GatewayDetailView } from "./views/GatewayDetailView";
import { PluginDetailView } from "./views/PluginDetailView";
import { SettingsView } from "./views/SettingsView";
import { Badge, Toaster } from "@ryuzi/ui";

function MainView() {
  const view = useNav((s) => s.history.current);
  switch (view.kind) {
    case "home":
      return <HomeView />;
    case "session":
      return <SessionView />;
    case "models":
      return <ModelsView />;
    case "providerDetail":
      return <ProviderDetailView provider={view.provider} />;
    case "connectionDetail":
      return <ConnectionDetailView id={view.id} />;
    case "runtime":
      return <RuntimeView />;
    case "runtimeDetail":
      return <RuntimeDetailView id={view.id} />;
    case "scheduler":
      return <SchedulerView />;
    case "jobDetail":
      return <JobDetailView id={view.id} />;
    case "jobNew":
      return <JobNewView />;
    case "plugins":
      return <PluginsView />;
    case "appDetail":
      return <AppDetailView id={view.id} />;
    case "gateways":
      return <GatewaysView />;
    case "gatewayDetail":
      return <GatewayDetailView id={view.id} />;
    case "pluginDetail":
      return <PluginDetailView id={view.id} />;
    case "settings":
      return <SettingsView />;
  }
}

export default function App() {
  const init = useStore((s) => s.init);
  const hydrateAgents = useRuntimes((s) => s.hydrate);
  const pending = useStore((s) => s.pendingApprovals.length);
  useDisableContextMenu();
  useEffect(() => {
    init();
    void hydrateAgents();
  }, [init, hydrateAgents]);
  return (
    <div className="relative flex h-screen flex-col overflow-hidden text-sm text-foreground antialiased">
      {/* Wallpaper behind the glass chrome; collapses to transparent when an OS backdrop is active. */}
      <div aria-hidden className="absolute inset-0 z-0" style={{ background: "var(--wallpaper)" }} />
      {/* Full-window glass layer — one blur pass for the whole chrome. */}
      <div aria-hidden className="acrylic-chrome absolute inset-0 z-0" />
      <TitleBar />
      {pending > 0 && (
        <div className="relative z-10 flex shrink-0 items-center gap-2 border-b border-amber-500/30 bg-amber-500/10 px-4 py-1.5 text-xs text-amber-700 dark:text-amber-300">
          <Badge variant="secondary">{pending}</Badge> session(s) need approval
        </div>
      )}
      <div className="relative z-10 flex min-h-0 flex-1">
        <Sidebar />
        <main className="acrylic-main mx-2.5 mb-2.5 flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden rounded-xl border border-border shadow-sm">
          <MainView />
        </main>
      </div>
      <ProjectSettingsModal />
      <Toaster richColors position="bottom-right" />
    </div>
  );
}
