import { test, expect, spyOn } from "bun:test";
import { useGateways } from "./store-gateways";
import { commands, type GatewayInfo } from "./bindings";

const localRow: GatewayInfo = {
  id: "local",
  name: "This PC",
  badge: "WIN",
  kind: "local",
  detail: "Windows · box",
  metaLine: "Windows · x64 · 8 cores · 16 GB",
  status: "connected",
  latency: "0ms",
  daemonVersion: "v0.6.0",
  uptime: "1h 0m",
  lastSeenMs: 1,
  resources: [],
  fingerprint: null,
  fsMode: "projects",
  paths: [],
};

const remoteRow: GatewayInfo = {
  id: "remote-abc123",
  name: "gpu-box",
  badge: "RMT",
  kind: "remote",
  detail: "remote · 10.0.0.9:7443",
  metaLine: "remote · 10.0.0.9:7443 · paired runner",
  status: "connected",
  latency: "12ms",
  daemonVersion: "—",
  uptime: null,
  lastSeenMs: 2,
  resources: [],
  fingerprint: "b64ssh256fingerprint==",
  fsMode: "projects",
  paths: [],
};

function reset() {
  useGateways.setState({ gateways: [], loaded: false });
}

test("addRunner: on success, applies the refreshed gateway list and returns true", async () => {
  reset();
  const spy = spyOn(commands, "addRunner").mockResolvedValue({ status: "ok", data: [localRow, remoteRow] });

  const ok = await useGateways.getState().addRunner("gpu-box", "10.0.0.9", 7443, "b64ssh256fingerprint==", "the-pairing-code");

  expect(ok).toBe(true);
  expect(useGateways.getState().gateways).toEqual([localRow, remoteRow]);
  expect(useGateways.getState().loaded).toBe(true);
  expect(spy).toHaveBeenCalledWith("gpu-box", "10.0.0.9", 7443, "b64ssh256fingerprint==", "the-pairing-code");
  spy.mockRestore();
});

test("addRunner: on failure, leaves the gateway list untouched and returns false", async () => {
  reset();
  useGateways.setState({ gateways: [localRow], loaded: true });
  const spy = spyOn(commands, "addRunner").mockResolvedValue({
    status: "error",
    error: { message: "invalid or expired pairing code" },
  });

  const ok = await useGateways.getState().addRunner("gpu-box", "10.0.0.9", 7443, "b64ssh256fingerprint==", "wrong-code");

  expect(ok).toBe(false);
  expect(useGateways.getState().gateways).toEqual([localRow]);
  spy.mockRestore();
});

test("addRunner: never sends a device token — only Name/Host/Port/Fingerprint/code cross the bindings call", async () => {
  reset();
  const spy = spyOn(commands, "addRunner").mockResolvedValue({ status: "ok", data: [localRow, remoteRow] });

  await useGateways.getState().addRunner("gpu-box", "10.0.0.9", 7443, "b64ssh256fingerprint==", "the-pairing-code");

  const args = spy.mock.calls[0];
  expect(args).toHaveLength(5);
  expect(args).toEqual(["gpu-box", "10.0.0.9", 7443, "b64ssh256fingerprint==", "the-pairing-code"]);
  spy.mockRestore();
});
