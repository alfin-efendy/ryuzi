import React, { useState } from "react";
import { Box, Text, useInput } from "ink";
import TextInput from "ink-text-input";
import type { AppController } from "../controller";
import type { ConfigField } from "@harness/core";
import { Panel } from "../components/panel";
import { palette, symbols } from "../theme";

type Row =
  | { kind: "header"; label: string }
  | { kind: "field"; field: ConfigField }
  | { kind: "toggle"; id: string; label: string; group: "gateway" | "runtime"; on: boolean };

function buildRows(c: AppController): Row[] {
  const rows: Row[] = [{ kind: "header", label: "General" }];
  for (const f of c.generalFields()) rows.push({ kind: "field", field: f });
  for (const id of c.enabledGateways()) {
    const d = c.gatewayDescriptors().find((g) => g.id === id);
    if (d?.fields.length) {
      rows.push({ kind: "header", label: d.label });
      for (const f of d.fields) rows.push({ kind: "field", field: f });
    }
  }
  for (const id of c.enabledRuntimes()) {
    const d = c.runtimeDescriptors().find((r) => r.id === id);
    if (d?.fields.length) {
      rows.push({ kind: "header", label: d.label });
      for (const f of d.fields) rows.push({ kind: "field", field: f });
    }
  }
  rows.push({ kind: "header", label: "Providers" });
  const en = new Set([...c.enabledGateways(), ...c.enabledRuntimes()]);
  for (const g of c.gatewayDescriptors())
    rows.push({ kind: "toggle", id: g.id, label: `${g.label} (gateway)`, group: "gateway", on: en.has(g.id) });
  for (const r of c.runtimeDescriptors())
    rows.push({ kind: "toggle", id: r.id, label: `${r.label} (runtime)`, group: "runtime", on: en.has(r.id) });
  return rows;
}

export function ConfigTab({ controller, setEditing }: { controller: AppController; setEditing: (b: boolean) => void }) {
  const rows = buildRows(controller);
  const selectable = rows.map((r, i) => (r.kind === "header" ? -1 : i)).filter((i) => i >= 0);
  const [pos, setPos] = useState(0); // index into selectable
  const [editing, setEd] = useState(false);
  const [draft, setDraft] = useState("");
  const [error, setError] = useState<string | null>(null);
  const curRowIdx = selectable[Math.min(pos, selectable.length - 1)] ?? -1;
  const cur = rows[curRowIdx];

  useInput(
    (_in, key) => {
      if (key.upArrow) setPos((p) => (p > 0 ? p - 1 : selectable.length - 1));
      else if (key.downArrow) setPos((p) => (p < selectable.length - 1 ? p + 1 : 0));
      else if (key.return && cur?.kind === "field") {
        setDraft(controller.get(cur.field.key) ?? "");
        setError(null);
        setEd(true);
        setEditing(true);
      } else if (_in === " " && cur?.kind === "toggle") toggleProvider(controller, cur);
    },
    { isActive: !editing },
  );

  useInput(
    (_in, key) => {
      if (key.escape) {
        setEd(false);
        setEditing(false);
        setError(null);
      }
    },
    { isActive: editing },
  );

  return (
    <Panel title="Configuration" focus>
      {rows.map((r, i) => {
        if (r.kind === "header")
          return (
            <Text key={`h${i}`} bold color={palette.signature}>
              {r.label}
            </Text>
          );
        const sel = i === curRowIdx;
        if (r.kind === "toggle") {
          return (
            <Text key={`t${i}`} color={sel ? palette.signature : palette.dim}>
              {sel ? `${symbols().caret} ` : "  "}[{r.on ? "x" : " "}] {r.label}
            </Text>
          );
        }
        const f = r.field;
        const v = controller.get(f.key) ?? "";
        const shown = f.secret && v ? "••••••••" : v || "(unset)";
        if (sel && editing) {
          return (
            <Box key={`f${i}`}>
              <Text color={palette.signature}>{`${symbols().caret} ` + f.label.padEnd(22)}</Text>
              <TextInput
                value={draft}
                onChange={setDraft}
                mask={f.secret ? "•" : undefined}
                onSubmit={() => {
                  try {
                    controller.set(f.key, draft);
                    setEd(false);
                    setEditing(false);
                    setError(null);
                  } catch (e) {
                    setError((e as Error).message);
                  }
                }}
              />
            </Box>
          );
        }
        return (
          <Box key={`f${i}`} flexDirection="column">
            <Box>
              <Text color={sel ? palette.signature : palette.dim}>{(sel ? `${symbols().caret} ` : "  ") + f.label.padEnd(22)}</Text>
              <Text>{shown}</Text>
            </Box>
            {sel && (
              <Text color={palette.dim}>
                {" "}
                {f.help}
                {f.example ? `  (e.g. ${f.example})` : ""}
              </Text>
            )}
          </Box>
        );
      })}
      {error && (
        <Text color={palette.bad}>
          {symbols().bad} {error}
        </Text>
      )}
      <Text color={palette.dim}>↑↓ select · Enter edit · Space toggle provider · Esc cancel</Text>
    </Panel>
  );
}

function toggleProvider(c: AppController, row: { id: string; group: "gateway" | "runtime" }): void {
  if (row.group === "gateway") {
    const on = new Set(c.enabledGateways());
    on.has(row.id) ? on.delete(row.id) : on.add(row.id);
    c.setEnabledGateways([...on]);
  } else {
    const on = new Set(c.enabledRuntimes());
    on.has(row.id) ? on.delete(row.id) : on.add(row.id);
    c.setEnabledRuntimes([...on]);
    if (on.size === 0) c.setDefaultRuntime("");
    else if (!on.has(c.defaultRuntime())) c.setDefaultRuntime([...on][0]!);
  }
}
