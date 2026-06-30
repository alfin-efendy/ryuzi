import { Tabs as TabsPrimitive } from "@base-ui/react/tabs";
import { cn } from "../../lib/utils";

function Tabs(props: TabsPrimitive.Root.Props) {
  return <TabsPrimitive.Root data-slot="tabs" {...props} />;
}

function TabsList({ className, ...props }: TabsPrimitive.List.Props) {
  return <TabsPrimitive.List data-slot="tabs-list" className={cn("flex items-center gap-1", className)} {...props} />;
}

function TabsTab({ className, ...props }: TabsPrimitive.Tab.Props) {
  return (
    <TabsPrimitive.Tab
      data-slot="tabs-tab"
      className={cn(
        "flex max-w-[170px] cursor-default items-center gap-1.5 rounded-lg px-2.5 py-1.5 text-xs text-muted-foreground outline-none transition-colors hover:bg-accent",
        "data-selected:bg-accent data-selected:text-foreground",
        className,
      )}
      {...props}
    />
  );
}

export { Tabs, TabsList, TabsTab };
