import type { Harness, HarnessRunInput, HarnessEvent } from "../../src/harness/types";

// Emits a text chunk then a result. If the run input has approve(), it first asks
// for approval of a Bash tool so approval-path tests can exercise the round-trip.
export class FakeHarness implements Harness {
  readonly id = "fake";
  async *run(input: HarnessRunInput): AsyncIterable<HarnessEvent> {
    const sessionId = input.resume ?? crypto.randomUUID();
    if (!input.resume) yield { type: "init", sessionId };
    await input.approve({ tool: "Bash", input: { command: "echo hi" } });
    yield { type: "text", text: "hello from fake" };
    yield { type: "result", usage: undefined, sessionId };
  }
}
