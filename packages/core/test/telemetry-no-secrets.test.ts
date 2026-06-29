// apps/router/test/telemetry-no-secrets.test.ts
//
// Regression guard: proves that no secret (e.g. discord.token) or prompt body
// ever reaches a telemetry span / counter attribute.
//
// If this test FAILS it means a real leak was introduced — do NOT weaken the
// assertions; treat it as a security finding.

import { test, expect } from "bun:test";
import type { Telemetry, Span, Attrs } from "../src/observability/types";
import type { Agent, AgentEvent, AgentRunInput } from "../src/agents/types";
import { openDb } from "../src/store/db";
import { ProjectsStore } from "../src/store/projects";
import { SessionsStore } from "../src/store/sessions";
import { SettingsStore } from "../src/config/store";
import { ControlPlane } from "../src/core/control-plane";

// ─── Distinctive sentinel values ────────────────────────────────────────────
// Chosen to be unique strings that would never appear in span/counter names or
// generic attribute values, so a match in serialised telemetry is unambiguous.
const SECRET_TOKEN = "Bot xSECRET_DISCORD_TOKEN_SENTINEL_xyzzy_42!";
const PROMPT_TEXT = "PROMPT_SENTINEL_DO_NOT_LEAK_abcdef1234567890";

// ─── Capturing telemetry sink ────────────────────────────────────────────────
// Mirrors the Recording class from telemetry-instrumentation.test.ts exactly.
class Recording implements Telemetry {
  spans: Array<{ name: string; attrs: Attrs; error?: string; ended: boolean }> = [];
  counts: Array<{ name: string; attrs: Attrs }> = [];
  records: Array<{ name: string; value: number }> = [];

  startSpan(name: string, attrs: Attrs = {}): Span {
    const rec = {
      name,
      attrs: { ...attrs },
      ended: false,
    } as { name: string; attrs: Attrs; error?: string; ended: boolean };
    this.spans.push(rec);
    return {
      setAttribute: (k, v) => {
        rec.attrs[k] = v;
      },
      setError: (m) => {
        rec.error = m;
      },
      end: () => {
        rec.ended = true;
      },
    };
  }

  count(name: string, attrs: Attrs = {}): void {
    this.counts.push({ name, attrs });
  }

  record(name: string, value: number): void {
    this.records.push({ name, value });
  }
}

// ─── Fake harness that emits a minimal successful result ─────────────────────
function makeWire(events: AgentEvent[]) {
  class FakeHarness implements Agent {
    readonly id = "claude-code";
    async *run(_input: AgentRunInput): AsyncIterable<AgentEvent> {
      for (const e of events) yield e;
    }
  }

  const db = openDb(":memory:");
  const projects = new ProjectsStore(db);
  const sessions = new SessionsStore(db);
  const settings = new SettingsStore(db);

  // Store the secret token in settings, exactly as production does.
  settings.set("discord.token", SECRET_TOKEN);

  projects.insert({
    projectId: "p1",
    name: "guard-test-project",
    workdir: "/repo",
    harness: "claude-code",
    permMode: "bypassPermissions",
  });

  const tel = new Recording();

  const cp = new ControlPlane({
    projects,
    sessions,
    settings,
    workdirRoot: "/root",
    telemetry: tel,
    worktree: {
      pathFor: (r, p, s) => `${r}/${p}/${s}`,
      create: async () => {},
      remove: async () => {},
    },
  });

  cp.harnesses.register("claude-code", () => new FakeHarness());

  return { cp, tel, settings };
}

// ─── Tests ───────────────────────────────────────────────────────────────────

test("telemetry captures no secret token or prompt body in spans or counts", async () => {
  const { cp, tel, settings } = makeWire([{ type: "result", usage: {} }]);

  // The token IS reachable in settings, so its absence from telemetry below is a
  // meaningful negative (not merely "never in scope"). Today the token is never read
  // on the telemetry path; this guards against a future regression that forwards it.
  expect(settings.get("discord.token")).toBe(SECRET_TOKEN);

  // Drive a real harness.run with the distinctive prompt text.
  await cp.startSession({ projectId: "p1", prompt: PROMPT_TEXT });

  // Serialise everything the telemetry sink captured.
  const captured = JSON.stringify({ spans: tel.spans, counts: tel.counts, records: tel.records });

  // Primary assertions: neither secret nor prompt must appear anywhere.
  expect(captured).not.toContain(SECRET_TOKEN);
  expect(captured).not.toContain(PROMPT_TEXT);

  // Sanity check: the run actually happened and telemetry is non-empty.
  expect(tel.spans.length).toBeGreaterThan(0);
  expect(tel.counts.length).toBeGreaterThan(0);
});

test("telemetry captures no secret token or prompt body when an error event fires", async () => {
  const { cp, tel } = makeWire([{ type: "error", message: "transient failure" }]);

  await cp.startSession({ projectId: "p1", prompt: PROMPT_TEXT });

  const captured = JSON.stringify({ spans: tel.spans, counts: tel.counts, records: tel.records });

  expect(captured).not.toContain(SECRET_TOKEN);
  expect(captured).not.toContain(PROMPT_TEXT);

  // Sanity: the error span was recorded (contains truncated error, not prompt).
  const errSpan = tel.spans.find((s) => s.name === "harness.run");
  expect(errSpan?.error).toBe("transient failure");
});
