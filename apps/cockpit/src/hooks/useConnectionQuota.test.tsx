import { afterEach, expect, mock, test } from "bun:test";
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import type { ProviderQuotaInfo } from "@/bindings";
import { LOCAL_RUNNER } from "@/lib/session-key";

type QuotaCommandMocks = {
  connectionProviderQuota: ReturnType<typeof mock<(...args: [string, string]) => Promise<unknown>>>;
  resetCodexCredit: ReturnType<typeof mock<(...args: [string, string]) => Promise<unknown>>>;
};

const shared = globalThis as typeof globalThis & { __quotaCommandMocks?: QuotaCommandMocks };
const quotaCommandMocks = (shared.__quotaCommandMocks ??= {
  connectionProviderQuota: mock<(...args: [string, string]) => Promise<unknown>>(),
  resetCodexCredit: mock<(...args: [string, string]) => Promise<unknown>>(),
});
const { connectionProviderQuota, resetCodexCredit } = quotaCommandMocks;

mock.module("@/bindings", () => ({
  commands: { connectionProviderQuota, resetCodexCredit },
}));

const { useConnectionQuota } = await import("./useConnectionQuota");

type Deferred<T> = {
  promise: Promise<T>;
  resolve: (value: T) => void;
  reject: (reason?: unknown) => void;
};

function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function quota(plan = "Plus"): ProviderQuotaInfo {
  return {
    provider: "openai-oauth",
    plan,
    message: null,
    limitReached: false,
    reviewLimitReached: false,
    resetCredits: null,
    quotas: [],
  };
}

function ok(value: ProviderQuotaInfo) {
  return { status: "ok" as const, data: value };
}

function error(message: string) {
  return { status: "error" as const, error: { message } };
}

afterEach(() => {
  cleanup();
  connectionProviderQuota.mockReset();
  resetCodexCredit.mockReset();
});

test("loads quota on mount and does nothing for unsupported accounts", async () => {
  connectionProviderQuota.mockResolvedValueOnce(ok(quota("Pro")));
  const mounted = renderHook(() => useConnectionQuota("account-a", "claude"));

  await waitFor(() => expect(mounted.result.current.state.status).toBe("loaded"));
  expect(mounted.result.current.state).toEqual({ status: "loaded", quota: quota("Pro"), error: null });
  expect(connectionProviderQuota).toHaveBeenCalledWith(LOCAL_RUNNER, "account-a");

  const unsupported = renderHook(() => useConnectionQuota("account-b", null));
  expect(unsupported.result.current.state).toEqual({ status: "idle", quota: null, error: null });
  expect(connectionProviderQuota).toHaveBeenCalledTimes(1);
});

test("refreshes manually and retries an account-local error", async () => {
  connectionProviderQuota.mockResolvedValueOnce(error("Account A unavailable")).mockResolvedValueOnce(ok(quota("Team")));
  const { result } = renderHook(() => useConnectionQuota("account-a", "claude"));

  await waitFor(() => expect(result.current.state).toEqual({ status: "error", quota: null, error: "Account A unavailable" }));
  await act(async () => {
    await result.current.refresh();
  });

  expect(result.current.state).toEqual({ status: "loaded", quota: quota("Team"), error: null });
  expect(connectionProviderQuota).toHaveBeenCalledTimes(2);
});

test("keeps quota errors isolated between accounts", async () => {
  connectionProviderQuota.mockImplementation((_runnerId, id) =>
    Promise.resolve(id === "account-a" ? error("Account A unavailable") : ok(quota("Account B"))),
  );
  const accountA = renderHook(() => useConnectionQuota("account-a", "claude"));
  const accountB = renderHook(() => useConnectionQuota("account-b", "claude"));

  await waitFor(() => expect(accountA.result.current.state.status).toBe("error"));
  await waitFor(() => expect(accountB.result.current.state.status).toBe("loaded"));
  expect(accountA.result.current.state).toEqual({ status: "error", quota: null, error: "Account A unavailable" });
  expect(accountB.result.current.state).toEqual({ status: "loaded", quota: quota("Account B"), error: null });
});

test("keeps only the newest manual refresh generation", async () => {
  const first = deferred<ReturnType<typeof ok>>();
  const second = deferred<ReturnType<typeof ok>>();
  connectionProviderQuota.mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);
  const { result } = renderHook(() => useConnectionQuota("account-a", "claude"));

  await waitFor(() => expect(connectionProviderQuota).toHaveBeenCalledTimes(1));
  act(() => {
    void result.current.refresh();
  });
  await waitFor(() => expect(connectionProviderQuota).toHaveBeenCalledTimes(2));

  await act(async () => {
    second.resolve(ok(quota("Newest")));
    await second.promise;
  });
  await act(async () => {
    first.resolve(ok(quota("Stale")));
    await first.promise;
  });

  expect(result.current.state).toEqual({ status: "loaded", quota: quota("Newest"), error: null });
});

test("does not carry quota from one connection into another connection's error", async () => {
  const accountB = deferred<ReturnType<typeof error>>();
  connectionProviderQuota.mockResolvedValueOnce(ok(quota("Account A"))).mockReturnValueOnce(accountB.promise);
  const { result, rerender } = renderHook(({ id, capability }) => useConnectionQuota(id, capability), {
    initialProps: { id: "account-a" as string | null, capability: "claude" as "claude" | null },
  });

  await waitFor(() => expect(result.current.state).toEqual({ status: "loaded", quota: quota("Account A"), error: null }));
  rerender({ id: "account-b", capability: "claude" });
  await waitFor(() => expect(connectionProviderQuota).toHaveBeenCalledWith(LOCAL_RUNNER, "account-b"));
  expect(result.current.state).toEqual({ status: "loading", quota: null, error: null });
  await act(async () => {
    accountB.resolve(error("Account B unavailable"));
    await accountB.promise;
  });
  expect(result.current.state).toEqual({ status: "error", quota: null, error: "Account B unavailable" });
});

