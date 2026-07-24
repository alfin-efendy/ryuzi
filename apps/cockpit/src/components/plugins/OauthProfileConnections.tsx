import { openUrl } from "@tauri-apps/plugin-opener";
import { Copy, ExternalLink } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import {
  Button,
  SettingsCard as Card,
  SettingsCardHeader as CardHeader,
  SettingsCardHint as CardHint,
  SettingsCardRow as CardRow,
  SettingsCardTitle as CardTitle,
} from "@ryuzi/ui";
import type { ComponentOauthProfileInfo } from "@/bindings";
import { Pill } from "@/components/common/bits";
import { usePlugins } from "@/store-plugins";

/** A profile can be connected via the device grant only when it declares BOTH
 *  a device-authorization endpoint (to start the flow) and a token endpoint (to
 *  exchange the device code). */
export function isDeviceFlowConnectable(p: ComponentOauthProfileInfo): boolean {
  return !!p.deviceAuthorizationUrl && !!p.tokenUrl;
}

const sleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

/** Transient state of an in-progress device-flow connection for one profile. */
type Flow = {
  profileId: string;
  userCode: string;
  /** `verification_uri_complete` when the provider offers it (pre-fills the
   *  code), else the plain `verification_uri`. */
  openUrl: string;
  status: "polling" | "expired" | "denied" | "error";
};

/** Give up a device-flow poll after this many CONSECUTIVE transport errors —
 *  transient blips on each fresh poll connection are expected and retried, but a
 *  persistent failure (offline, DNS down) should stop rather than spin. */
const MAX_CONSECUTIVE_POLL_ERRORS = 6;

/**
 * The "Connections (OAuth)" card for a component plugin: one row per declared
 * OAuth profile, with a device-grant Connect / Disconnect action and a live
 * status badge. Generic — it renders whatever profiles the manifest declares,
 * with no per-provider branch. The poll loop lives here (the view owns the
 * transient flow state); the thin RPCs live on the store.
 */
