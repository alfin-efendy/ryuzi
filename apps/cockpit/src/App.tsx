import { useEffect } from "react";
import { MonitorUp } from "lucide-react";
import { SettingsCard as Card } from "@ryuzi/ui";
import { useStore } from "./store";
import { useRuntimes } from "./store-runtimes";
import { useModelStatuses } from "./store-model-statuses";
import { useNav } from "./store-nav";
import { usePlugins } from "./store-plugins";
import { useDisableContextMenu } from "./lib/contextMenu";
import { TitleBar } from "./components/shell/TitleBar";
import { Sidebar } from "./components/shell/Sidebar";
import { ProjectSettingsModal } from "./components/modals/ProjectSettingsModal";
import { HomeView } from "./views/HomeView";
import { InboxView } from "./views/InboxView";
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
import { Toaster } from "@ryuzi/ui";

function MainView() {
  const view = useNav((s) => s.history.current);
  switch (view.kind) {
    case "home":
      return <HomeView />;
    case "inbox":
      return <InboxView />;
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

const WARN = "#F59E0B";

export default function App() {
  const init = useStore((s) => s.init);
  const hydrateAgents = useRuntimes((s) => s.hydrate);
  const hydrateModelStatuses = useModelStatuses((s) => s.hydrate);
  const restartRequired = usePlugins((s) => s.restartRequired);
  useDisableContextMenu();
  useEffect(() => {
    init();
    void hydrateAgents();
    void hydrateModelStatuses();
    // Read the store directly (not via the reactive selector above) so this
    // mount-time fetch runs exactly once, guarded the same way every other
    // domain store's `load()` is — visiting the Plugins hub first is not a
    // precondition for the restart banner below to be accurate.
    if (!usePlugins.getState().loaded) void usePlugins.getState().load();
  }, [init, hydrateAgents, hydrateModelStatuses]);
  return (
    <div className="relative flex h-screen flex-col overflow-hidden text-sm text-foreground antialiased">
      {/* Wallpaper behind the glass chrome; collapses to transparent when an OS backdrop is active. */}
      <div aria-hidden className="absolute inset-0 z-0" style={{ background: "var(--wallpaper)" }} />
      {/* Full-window glass layer — one blur pass for the whole chrome. */}
      <div aria-hidden className="acrylic-chrome absolute inset-0 z-0" />
      <TitleBar />
      <div className="relative z-10 flex min-h-0 flex-1">
        <Sidebar />
        <main className="acrylic-main mx-2.5 mb-2.5 flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden rounded-xl border border-border shadow-sm">
          {restartRequired && (
            <Card className="m-2.5 mb-0 flex shrink-0 items-center gap-2.5 px-[18px] py-2.5 text-[12.5px]">
              <MonitorUp aria-hidden size={15} strokeWidth={2} className="shrink-0" style={{ color: WARN }} />
              <span className="min-w-0 flex-1">Restart Cockpit to apply plugin changes.</span>
            </Card>
          )}
          <MainView />
        </main>
      </div>
      <ProjectSettingsModal />
      <Toaster richColors position="bottom-right" />
    </div>
  );
}
