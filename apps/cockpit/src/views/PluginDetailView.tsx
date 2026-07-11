import { openUrl } from "@tauri-apps/plugin-opener";
import { CircleAlert, ExternalLink, Pin, PinOff, RefreshCw } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import {
  Badge,
  Button,
  Combobox,
  FormField,
  Input,
  SettingsCard as Card,
  SettingsCardHeader as CardHeader,
  SettingsCardHint as CardHint,
  SettingsCardRow as CardRow,
  SettingsCardTitle as CardTitle,
  Switch,
} from "@ryuzi/ui";
import { commands, events, type ExtensionStatusEntry, type PluginDetail } from "@/bindings";
import { LOCAL_RUNNER } from "@/lib/session-key";
import { BackButton, DetailHeader } from "@/components/common/DetailHeader";
import { IconChip, Pill, PluginStatusBadge } from "@/components/common/bits";
import { pluginIcon } from "@/lib/plugin-icons";
import { useNav } from "@/store-nav";
import { usePlugins } from "@/store-plugins";

const WARN = "#F59E0B";

/** First 8 characters of a resolved git commit SHA — the ledger stores the
 *  full hash; only a short prefix is useful in the UI (matches `git log
 *  --oneline`'s convention). */
function shortCommit(commit: string): string {
  return commit.slice(0, 8);
}

/** Localized date for a `plugin_installs` ledger timestamp (unix ms, per
 *  `PluginInfo.installedAt`/`updatedAt`). */
function formatLedgerTimestamp(ms: number): string {
  return new Date(ms).toLocaleDateString();
}

/** Human label for an `ExtensionStatusEntry.status` value (Track D
 *  observability, DT8). Pure and exported so it stays unit-testable without
 *  mounting the view — mirrors `PluginsView.tsx`'s `catalogStatusLabel`
 *  convention. */
export function extensionStatusLabel(status: string): string {
  switch (status) {
    case "running":
      return "Running";
    case "starting":
      return "Starting";
    case "restarting":
      return "Restarting";
    case "failed":
      return "Failed";
    case "stopped":
      return "Stopped";
    case "not-running":
      return "Not running";
    default:
      return status;
  }
}

/** `Pill` color variant for an `ExtensionStatusEntry.status` value — green-ish
 *  "primary" for healthy/running, "warn" amber for a mid-restart/transient
 *  state, "danger" red for failed, muted "secondary" for stopped/not-running. */
export function extensionStatusPillVariant(status: string): "primary" | "warn" | "danger" | "secondary" {
  switch (status) {
    case "running":
      return "primary";
    case "starting":
    case "restarting":
      return "warn";
    case "failed":
      return "danger";
    default:
      return "secondary";
  }
}

