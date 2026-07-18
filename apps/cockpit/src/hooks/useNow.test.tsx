import { afterEach, expect, setSystemTime, test } from "bun:test";
import { cleanup, renderHook, waitFor } from "@testing-library/react";
import { useNow } from "./useNow";

afterEach(() => {
  cleanup();
  setSystemTime();
});

test("returns the current time on mount", () => {
  setSystemTime(new Date(1_000_000));
  const { result } = renderHook(() => useNow(false));
  expect(result.current).toBe(1_000_000);
});

test("ticks forward while active", async () => {
  setSystemTime(new Date(1_000_000));
  const { result } = renderHook(() => useNow(true, 10));
  setSystemTime(new Date(1_002_000));
  await waitFor(() => expect(result.current).toBe(1_002_000));
});

test("stays frozen while inactive", async () => {
  setSystemTime(new Date(5_000_000));
  const { result } = renderHook(() => useNow(false, 10));
  setSystemTime(new Date(9_000_000));
  await new Promise((resolve) => setTimeout(resolve, 50));
  expect(result.current).toBe(5_000_000);
});