test("ignores a late completion after a connection is deleted", async () => {
  const lateRequest = deferred<ReturnType<typeof ok>>();
  connectionProviderQuota.mockResolvedValueOnce(ok(quota("Account A"))).mockReturnValueOnce(lateRequest.promise);
  const { result, rerender } = renderHook(({ id, capability }) => useConnectionQuota(id, capability), {
    initialProps: { id: "account-a" as string | null, capability: "claude" as "claude" | null },
  });

  await waitFor(() => expect(result.current.state).toEqual({ status: "loaded", quota: quota("Account A"), error: null }));
  act(() => {
    void result.current.refresh();
  });
  await waitFor(() => expect(result.current.state.status).toBe("loading"));

  rerender({ id: null, capability: null });
  expect(result.current.state).toEqual({ status: "idle", quota: null, error: null });
  await act(async () => {
    lateRequest.resolve(ok(quota("Late account A")));
    await lateRequest.promise;
  });
  expect(result.current.state).toEqual({ status: "idle", quota: null, error: null });
});

test("does not commit a late quota completion after unmount", async () => {
  const request = deferred<ReturnType<typeof ok>>();
  connectionProviderQuota.mockReturnValueOnce(request.promise);
  const { result, unmount } = renderHook(() => useConnectionQuota("account-a", "claude"));

  await waitFor(() => expect(connectionProviderQuota).toHaveBeenCalledTimes(1));
  expect(result.current.state).toEqual({ status: "loading", quota: null, error: null });
  unmount();
  await act(async () => {
    request.resolve(ok(quota("Late")));
    await request.promise;
  });
  expect(connectionProviderQuota).toHaveBeenCalledTimes(1);
  expect(result.current.state).toEqual({ status: "loading", quota: null, error: null });
});

test("resets a Codex credit and refreshes exactly once", async () => {
  connectionProviderQuota.mockResolvedValueOnce(ok(quota("Before"))).mockResolvedValueOnce(ok(quota("After")));
  resetCodexCredit.mockResolvedValueOnce({ status: "ok", data: { reset: true } });
  const { result } = renderHook(() => useConnectionQuota("account-a", "codex"));

  await waitFor(() => expect(result.current.state.status).toBe("loaded"));
  let reset = false;
  await act(async () => {
    reset = await result.current.resetCredit();
  });

  expect(reset).toBe(true);
  expect(resetCodexCredit).toHaveBeenCalledWith(LOCAL_RUNNER, "account-a");
  expect(connectionProviderQuota).toHaveBeenCalledTimes(2);
  expect(result.current.state).toEqual({ status: "loaded", quota: quota("After"), error: null });
});

test("returns false when the reset command fails without refreshing", async () => {
  connectionProviderQuota.mockResolvedValueOnce(ok(quota("Before")));
  resetCodexCredit.mockResolvedValueOnce(error("No reset credits available"));
  const { result } = renderHook(() => useConnectionQuota("account-a", "codex"));

  await waitFor(() => expect(result.current.state.status).toBe("loaded"));
  await act(async () => {
    expect(await result.current.resetCredit()).toBe(false);
  });
  expect(connectionProviderQuota).toHaveBeenCalledTimes(1);
});

test("returns true after a successful reset even when its refresh fails and retains prior quota", async () => {
  connectionProviderQuota.mockResolvedValueOnce(ok(quota("Before"))).mockResolvedValueOnce(error("Quota unavailable"));
  resetCodexCredit.mockResolvedValueOnce({ status: "ok", data: { reset: true } });
  const { result } = renderHook(() => useConnectionQuota("account-a", "codex"));

  await waitFor(() => expect(result.current.state.status).toBe("loaded"));
  await act(async () => {
    expect(await result.current.resetCredit()).toBe(true);
  });
  expect(result.current.state).toEqual({ status: "error", quota: quota("Before"), error: "Quota unavailable" });
  expect(connectionProviderQuota).toHaveBeenCalledTimes(2);
});

test("clears reset state when its account changes before the reset completes", async () => {
  const reset = deferred<{ status: "ok"; data: { reset: true } }>();
  connectionProviderQuota.mockResolvedValueOnce(ok(quota("Account A"))).mockResolvedValueOnce(ok(quota("Account B")));
  resetCodexCredit.mockReturnValueOnce(reset.promise);
  const { result, rerender } = renderHook(({ id }) => useConnectionQuota(id, "codex"), {
    initialProps: { id: "account-a" },
  });

  await waitFor(() => expect(result.current.state).toEqual({ status: "loaded", quota: quota("Account A"), error: null }));
  let pendingReset!: Promise<boolean>;
  act(() => {
    pendingReset = result.current.resetCredit();
  });
  await waitFor(() => expect(result.current.resetting).toBe(true));

  rerender({ id: "account-b" });
  await waitFor(() => expect(result.current.state).toEqual({ status: "loaded", quota: quota("Account B"), error: null }));
  expect(result.current.resetting).toBe(false);

  await act(async () => {
    reset.resolve({ status: "ok", data: { reset: true } });
    expect(await pendingReset).toBe(true);
  });
  expect(result.current.state).toEqual({ status: "loaded", quota: quota("Account B"), error: null });
  expect(result.current.resetting).toBe(false);
  expect(connectionProviderQuota).toHaveBeenCalledTimes(2);
});
