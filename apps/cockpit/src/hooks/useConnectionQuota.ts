import { useCallback, useEffect, useRef, useState } from "react";
import { commands, type ProviderQuotaCapability, type ProviderQuotaInfo } from "@/bindings";

export type ConnectionQuotaState =
  | { status: "idle"; quota: null; error: null }
  | { status: "loading"; quota: ProviderQuotaInfo | null; error: null }
  | { status: "loaded"; quota: ProviderQuotaInfo; error: null }
  | { status: "error"; quota: ProviderQuotaInfo | null; error: string };

const idleState: ConnectionQuotaState = { status: "idle", quota: null, error: null };

function errorMessage(error: unknown) {
  return error instanceof Error ? error.message : "Quota unavailable";
}

export function useConnectionQuota(connectionId: string | null, capability: ProviderQuotaCapability | null) {
  const [state, setState] = useState<ConnectionQuotaState>(idleState);
  const [resetting, setResetting] = useState(false);
  const mountedRef = useRef(false);
  const generationRef = useRef(0);
  const resettingRef = useRef(false);
  const contextRef = useRef({ connectionId, capability });
  const stateRef = useRef<ConnectionQuotaState>(idleState);
  contextRef.current = { connectionId, capability };

  const commit = useCallback(
    (generation: number, requestConnectionId: string, requestCapability: ProviderQuotaCapability, next: ConnectionQuotaState) => {
      const current = contextRef.current;
      if (
        !mountedRef.current ||
        generation !== generationRef.current ||
        current.connectionId !== requestConnectionId ||
        current.capability !== requestCapability
      ) {
        return false;
      }
      stateRef.current = next;
      setState(next);
      return true;
    },
    [],
  );

  const refreshFor = useCallback(
    async (requestConnectionId: string | null, requestCapability: ProviderQuotaCapability | null) => {
      if (!requestConnectionId || !requestCapability) {
        generationRef.current += 1;
        if (mountedRef.current) {
          stateRef.current = idleState;
          setState(idleState);
        }
        return;
      }

      const generation = ++generationRef.current;
      const priorQuota = stateRef.current.quota;
      commit(generation, requestConnectionId, requestCapability, { status: "loading", quota: priorQuota, error: null });

      try {
        const result = await commands.connectionProviderQuota(requestConnectionId);
        if (result.status === "ok") {
          commit(generation, requestConnectionId, requestCapability, { status: "loaded", quota: result.data, error: null });
          return;
        }
        commit(generation, requestConnectionId, requestCapability, {
          status: "error",
          quota: priorQuota,
          error: result.error.message,
        });
      } catch (error) {
        commit(generation, requestConnectionId, requestCapability, {
          status: "error",
          quota: priorQuota,
          error: errorMessage(error),
        });
      }
    },
    [commit],
  );

  const refresh = useCallback(async () => {
    const current = contextRef.current;
    await refreshFor(current.connectionId, current.capability);
  }, [refreshFor]);

  const resetCredit = useCallback(async () => {
    const { connectionId: requestConnectionId, capability: requestCapability } = contextRef.current;
    if (!requestConnectionId || requestCapability !== "codex" || resettingRef.current) return false;

    resettingRef.current = true;
    if (mountedRef.current) setResetting(true);
    try {
      const result = await commands.resetCodexCredit(requestConnectionId);
      if (result.status !== "ok") return false;

      const current = contextRef.current;
      if (mountedRef.current && current.connectionId === requestConnectionId && current.capability === requestCapability) {
        await refreshFor(requestConnectionId, requestCapability);
      }
      return true;
    } catch {
      return false;
    } finally {
      resettingRef.current = false;
      const current = contextRef.current;
      if (mountedRef.current && current.connectionId === requestConnectionId && current.capability === requestCapability) {
        setResetting(false);
      }
    }
  }, [refreshFor]);

  useEffect(() => {
    mountedRef.current = true;
    void refreshFor(connectionId, capability);
    return () => {
      generationRef.current += 1;
      mountedRef.current = false;
    };
  }, [capability, connectionId, refreshFor]);

  return { state, refresh, resetCredit, resetting };
}
