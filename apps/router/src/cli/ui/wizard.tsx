import React, { useState } from "react";
import { Box, Text, useApp, useInput } from "ink";
import TextInput from "ink-text-input";
import type { AppController } from "./controller";
import { theme } from "./theme";

export function Wizard({ controller, onDone }: { controller: AppController; onDone: () => void }) {
  const required = controller.requiredKeys();
  const [step, setStep] = useState(0);
  const [draft, setDraft] = useState("");
  const [error, setError] = useState<string | null>(null);
  const { exit } = useApp();

  useInput((_in, key) => { if (key.escape) exit(); });

  const k = required[step]!;
  const masked = controller.isSecret(k);
  const submit = () => {
    try {
      controller.set(k, draft);
      setError(null);
      if (step + 1 < required.length) { setStep(step + 1); setDraft(""); }
      else onDone();
    } catch (e) { setError((e as Error).message); }
  };

  return (
    <Box flexDirection="column" padding={1}>
      <Text bold color={theme.accent}>hr setup ({step + 1}/{required.length})</Text>
      <Text color={theme.dim}>Fill in your settings. Esc to cancel.</Text>
      <Box marginTop={1}>
        <Text>{k.padEnd(20)} </Text>
        <TextInput value={draft} onChange={setDraft} onSubmit={submit} mask={masked ? "•" : undefined} />
      </Box>
      {error && <Text color={theme.bad}>✗ {error}</Text>}
    </Box>
  );
}
