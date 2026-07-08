import { openUrl } from "@tauri-apps/plugin-opener";
import { ExternalLink } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { toast } from "sonner";
import {
  Badge,
  Button,
  FormField,
  Input,
  SettingsCard as Card,
  SettingsCardHeader as CardHeader,
  SettingsCardHint as CardHint,
  SettingsCardRow as CardRow,
  SettingsCardTitle as CardTitle,
  Switch,
} from "@ryuzi/ui";
import { commands, events, type PluginDetail } from "@/bindings";
import { BackButton, DetailHeader } from "@/components/common/DetailHeader";
import { IconChip, Pill, PluginStatusBadge } from "@/components/common/bits";
import { pluginIcon } from "@/lib/plugin-icons";
import { useNav } from "@/store-nav";
import { usePlugins } from "@/store-plugins";

// One label+input+Save row, shared by the auth credential and every
// manifest-declared settings field. Values are never pre-filled from the
// engine (it never sends them back) — only a `valueSet` boolean decides the
// placeholder, so a saved secret can only ever be replaced, never revealed.
function FieldRow({
  label,
  help,
  secret,
  required,
  valueSet,
  value,
  onChange,
  onSave,
  saving,
}: {
  label: string;
  help?: string;
  secret: boolean;
  required: boolean;
  valueSet: boolean;
  value: string;
  onChange: (v: string) => void;
  onSave: () => void;
  saving: boolean;
}) {
  return (
    <div className="border-b border-border px-[18px] py-3 last:border-b-0">
      <div className="flex items-end gap-2">
        <FormField label={required ? `${label} *` : label} className="min-w-0 flex-1">
          <Input
            type={secret ? "password" : "text"}
            value={value}
            onChange={(e) => onChange(e.target.value)}
            placeholder={valueSet ? "●●●● saved" : required ? "Required — not set" : "Optional — not set"}
          />
        </FormField>
        {/* Outside the FormField's <label> on purpose — button is a labelable
            element too, so nesting it inside would fold the label's (and
            hint's) text into "Save"'s accessible name. */}
        <Button size="sm" onClick={onSave} disabled={saving || value.trim().length === 0}>
          {saving ? "Saving…" : "Save"}
        </Button>
      </div>
      {help && <p className="m-0 mt-1.5 text-xs text-muted-foreground">{help}</p>}
    </div>
  );
}

export function PluginDetailView({ id }: { id: string }) {
  const nav = useNav();
  const { setEnabled, load: reloadPlugins } = usePlugins();
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

  const load = useCallback(async () => {
    const res = await commands.pluginDetail(id);
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

  const onToggleEnabled = async () => {
    if (experimental) return;
    await setEnabled(id, !info.enabled);
    await load();
  };

  const saveAuth = async () => {
    if (!detail.auth?.setting || authValue.trim().length === 0 || savingAuth) return;
    setSavingAuth(true);
    const res = await commands.setPluginSetting(detail.auth.setting, authValue.trim());
    if (res.status === "error") toast.error(res.error.message);
    else {
      toast.success("Saved");
      setAuthValue("");
    }
    setSavingAuth(false);
    await load();
    await reloadPlugins();
  };

  const saveField = async (key: string) => {
    const value = (fieldValues[key] ?? "").trim();
    if (value.length === 0 || savingField) return;
    setSavingField(key);
    const res = await commands.setPluginSetting(key, value);
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
    const res = await commands.beginPluginOauth(id);
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
    const res = await commands.completePluginOauth(id, oauthCode.trim(), oauthStateToken);
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
    const res = await commands.disconnectPluginOauth(id);
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
          <span className={experimental ? "pointer-events-none opacity-40" : ""}>
            <Switch on={info.enabled} onToggle={() => void onToggleEnabled()} label="Enabled" />
          </span>
        </DetailHeader>

        <div className="mb-4 flex flex-wrap items-center gap-1.5">
          <PluginStatusBadge verified={info.verified} experimental={info.experimental} />
          {info.categories.map((c) => (
            <Badge key={c} variant="outline">
              {c}
            </Badge>
          ))}
        </div>

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
                      <Button size="sm" onClick={() => void completeOauth()} disabled={oauthBusy !== null || oauthCode.trim().length === 0}>
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
        )}

        {detail.settings.length > 0 && (
          <Card className="mb-3">
            <CardHeader>
              <CardTitle>Settings</CardTitle>
            </CardHeader>
            {detail.settings.map((f) => (
              <FieldRow
                key={f.key}
                label={f.label}
                help={f.help || undefined}
                secret={f.secret}
                required={f.required}
                valueSet={f.valueSet}
                value={fieldValues[f.key] ?? ""}
                onChange={(v) => setFieldValues((m) => ({ ...m, [f.key]: v }))}
                onSave={() => void saveField(f.key)}
                saving={savingField === f.key}
              />
            ))}
          </Card>
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
