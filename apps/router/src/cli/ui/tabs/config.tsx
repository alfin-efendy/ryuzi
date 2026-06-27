import React, { useState } from "react";
import { Box, Text, useInput } from "ink";
import TextInput from "ink-text-input";
import type { AppController } from "../controller";
import { theme } from "../theme";

export function ConfigTab({ controller, setEditing }: { controller: AppController; setEditing: (b: boolean) => void }) {
  const keys = controller.settingKeys();
  const [idx, setIdx] = useState(0);
  const [editing, setEd] = useState(false);
  const [draft, setDraft] = useState("");
  const [error, setError] = useState<string | null>(null);

  useInput((_in, key) => {
    if (key.upArrow) setIdx((i) => (i > 0 ? i - 1 : keys.length - 1));
    else if (key.downArrow) setIdx((i) => (i < keys.length - 1 ? i + 1 : 0));
    else if (key.return) { setDraft(controller.get(keys[idx]!) ?? ""); setError(null); setEd(true); setEditing(true); }
  }, { isActive: !editing });

  useInput((_in, key) => {
    if (key.escape) { setEd(false); setEditing(false); setError(null); }
  }, { isActive: editing });

  return (
    <Box flexDirection="column">
      {keys.map((k, i) => {
        const v = controller.get(k) ?? "";
        const masked = controller.isSecret(k);
        const shown = masked && v ? "••••••••" : v || "(unset)";
        const sel = i === idx;
        if (sel && editing) {
          return (
            <Box key={k}>
              <Text color={theme.accent}>{"› " + k.padEnd(22)}</Text>
              <TextInput value={draft} onChange={setDraft} mask={masked ? "•" : undefined}
                onSubmit={() => {
                  try { controller.set(k, draft); setEd(false); setEditing(false); setError(null); }
                  catch (e) { setError((e as Error).message); }
                }} />
            </Box>
          );
        }
        return (
          <Box key={k}>
            <Text color={sel ? theme.accent : theme.dim}>{(sel ? "› " : "  ") + k.padEnd(22)}</Text>
            <Text>{shown}</Text>
          </Box>
        );
      })}
      {error && <Text color={theme.bad}>✗ {error}</Text>}
      <Text color={theme.dim}>↑↓ select · Enter edit · Esc cancel</Text>
    </Box>
  );
}