// One label+input+Save row, shared by the auth credential and every
// manifest-declared settings field. Values are never pre-filled from the
// engine (it never sends them back) — only a `valueSet` boolean decides the
// placeholder, so a saved secret can only ever be replaced, never revealed.
//
// Widget-by-kind: `bool` renders a `Switch` that saves immediately on
// toggle (no separate Save step — matches every other boolean setting in
// Cockpit, e.g. the plugin's own "Enabled" switch above); a non-empty
// `options` list renders a `Combobox` (enum/choice); `int` renders a
// numeric `Input`; anything else renders the original text/password
// `Input`. `onSave` always receives the value to persist explicitly (rather
// than reading component state) so the Bool row's immediate save can pass
// its freshly toggled value without racing the parent's async state update.
function FieldRow({
  label,
  help,
  kind = "string",
  secret,
  required,
  valueSet,
  value,
  options = [],
  defaultValue = null,
  onChange,
  onSave,
  saving,
}: {
  label: string;
  help?: string;
  /** `PluginFieldInfo.kind` — `"string" | "int" | "bool"` in practice, but
   *  typed loosely (matches the DTO's plain `string`) so an unrecognized
   *  value falls through to the default text/password `Input` rather than
   *  failing a type check. */
  kind?: string;
  secret: boolean;
  required: boolean;
  valueSet: boolean;
  value: string;
  options?: string[];
  defaultValue?: string | null;
  onChange: (v: string) => void;
  onSave: (v: string) => void;
  saving: boolean;
}) {
  const fieldLabel = required ? `${label} *` : label;
  const placeholder = valueSet
    ? "●●●● saved"
    : defaultValue != null
      ? `Default: ${defaultValue}`
      : required
        ? "Required — not set"
        : "Optional — not set";

  if (kind === "bool") {
    const on = value === "true" || (value === "" && defaultValue === "true");
    return (
      <div className="border-b border-border px-[18px] py-3 last:border-b-0">
        <div className="flex items-center justify-between gap-2">
          <span className="text-[13px] font-medium">{fieldLabel}</span>
          <span className={saving ? "pointer-events-none opacity-40" : ""}>
            <Switch
              on={on}
              onToggle={() => {
                const next = on ? "false" : "true";
                onChange(next);
                onSave(next);
              }}
              label={label}
            />
          </span>
        </div>
        {help && <p className="m-0 mt-1.5 text-xs text-muted-foreground">{help}</p>}
      </div>
    );
  }

  return (
    <div className="border-b border-border px-[18px] py-3 last:border-b-0">
      <div className="flex items-end gap-2">
        <FormField label={fieldLabel} className="min-w-0 flex-1">
          {options.length > 0 ? (
            <Combobox
              aria-label={label}
              options={options.map((o) => ({ value: o, label: o }))}
              value={value || null}
              onValueChange={onChange}
              placeholder={placeholder}
              className="w-full"
            />
          ) : (
            <Input
              type={kind === "int" ? "number" : secret ? "password" : "text"}
              value={value}
              onChange={(e) => onChange(e.target.value)}
              placeholder={placeholder}
            />
          )}
        </FormField>
        {/* Outside the FormField's <label> on purpose — button is a labelable
            element too, so nesting it inside would fold the label's (and
            hint's) text into "Save"'s accessible name. */}
        <Button size="sm" onClick={() => onSave(value)} disabled={saving || value.trim().length === 0}>
          {saving ? "Saving…" : "Save"}
        </Button>
      </div>
      {help && <p className="m-0 mt-1.5 text-xs text-muted-foreground">{help}</p>}
    </div>
  );
}

