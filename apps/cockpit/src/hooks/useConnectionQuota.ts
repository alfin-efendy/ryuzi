import { useCallback, useEffect, useRef, useState } from "react";
import { commands, type ProviderQuotaCapability, type ProviderQuotaInfo } from "@/bindings";

export type ConnectionQuotaState =
  | { status: "idle"; quota: null; error: null }
  | { status: "loading"; quota: ProviderQuotaInfo | null; error: null }
  | { status: "loaded"; quota: ProviderQuotaInfo; error: null }
  | { status: "error"; quota: ProviderQuotaInfo | null; error: string };

const idleState: ConnectionQuotaState = { status: "idle", quota: null, error: null };

type QuotaContext = { connectionId: string | null; capability: ProviderQuotaCapability | null };

function sameContext(left: QuotaContext, right: QuotaContext) {
  return left.connectionId === right.connectionId && left.capability === right.capability;
}

function errorMessage(error: unknown) {
  return error instanceof Error ? error.message : "Quota unavailable";
}

export function useConnectionQuota(connectionId: string | null, capability: ProviderQuotaCapability | null) {
  const [state, setState] = useState<ConnectionQuotaState>(idleState);
  const [resetting, setResetting] = useState(false);
  const mountedRef = useRef(false);
  const generationRef = useRef(0);
  const resettingRef = useRef(false);
  const resetGenerationRef = useRef(0);
  const contextRef = useRef({ connectionId, capability });
  const stateRef = useRef({ context: { connectionId, capability }, state: idleState });
  contextRef.current = { connectionId, capability };

  const commit = useCallback((generation: number, requestContext: QuotaContext, next: ConnectionQuotaState) => {
    const current = contextRef.current;
    if (!mountedRef.current || generation !== generationRef.current || !sameContext(current, requestContext)) {
      return false;
    }
    stateRef.current = { context: requestContext, state: next };
    setState(next);
    return true;
  }, []);

  const refreshFor = useCallback(
    async (requestConnectionId: string | null, requestCapability: ProviderQuotaCapability | null) => {
      const requestContext = { connectionId: requestConnectionId, capability: requestCapability };
      if (!requestConnectionId || !requestCapability) {
        generationRef.current += 1;
        if (mountedRef.current && sameContext(contextRef.current, requestContext)) {
          stateRef.current = { context: requestContext, state: idleState };
          setState(idleState);
        }
        return;
      }

      const generation = ++generationRef.current;
      const priorQuota = sameContext(stateRef.current.context, requestContext) ? stateRef.current.state.quota : null;
      commit(generation, requestContext, { status: "loading", quota: priorQuota, error: null });

      try {
        const result = await commands.connectionProviderQuota(requestConnectionId);
        if (result.status === "ok") {
          commit(generation, requestContext, { status: "loaded", quota: result.data, error: null });
          return;
        }
        commit(generation, requestContext, {
          status: "error",
          quota: priorQuota,
          error: result.error.message,
        });
      } catch (error) {
        commit(generation, requestContext, {
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

    const requestContext = { connectionId: requestConnectionId, capability: requestCapability };
    const resetGeneration = ++resetGenerationRef.current;
    resettingRef.current = true;
    if (mountedRef.current) setResetting(true);
    try {
      const result = await commands.resetCodexCredit(requestConnectionId);
      if (result.status !== "ok") return false;

      const current = contextRef.current;
      if (mountedRef.current && sameContext(current, requestContext)) {
        await refreshFor(requestConnectionId, requestCapability);
      }
      return true;
    } catch {
      return false;
    } finally {
      if (resetGeneration === resetGenerationRef.current) {
        resettingRef.current = false;
        const current = contextRef.current;
        if (mountedRef.current && sameContext(current, requestContext)) {
          setResetting(false);
        }
      }
    }
  }, [refreshFor]);

  useEffect(() => {
    const effectContext = { connectionId, capability };
    if (!sameContext(stateRef.current.context, effectContext)) {
      stateRef.current = { context: effectContext, state: idleState };
      setState(idleState);
      resetGenerationRef.current += 1;
      resettingRef.current = false;
      setResetting(false);
    }
    mountedRef.current = true;
    void refreshFor(connectionId, capability);
    return () => {
      generationRef.current += 1;
      mountedRef.current = false;
    };
  }, [capability, connectionId, refreshFor]);

  return { state, refresh, resetCredit, resetting };
}
