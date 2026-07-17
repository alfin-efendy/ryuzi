import { Folder, FolderGit2, FileText } from "lucide-react";
import { MenuPanel, MenuPanelItem, MenuPanelSection } from "@ryuzi/ui";
import { basename } from "@/lib/paths";
import type { ContextPickerGroup, ContextPickerItem } from "@/lib/composer-context";

/** Stable label + icon for a single flattened picker row. */
function rowContent(item: ContextPickerItem) {
  switch (item.kind) {
    case "project":
      return {
        icon: <FolderGit2 aria-hidden size={13} strokeWidth={2} className="size-[13px] shrink-0 text-muted-foreground" />,
        label: item.name,
        detail: "Current project",
      };
    case "agent":
      return {
        icon: <span className="size-2.5 shrink-0 rounded-full" style={{ backgroundColor: item.agent.avatarColor }} />,
        label: item.agent.name,
        detail: null,
      };
    case "workspace": {
      const Icon = item.dir ? Folder : FileText;
      return {
        icon: <Icon aria-hidden size={13} strokeWidth={2} className="size-[13px] shrink-0 text-muted-foreground" />,
        label: basename(item.path),
        detail: item.path,
      };
    }
  }
}

/** Unified `@` context picker: Project, Agents, Folders, Files, grouped and
 *  rendered in one popover. `activeIndex` addresses the flattened row order
 *  across all sections (see `flattenContextPickerGroups`) so a caller
 *  driving keyboard navigation from the composer's textarea can highlight
 *  the right row regardless of which section it falls in. This component
 *  owns no keyboard handling itself — the textarea in Task 4 drives
 *  `activeIndex` and calls `onPick`/`onClose`. */
export function ContextPickerMenu({
  groups,
  activeIndex,
  onPick,
  onClose,
}: {
  groups: ContextPickerGroup[];
  activeIndex: number;
  onPick: (item: ContextPickerItem) => void;
  onClose: () => void;
}) {
  let index = -1;

  return (
    <MenuPanel onClose={onClose} className="bottom-full left-2.5 z-50 mb-1.5 w-[360px]">
      <div role="menu">
        {groups.map((group) => (
          <div key={group.section}>
            <MenuPanelSection>{group.label}</MenuPanelSection>
            {group.items.map((item) => {
              index += 1;
              const rowIndex = index;
              const { icon, label, detail } = rowContent(item);
              const key =
                item.kind === "project"
                  ? `project-${item.id}`
                  : item.kind === "agent"
                    ? `agent-${item.agent.id}`
                    : `workspace-${item.path}`;
              return (
                <MenuPanelItem key={key} selected={rowIndex === activeIndex} onClick={() => onPick(item)} className="font-medium">
                  {icon}
                  <span className="min-w-0 flex-1 truncate">{label}</span>
                  {detail && <span className="shrink-0 truncate text-[11px] text-muted-foreground">{detail}</span>}
                </MenuPanelItem>
              );
            })}
          </div>
        ))}
      </div>
    </MenuPanel>
  );
}