export function OauthProfileConnections({
  pluginId,
  profiles,
  onChanged,
}: {
  pluginId: string;
  profiles: ComponentOauthProfileInfo[];
  onChanged: () => void;
}) {
  const beginProfileDeviceFlow = usePlugins((s) => s.beginProfileDeviceFlow);
  const pollProfileDeviceFlow = usePlugins((s) => s.pollProfileDeviceFlow);
  const disconnectProfile = usePlugins((s) => s.disconnectProfile);

  const [flow, setFlow] = useState<Flow | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  // Set when the user cancels (or the component unmounts) so the async poll
  // loop stops touching state after teardown.
  const cancelledRef = useRef(false);
  useEffect(() => () => void (cancelledRef.current = true), []);

  const connect = useCallback(
    async (profile: ComponentOauthProfileInfo) => {
      if (!profile.deviceAuthorizationUrl || !profile.tokenUrl) return;
      cancelledRef.current = false;
      setBusy(profile.id);
      const start = await beginProfileDeviceFlow(pluginId, profile.id, profile.deviceAuthorizationUrl);
      setBusy(null);
      if (!start) return;

      setFlow({
        profileId: profile.id,
        userCode: start.userCode,
        openUrl: start.verificationUriComplete ?? start.verificationUri,
        status: "polling",
      });

      let intervalMs = Math.max(1, start.intervalSecs) * 1000;
      let consecutiveErrors = 0;
      while (!cancelledRef.current) {
        if (Date.now() > start.expiresAt) {
          setFlow((f) => (f ? { ...f, status: "expired" } : f));
          return;
        }
        await sleep(intervalMs);
        if (cancelledRef.current) return;
        const outcome = await pollProfileDeviceFlow(pluginId, profile.id, profile.tokenUrl, start.deviceCode, start.expiresAt);
        if (cancelledRef.current) return;
        if (outcome === null) {
          // Transient poll error (a network blip on this fresh connection).
          // Keep polling — GitHub's endpoint is reachable — but give up after a
          // persistent run so a truly offline machine doesn't spin to expiry.
          if (++consecutiveErrors >= MAX_CONSECUTIVE_POLL_ERRORS) {
            setFlow((f) => (f ? { ...f, status: "error" } : f));
            return;
          }
          continue;
        }
        consecutiveErrors = 0;
        if (outcome === "ready") {
          toast.success(`Connected ${profile.id}`);
          setFlow(null);
          onChanged();
          return;
        }
        if (outcome === "slow-down") intervalMs += 5000;
        else if (outcome === "expired") {
          setFlow((f) => (f ? { ...f, status: "expired" } : f));
          return;
        } else if (outcome === "denied") {
          setFlow((f) => (f ? { ...f, status: "denied" } : f));
          return;
        }
        // "pending" → keep polling
      }
    },
    [pluginId, beginProfileDeviceFlow, pollProfileDeviceFlow, onChanged],
  );

  const cancel = useCallback(() => {
    cancelledRef.current = true;
    setFlow(null);
  }, []);

  const disconnect = useCallback(
    async (profileId: string) => {
      setBusy(profileId);
      const ok = await disconnectProfile(pluginId, profileId);
      setBusy(null);
      if (ok) onChanged();
    },
    [pluginId, disconnectProfile, onChanged],
  );

  const connectable = profiles.filter(isDeviceFlowConnectable);
  if (connectable.length === 0) return null;

  return (
    <Card className="mb-3">
      <CardHeader>
        <CardTitle>Connections (OAuth)</CardTitle>
        <CardHint>Sign in with the device grant — no configuration needed.</CardHint>
      </CardHeader>

      {connectable.map((profile) => {
        const flowing = flow?.profileId === profile.id;
        return (
          <div key={profile.id} className="border-b border-border last:border-b-0">
            <CardRow>
              <div className="flex min-w-0 flex-1 flex-col gap-0.5">
                <span className="truncate text-[13px] font-medium">{profile.id}</span>
                {profile.scopes.length > 0 && (
                  <span className="truncate text-[11.5px] text-muted-foreground">{profile.scopes.join(", ")}</span>
                )}
              </div>
              {profile.connected ? <Pill variant="primary">Connected</Pill> : <Pill variant="secondary">Not connected</Pill>}
              {profile.connected ? (
                <Button variant="outline" size="sm" disabled={busy === profile.id} onClick={() => void disconnect(profile.id)}>
                  Disconnect
                </Button>
              ) : (
                <Button
                  size="sm"
                  disabled={!profile.clientIdConfigured || busy === profile.id || flowing}
                  title={profile.clientIdConfigured ? undefined : "This plugin ships no OAuth client id"}
                  onClick={() => void connect(profile)}
                >
                  {busy === profile.id ? "Starting…" : "Connect"}
                </Button>
              )}
            </CardRow>

            {!profile.clientIdConfigured && !profile.connected && (
              <div className="px-[18px] pb-3 text-[11.5px] text-muted-foreground">
                No OAuth client id is configured for this profile, so it can't be connected.
              </div>
            )}

            {flowing && flow && (
              <div className="flex flex-col gap-3 px-[18px] pb-4">
                {flow.status === "polling" && (
                  <>
                    <div className="text-[12.5px] text-muted-foreground">
                      Enter this code at the sign-in page, then keep this open — it connects automatically.
                    </div>
                    <div className="flex items-center gap-2">
                      <code className="rounded-md border border-border bg-muted px-3 py-1.5 font-mono text-lg tracking-[0.2em]">
                        {flow.userCode}
                      </code>
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={() => {
                          void navigator.clipboard?.writeText(flow.userCode);
                          toast.success("Code copied");
                        }}
                      >
                        <Copy aria-hidden size={13} strokeWidth={2} />
                        Copy
                      </Button>
                      <Button size="sm" onClick={() => void openUrl(flow.openUrl)}>
                        <ExternalLink aria-hidden size={13} strokeWidth={2} />
                        Open sign-in
                      </Button>
                    </div>
                    <div className="flex items-center gap-3">
                      <span className="text-[12px] text-muted-foreground">Waiting for you to authorize…</span>
                      <Button variant="ghost" size="sm" onClick={cancel}>
                        Cancel
                      </Button>
                    </div>
                  </>
                )}
                {flow.status === "expired" && (
                  <div className="flex items-center gap-3">
                    <span className="text-[12.5px] text-muted-foreground">The code expired before you authorized.</span>
                    <Button size="sm" onClick={() => void connect(profile)}>
                      Try again
                    </Button>
                    <Button variant="ghost" size="sm" onClick={cancel}>
                      Dismiss
                    </Button>
                  </div>
                )}
                {flow.status === "denied" && (
                  <div className="flex items-center gap-3">
                    <span className="text-[12.5px] text-muted-foreground">The authorization was declined.</span>
                    <Button size="sm" onClick={() => void connect(profile)}>
                      Try again
                    </Button>
                    <Button variant="ghost" size="sm" onClick={cancel}>
                      Dismiss
                    </Button>
                  </div>
                )}
                {flow.status === "error" && (
                  <div className="flex items-center gap-3">
                    <span className="text-[12.5px] text-muted-foreground">
                      Couldn't reach the sign-in service. Check your connection and try again.
                    </span>
                    <Button size="sm" onClick={() => void connect(profile)}>
                      Try again
                    </Button>
                    <Button variant="ghost" size="sm" onClick={cancel}>
                      Dismiss
                    </Button>
                  </div>
                )}
              </div>
            )}
          </div>
        );
      })}
    </Card>
  );
}
