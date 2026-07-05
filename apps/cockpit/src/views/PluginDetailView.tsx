import { BackButton } from "@/components/common/DetailHeader";
import { useNav } from "@/store-nav";
import { pluginById, usePlugins } from "@/store-plugins";

// Placeholder screen for a single plugin. Task 12 replaces this with the
// full detail view (settings, MCP servers, models, auth).
export function PluginDetailView({ id }: { id: string }) {
  const nav = useNav();
  const plugin = usePlugins((s) => pluginById(s.plugins, id));

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-8 pb-10 pt-[22px]">
      <div className="mx-auto max-w-[760px]">
        <BackButton label="Back" onClick={() => nav.goBack()} />
        <div className="text-[13px] text-muted-foreground">Plugin: {plugin?.name ?? id}</div>
      </div>
    </div>
  );
}
