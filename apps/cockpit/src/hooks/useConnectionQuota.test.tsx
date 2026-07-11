import { afterEach, expect, mock, test } from "bun:test";
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import type { ProviderQuotaInfo } from "@/bindings";

type QuotaCommandMocks = {
  connectionProviderQuota: ReturnType<typeof mock<(...args: [string]) => Promise<unknown>>>;
  resetCodexCredit: ReturnType<typeof mock<(...args: [string]) => Promise<unknown>>>;
};

const shared = globalThis as typeof globalThis & { __quotaCommandMocks?: QuotaCommandMocks };
const quotaCommandMocks = (shared.__quotaCommandMocks ??= {
  connectionProviderQuota: mock<(...args: [string]) => Promise<unknown>>(),
  resetCodexCredit: mock<(...args: [string]) => Promise<unknown>>(),
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
  expect(connectionProviderQuota).toHaveBeenCalledWith("account-a");

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
  connectionProviderQuota.mockImplementation((id) =>
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

test("ignores an old connection completion after the connection changes or is deleted", async () => {
  const oldRequest = deferred<ReturnType<typeof ok>>();
  const newRequest = deferred<ReturnType<typeof ok>>();
  connectionProviderQuota.mockReturnValueOnce(oldRequest.promise).mockReturnValueOnce(newRequest.promise);
  const { result, rerender } = renderHook(({ id, capability }) => useConnectionQuota(id, capability), {
    initialProps: { id: "account-a" as string | null, capability: "claude" as "claude" | null },
  });

  await waitFor(() => expect(connectionProviderQuota).toHaveBeenCalledWith("account-a"));
  rerender({ id: "account-b", capability: "claude" });
  await waitFor(() => expect(connectionProviderQuota).toHaveBeenCalledWith("account-b"));
  await act(async () => {
    oldRequest.resolve(ok(quota("Old account")));
    newRequest.resolve(ok(quota("New account")));
    await Promise.all([oldRequest.promise, newRequest.promise]);
  });
  expect(result.current.state).toEqual({ status: "loaded", quota: quota("New account"), error: null });

  rerender({ id: null, capability: null });
  expect(result.current.state).toEqual({ status: "idle", quota: null, error: null });
});

test("ignores late quota completion after unmount", async () => {
  const request = deferred<ReturnType<typeof ok>>();
  connectionProviderQuota.mockReturnValueOnce(request.promise);
  const { unmount } = renderHook(() => useConnectionQuota("account-a", "claude"));

  await waitFor(() => expect(connectionProviderQuota).toHaveBeenCalledTimes(1));
  unmount();
  await act(async () => {
    request.resolve(ok(quota("Late")));
    await request.promise;
  });
  expect(connectionProviderQuota).toHaveBeenCalledTimes(1);
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
  expect(resetCodexCredit).toHaveBeenCalledWith("account-a");
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
