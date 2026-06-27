import React, { useEffect, useState } from "react";
import { Box, Text, useApp, useInput } from "ink";
import TextInput from "ink-text-input";
import type { AppController } from "./controller";
import type { ConfigField } from "../../providers/types";
import { MultiSelectList } from "./components/multi-select-list";
import { theme } from "./theme";

type Phase = "gateways" | "runtimes" | "fields";

/** Re-order missing fields: provider fields (gateway then runtime) before global fields */
function orderFields(controller: AppController, missing: ConfigField[]): ConfigField[] {
  const gwKeys = new Set(
    controller.enabledGateways().flatMap((id) => controller.gatewayFields(id).map((f) => f.key)),
  );
  const rtKeys = new Set(
    controller.enabledRuntimes().flatMap((id) => controller.runtimeFields(id).map((f) => f.key)),
  );
  const providerFields = missing.filter((f) => gwKeys.has(f.key) || rtKeys.has(f.key));
  const globalFields = missing.filter((f) => !gwKeys.has(f.key) && !rtKeys.has(f.key));
  return [...providerFields, ...globalFields];
}

export function Wizard({ controller, onDone }: { controller: AppController; onDone: () => void }) {
  const { exit } = useApp();
  const [phase, setPhase] = useState<Phase>("gateways");
  const [gwSel, setGwSel] = useState<Set<string>>(new Set(controller.enabledGateways()));
  const [rtSel, setRtSel] = useState<Set<string>>(new Set(controller.enabledRuntimes()));
  const [detected, setDetected] = useState<Record<string, string>>({});
  const [fields, setFields] = useState<ConfigField[]>([]);
  const [fieldIdx, setFieldIdx] = useState(0);
  const [draft, setDraft] = useState("");
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let on = true;
    for (const r of controller.runtimeDescriptors())
      controller.detectRuntime(r.id).then((info) => {
        if (on) setDetected((d) => ({ ...d, [r.id]: info.found ? `✓ ${info.version ?? ""}`.trim() : "✗ not found" }));
      });
    return () => { on = false; };
  }, [controller]);

  useInput((_in, key) => {
    if (key.escape) exit();
    if (phase === "gateways" && key.return && gwSel.size > 0) {
      controller.setEnabledGateways([...gwSel]); setPhase("runtimes");
    } else if (phase === "runtimes" && key.return && rtSel.size > 0) {
      controller.setEnabledRuntimes([...rtSel]);
      const orderedRt = controller.runtimeDescriptors().filter((r) => rtSel.has(r.id)).map((r) => r.id);
      controller.setDefaultRuntime(orderedRt[0]!);
      const missing = orderFields(controller, controller.requiredMissingFields());
      if (missing.length === 0) onDone();
      else { setFields(missing); setPhase("fields"); }
    }
  }, { isActive: phase !== "fields" });

  if (phase === "gateways") {
    const items = controller.gatewayDescriptors().map((g) => ({ id: g.id, label: g.label, description: g.description }));
    return (
      <Box flexDirection="column" padding={1}>
        <Text bold color={theme.accent}>hr setup — Choose gateways</Text>
        <Text color={theme.dim}>Space toggles · Enter continues · Esc cancels</Text>
        <Box marginTop={1}><MultiSelectList items={items} selected={gwSel} onToggle={(id) => setGwSel((s) => toggle(s, id))} /></Box>
      </Box>
    );
  }
  if (phase === "runtimes") {
    const items = controller.runtimeDescriptors().map((r) => ({ id: r.id, label: r.label, description: r.description }));
    return (
      <Box flexDirection="column" padding={1}>
        <Text bold color={theme.accent}>hr setup — Choose runtimes</Text>
        <Text color={theme.dim}>Space toggles · Enter continues · Esc cancels</Text>
        <Box marginTop={1}>
          <MultiSelectList items={items} selected={rtSel} onToggle={(id) => setRtSel((s) => toggle(s, id))} renderRight={(id) => detected[id] ?? "…"} />
        </Box>
      </Box>
    );
  }
  // phase === "fields"
  const f = fields[fieldIdx]!;
  const submit = () => {
    try {
      controller.set(f.key, draft);
      setError(null);
      if (fieldIdx + 1 < fields.length) { setFieldIdx(fieldIdx + 1); setDraft(""); }
      else onDone();
    } catch (e) { setError((e as Error).message); }
  };
  return (
    <Box flexDirection="column" padding={1}>
      <Text bold color={theme.accent}>hr setup — Settings ({fieldIdx + 1}/{fields.length})</Text>
      <Text>{f.label}</Text>
      <Text color={theme.dim}>{f.help}{f.example ? `  (e.g. ${f.example})` : ""}</Text>
      <Box marginTop={1}>
        <Text>{"› "}</Text>
        <TextInput value={draft} onChange={setDraft} onSubmit={submit} mask={f.secret ? "•" : undefined} />
      </Box>
      {error && <Text color={theme.bad}>✗ {error}</Text>}
    </Box>
  );
}

function toggle(s: Set<string>, id: string): Set<string> {
  const next = new Set(s); next.has(id) ? next.delete(id) : next.add(id); return next;
}