export function PluginDetailView({ id }: { id: string }) {
  const nav = useNav();
  const { setEnabled, load: reloadPlugins, update: updatePlugin, pin: pinPlugin, doctorFindings, doctorLoaded, loadDoctor } = usePlugins();
  const [detail, setDetail] = useState<PluginDetail | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [authValue, setAuthValue] = useState("");
  const [savingAuth, setSavingAuth] = useState(false);
  const [fieldValues, setFieldValues] = useState<Record<string, string>>({});
  const [savingField, setSavingField] = useState<string | null>(null);
  const [oauthStateToken, setOauthStateToken] = useState<string | null>(null);
  const [oauthAuthorizeUrl, setOauthAuthorizeUrl] = useState("");
  const [oauthRedirectUri, setOauthRedirectUri] = useState("");
  const [oauthCode, setOauthCode] = useState("");
  const [oauthBusy, setOauthBusy] = useState<"begin" | "complete" | "disconnect" | null>(null);
  const [updatingPack, setUpdatingPack] = useState(false);
  const [extensionEntries, setExtensionEntries] = useState<ExtensionStatusEntry[]>([]);
  // Scroll targets for the attach-failure banner's "Configure" affordance —
  // whichever of Authentication/Settings actually rendered (each ref only
  // attaches when its section is present, so an absent section reads as
  // `null` rather than pointing at an empty wrapper).
  const authRef = useRef<HTMLDivElement>(null);
  const settingsRef = useRef<HTMLDivElement>(null);

  const load = useCallback(async () => {
    const res = await commands.pluginDetail(LOCAL_RUNNER, id);
    if (res.status === "ok") setDetail(res.data);
    else toast.error(`Couldn't load plugin: ${res.error.message}`);
    setLoaded(true);
  }, [id]);

  useEffect(() => {
    setDetail(null);
    setLoaded(false);
    setAuthValue("");
    setFieldValues({});
    setOauthStateToken(null);
    setOauthAuthorizeUrl("");
    setOauthRedirectUri("");
    setOauthCode("");
    setOauthBusy(null);
    void load();
  }, [load]);

  useEffect(() => {
    if (!doctorLoaded) void loadDoctor();
  }, [doctorLoaded, loadDoctor]);

  // Extension (Track D "code plugin") status — DT8. `extension_status` is a
  // params-free rpc returning every plugin's entries (mirrors `catalog_status`),
  // so this view fetches it only when the plugin actually declares the
  // capability, then filters down to its own `id` client-side (same pattern
  // `doctorFindings.find((f) => f.pluginId === id ...)` above uses).
  const isExtensionPlugin = detail?.info.capabilities.includes("extension") ?? false;
  useEffect(() => {
    if (!isExtensionPlugin) {
      setExtensionEntries([]);
      return;
    }
    let active = true;
    void commands.extensionStatus(LOCAL_RUNNER).then((res) => {
      if (active && res.status === "ok") setExtensionEntries(res.data.filter((e) => e.pluginId === id));
    });
    return () => {
      active = false;
    };
  }, [isExtensionPlugin, id]);

  useEffect(() => {
    let active = true;
    let unlisten: (() => void) | null = null;

    void events.pluginOauthAuthorizeUrlMsg
      .listen((event) => {
        if (!active || event.payload.pluginId !== id) return;
        setOauthAuthorizeUrl(event.payload.authorizeUrl);
      })
      .then((stop) => {
        if (active) unlisten = stop;
        else stop();
      });

    return () => {
      active = false;
      unlisten?.();
    };
  }, [id]);

  // Loopback completions land as an event (the install wizard's callback
  // server also serves flows begun here) — pick them up so Connect finishes
  // without the manual code paste. The paste UI stays as the fallback.
  useEffect(() => {
    let active = true;
    let unlisten: (() => void) | null = null;

    void events.pluginOauthCompletedMsg
      .listen((event) => {
        if (!active || event.payload.pluginId !== id) return;
        if (!event.payload.ok) {
          toast.error(event.payload.error ?? "OAuth sign-in didn't finish.");
          return;
        }
        toast.success("Connected");
        setOauthStateToken(null);
        setOauthAuthorizeUrl("");
        setOauthRedirectUri("");
        setOauthCode("");
        void load().then(() => reloadPlugins());
      })
      .then((stop) => {
        if (active) unlisten = stop;
        else stop();
      });

    return () => {
      active = false;
      unlisten?.();
    };
  }, [id, load, reloadPlugins]);

  if (!loaded || !detail) {
    return (
      <div className="min-h-0 flex-1 overflow-y-auto px-8 pb-10 pt-[22px]">
        <div className="mx-auto max-w-[720px]">
          <BackButton label="Back" onClick={() => nav.goBack()} />
          <div className="text-[13px] text-muted-foreground">{loaded ? "Plugin not found." : "Loading…"}</div>
        </div>
      </div>
    );
  }

  const { info } = detail;
  const Icon = pluginIcon(info.icon);
  const experimental = info.experimental;
  // The source of truth on load is the ledger's persisted `pinned` flag —
  // `pin()` still paints `usePlugins`' state optimistically before this
  // view's next `load()` brings back the authoritative value.
  const pinned = info.pinned;
  // Doctor's `attach-failed` finding is the only signal today for a
  // connector that failed to attach — `PluginDetail` itself carries no
  // attach-status field (see the Task 11 report's DTO-gap note).
  const attachFailure = doctorFindings.find((f) => f.pluginId === id && f.kind === "attach-failed");

  const onToggleEnabled = async () => {
    if (experimental) return;
    await setEnabled(id, !info.enabled);
    await load();
  };

  const onUpdatePack = async () => {
    if (updatingPack) return;
    setUpdatingPack(true);
    await updatePlugin(id, false);
    setUpdatingPack(false);
    await load();
  };

  const onTogglePin = async () => {
    // `pin()` reloads the LIST store; this view's `detail.info.pinned` comes
    // from a separate `pluginDetail` fetch, so reload it too or the pill/
    // button would stay on the pre-toggle value until the next navigation.
    await pinPlugin(id, !pinned, pinned ? undefined : "Pinned from Cockpit");
    await load();
  };

  const scrollToConfigure = () => {
    (authRef.current ?? settingsRef.current)?.scrollIntoView({ behavior: "smooth", block: "start" });
  };

  const saveAuth = async () => {
    if (!detail.auth?.setting || authValue.trim().length === 0 || savingAuth) return;
    setSavingAuth(true);
    const res = await commands.setPluginSetting(LOCAL_RUNNER, detail.auth.setting, authValue.trim());
    if (res.status === "error") toast.error(res.error.message);
    else {
      toast.success("Saved");
      setAuthValue("");
    }
    setSavingAuth(false);
    await load();
    await reloadPlugins();
  };

  // Takes the value explicitly (rather than reading `fieldValues[key]`
  // itself) so a `FieldRow`'s immediate-save kinds (Bool's toggle) can pass
  // their freshly computed value without racing `setFieldValues`'s async
  // state update.
  const saveField = async (key: string, rawValue: string) => {
    const value = rawValue.trim();
    if (value.length === 0 || savingField) return;
    setSavingField(key);
    const res = await commands.setPluginSetting(LOCAL_RUNNER, key, value);
    if (res.status === "error") toast.error(res.error.message);
    else {
      toast.success("Saved");
      setFieldValues((v) => ({ ...v, [key]: "" }));
    }
    setSavingField(null);
    await load();
    await reloadPlugins();
  };

  const startOauth = async () => {
    if (!detail?.auth || oauthBusy) return;
    setOauthBusy("begin");
    const res = await commands.beginPluginOauth(LOCAL_RUNNER, id);
    if (res.status === "error") {
      toast.error(res.error.message);
      setOauthBusy(null);
      return;
    }
    setOauthStateToken(res.data.stateToken);
    setOauthAuthorizeUrl(res.data.authorizeUrl);
    setOauthRedirectUri(res.data.redirectUri);
    setOauthCode("");
    setOauthBusy(null);
  };

  const completeOauth = async () => {
    if (!oauthStateToken || oauthCode.trim().length === 0 || oauthBusy) return;
    setOauthBusy("complete");
    const res = await commands.completePluginOauth(LOCAL_RUNNER, id, oauthCode.trim(), oauthStateToken);
    if (res.status === "error") {
      toast.error(res.error.message);
      setOauthBusy(null);
      return;
    }
    toast.success("Connected");
    setOauthStateToken(null);
    setOauthAuthorizeUrl("");
    setOauthRedirectUri("");
    setOauthCode("");
    setOauthBusy(null);
    await load();
    await reloadPlugins();
  };

  const disconnectOauth = async () => {
    if (!detail?.auth?.oauthTokenStored || oauthBusy) return;
    setOauthBusy("disconnect");
    const res = await commands.disconnectPluginOauth(LOCAL_RUNNER, id);
    if (res.status === "error") toast.error(res.error.message);
    else {
      toast.success("Disconnected");
      setOauthStateToken(null);
      setOauthAuthorizeUrl("");
      setOauthRedirectUri("");
      setOauthCode("");
      await load();
      await reloadPlugins();
    }
    setOauthBusy(null);
  };

  const cancelOauth = () => {
    setOauthStateToken(null);
    setOauthAuthorizeUrl("");
    setOauthRedirectUri("");
    setOauthCode("");
  };

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-8 pb-10 pt-[22px]">
      <div className="mx-auto max-w-[720px]">
        <BackButton label="Back" onClick={() => nav.goBack()} />

        <DetailHeader chip={<IconChip icon={Icon} size={44} />} title={info.name} sub={detail.publisher || info.description || info.id}>
          {info.kind === "skill-pack" && (
            <>
              <Button variant="outline" size="sm" onClick={() => void onUpdatePack()} disabled={updatingPack}>
                <RefreshCw aria-hidden size={13} strokeWidth={2} className={updatingPack ? "animate-spin" : undefined} />
                {updatingPack ? "Updating…" : "Update"}
              </Button>
              <Button variant="outline" size="sm" onClick={() => void onTogglePin()}>
                {pinned ? <PinOff aria-hidden size={13} strokeWidth={2} /> : <Pin aria-hidden size={13} strokeWidth={2} />}
                {pinned ? "Unpin" : "Pin"}
              </Button>
            </>
          )}
          <span className={experimental ? "pointer-events-none opacity-40" : ""}>
            <Switch on={info.enabled} onToggle={() => void onToggleEnabled()} label="Enabled" />
          </span>
        </DetailHeader>

        <div className="mb-4 flex flex-wrap items-center gap-1.5">
          <PluginStatusBadge verified={info.verified} experimental={info.experimental} />
          {info.capabilities.includes("extension") && <Pill variant="mono">Runs code</Pill>}
          {pinned && (
            <Pill variant="mono">
              <Pin aria-hidden size={9} strokeWidth={2} className="mr-1 inline align-[-1px]" />
              Pinned
            </Pill>
          )}
          {info.categories.map((c) => (
            <Badge key={c} variant="outline">
              {c}
            </Badge>
          ))}
        </div>

        {(info.sourceSpec || info.resolvedCommit || info.installedAt != null || info.updatedAt != null) && (
          <Card className="mb-3">
            <CardHeader>
              <CardTitle>Provenance</CardTitle>
            </CardHeader>
            {info.sourceSpec && (
              <CardRow>
                <span className="w-[100px] shrink-0 text-[13px] font-medium">Source</span>
                <span className="min-w-0 flex-1 truncate font-mono text-xs text-muted-foreground">{info.sourceSpec}</span>
              </CardRow>
            )}
            {info.resolvedCommit && (
              <CardRow>
                <span className="w-[100px] shrink-0 text-[13px] font-medium">Commit</span>
                <span className="min-w-0 flex-1 truncate font-mono text-xs text-muted-foreground">{shortCommit(info.resolvedCommit)}</span>
              </CardRow>
            )}
            {info.installedAt != null && (
              <CardRow>
                <span className="w-[100px] shrink-0 text-[13px] font-medium">Installed</span>
                <span className="min-w-0 flex-1 truncate text-xs text-muted-foreground">{formatLedgerTimestamp(info.installedAt)}</span>
              </CardRow>
            )}
            {info.updatedAt != null && (
              <CardRow>
                <span className="w-[100px] shrink-0 text-[13px] font-medium">Updated</span>
                <span className="min-w-0 flex-1 truncate text-xs text-muted-foreground">{formatLedgerTimestamp(info.updatedAt)}</span>
              </CardRow>
            )}
          </Card>
        )}

        {attachFailure && (
          <Card className="mb-3 flex items-start gap-3 px-[18px] py-3.5">
            <CircleAlert aria-hidden size={16} strokeWidth={2} className="mt-px shrink-0" style={{ color: WARN }} />
            <div className="min-w-0 flex-1">
              <div className="text-[13.5px] font-semibold">Attach failed</div>
              <div className="mt-1 text-[12.5px] text-muted-foreground">{attachFailure.message}</div>
              <div className="mt-1 text-[11.5px] text-muted-foreground">{attachFailure.suggestedAction}</div>
            </div>
            <Button variant="outline" size="sm" onClick={scrollToConfigure} className="shrink-0">
              Configure
            </Button>
          </Card>
        )}

        <Card className="mb-3">
          <CardHeader>
            <CardTitle>About</CardTitle>
          </CardHeader>
          <div className="px-[18px] py-3.5 text-[12.5px] leading-[1.55] text-muted-foreground">
            {info.description || "No description provided."}
          </div>
          {detail.homepage && (
            <CardRow>
              <span className="w-[100px] shrink-0 text-[13px] font-medium">Homepage</span>
              <span className="min-w-0 flex-1 truncate font-mono text-xs text-muted-foreground">{detail.homepage}</span>
              <Button variant="outline" size="sm" onClick={() => void openUrl(detail.homepage as string)}>
                <ExternalLink aria-hidden size={12} strokeWidth={2} className="size-3" />
                Open
              </Button>
            </CardRow>
          )}
        </Card>

        {detail.auth && detail.auth.kind !== "none" && (
          <div ref={authRef}>
            <Card className="mb-3">
              <CardHeader>
                <CardTitle>Authentication</CardTitle>
                <Pill variant={detail.auth.configured ? "primary" : "secondary"}>
                  {detail.auth.kind === "oauth" && detail.auth.oauthReconnectRequired
                    ? "Reconnect required"
                    : detail.auth.configured
                      ? "Configured"
                      : "Not configured"}
                </Pill>
              </CardHeader>
              {detail.auth.kind === "oauth" ? (
                <>
                  <div className="px-[18px] py-3.5 text-[12.5px] text-muted-foreground">
                    {detail.auth.oauthConnectAvailable
                      ? detail.auth.oauthReconnectRequired
                        ? "Cockpit has a saved token for this plugin, but it needs to be reconnected."
                        : detail.auth.oauthTokenStored
                          ? "Cockpit has a saved OAuth token for this plugin."
                          : "Cockpit can start OAuth for this plugin. After the browser redirects, paste the returned code below to finish connecting."
                      : (detail.auth.oauthConnectError ??
                        "Cockpit needs an authorize URL, token URL, and a saved client ID before it can start OAuth for this plugin.")}
                  </div>
                  {detail.auth.oauthConnectAvailable && (
                    <div className="border-t border-border px-[18px] py-3">
                      <div className="flex flex-wrap items-center justify-end gap-2">
                        {detail.auth.oauthTokenStored && (
                          <Button variant="outline" size="sm" onClick={() => void disconnectOauth()} disabled={oauthBusy !== null}>
                            {oauthBusy === "disconnect" ? "Disconnecting…" : "Disconnect"}
                          </Button>
                        )}
                        <Button size="sm" onClick={() => void startOauth()} disabled={oauthBusy !== null}>
                          {oauthBusy === "begin"
                            ? "Opening…"
                            : detail.auth.oauthReconnectRequired || detail.auth.oauthTokenStored
                              ? "Reconnect"
                              : "Connect"}
                        </Button>
                      </div>
                    </div>
                  )}
                  {oauthStateToken && (
                    <>
                      <div className="border-t border-border px-[18px] py-3">
                        <FormField label="Login URL">
                          <div className="flex min-w-0 gap-2">
                            <Input
                              readOnly
                              value={oauthAuthorizeUrl}
                              onFocus={(event) => event.currentTarget.select()}
                              className="min-w-0 font-mono text-[11.5px]"
                            />
                            <Button
                              variant="outline"
                              size="sm"
                              onClick={() => void openUrl(oauthAuthorizeUrl)}
                              disabled={oauthAuthorizeUrl.length === 0 || oauthBusy !== null}
                              className="shrink-0"
                            >
                              Open
                            </Button>
                          </div>
                        </FormField>
                        <div className="mt-3">
                          <FormField label="Authorization code">
                            <Input
                              value={oauthCode}
                              onChange={(event) => setOauthCode(event.target.value)}
                              placeholder="Paste the code value from the callback URL"
                            />
                          </FormField>
                        </div>
                        <p className="m-0 mt-1.5 text-xs text-muted-foreground">
                          Callback URL: <span className="font-mono text-[11px]">{oauthRedirectUri}</span>
                        </p>
                      </div>
                      <div className="flex justify-end gap-2 border-t border-border px-[18px] py-3">
                        <Button variant="outline" size="sm" onClick={cancelOauth} disabled={oauthBusy !== null}>
                          Cancel
                        </Button>
                        <Button
                          size="sm"
                          onClick={() => void completeOauth()}
                          disabled={oauthBusy !== null || oauthCode.trim().length === 0}
                        >
                          {oauthBusy === "complete" ? "Connecting…" : "Finish connect"}
                        </Button>
                      </div>
                    </>
                  )}
                </>
              ) : detail.auth.setting ? (
                <FieldRow
                  label="Credential"
                  help={detail.auth.env ? `Falls back to the ${detail.auth.env} environment variable if unset.` : undefined}
                  secret
                  required
                  valueSet={detail.auth.configured}
                  value={authValue}
                  onChange={setAuthValue}
                  onSave={() => void saveAuth()}
                  saving={savingAuth}
                />
              ) : (
                <div className="px-[18px] py-3.5 text-[12.5px] text-muted-foreground">
                  {detail.auth.env && (
                    <>
                      Set the <span className="font-mono text-xs">{detail.auth.env}</span> environment variable.
                    </>
                  )}
                  {!detail.auth.env && "No credential required beyond enabling the plugin."}
                </div>
              )}
              {detail.auth.helpUrl && (
                <div className="flex justify-end border-t border-border px-[18px] py-3">
                  <Button variant="outline" size="sm" onClick={() => void openUrl(detail.auth?.helpUrl as string)}>
                    <ExternalLink aria-hidden size={12} strokeWidth={2} className="size-3" />
                    Help
                  </Button>
                </div>
              )}
            </Card>
          </div>
        )}

        {detail.settings.length > 0 && (
          <div ref={settingsRef}>
            <Card className="mb-3">
              <CardHeader>
                <CardTitle>Settings</CardTitle>
              </CardHeader>
              {detail.settings.map((f) => (
                <FieldRow
                  key={f.key}
                  label={f.label}
                  help={f.help || undefined}
                  kind={f.kind}
                  secret={f.secret}
                  required={f.required}
                  valueSet={f.valueSet}
                  value={fieldValues[f.key] ?? ""}
                  options={f.options}
                  defaultValue={f.default}
                  onChange={(v) => setFieldValues((m) => ({ ...m, [f.key]: v }))}
                  onSave={(v) => void saveField(f.key, v)}
                  saving={savingField === f.key}
                />
              ))}
            </Card>
          </div>
        )}

        {detail.mcp.length > 0 && (
          <Card className="mb-3">
            <CardHeader>
              <CardTitle>MCP servers</CardTitle>
            </CardHeader>
            {detail.mcp.map((m) => (
              <CardRow key={m.name}>
                <span className="w-[120px] shrink-0 text-[13px] font-medium">{m.name}</span>
                <Pill variant="mono">{m.transport}</Pill>
                <span className="min-w-0 flex-1 truncate font-mono text-xs text-muted-foreground">{m.commandOrUrl}</span>
              </CardRow>
            ))}
          </Card>
        )}

        {info.capabilities.includes("extension") && (
          <Card className="mb-3">
            <CardHeader>
              <CardTitle>Extension</CardTitle>
            </CardHeader>
            {extensionEntries.length === 0 ? (
              <div className="px-[18px] py-3.5 text-[12.5px] text-muted-foreground">No extension status reported yet.</div>
            ) : (
              extensionEntries.map((e) => (
                <CardRow key={e.name}>
                  <span className="w-[120px] shrink-0 truncate text-[13px] font-medium">{e.name}</span>
                  <Pill variant={extensionStatusPillVariant(e.status)}>{extensionStatusLabel(e.status)}</Pill>
                  {e.restartCount > 0 && (
                    <span className="shrink-0 text-[11.5px] text-muted-foreground">
                      {e.restartCount} restart{e.restartCount === 1 ? "" : "s"}
                    </span>
                  )}
                  {e.lastError && <span className="min-w-0 flex-1 truncate text-[11.5px] text-muted-foreground">{e.lastError}</span>}
                </CardRow>
              ))
            )}
          </Card>
        )}

        {info.capabilities.includes("provider") && (
          <Card>
            <CardHeader>
              <CardTitle>Models</CardTitle>
              <CardHint>{detail.models.length} available</CardHint>
            </CardHeader>
            {detail.models.length === 0 ? (
              <div className="px-[18px] py-3.5 text-[12.5px] text-muted-foreground">No models detected.</div>
            ) : (
              detail.models.map((m) => (
                <CardRow key={m}>
                  <span className="flex-1 truncate font-mono text-xs">{m}</span>
                </CardRow>
              ))
            )}
          </Card>
        )}
      </div>
    </div>
  );
}
