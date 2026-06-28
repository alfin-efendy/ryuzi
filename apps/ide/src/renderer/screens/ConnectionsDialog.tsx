import React, { useState } from "react";
import { useStore } from "../store";
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogTrigger } from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";

export function ConnectionsDialog({ defaultOpen = false }: { defaultOpen?: boolean }) {
  const [open, setOpen] = useState(defaultOpen);
  const connections = useStore((s) => s.connections);
  const [label, setLabel] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [authMode, setAuthMode] = useState<"loopback" | "oidc">("oidc");
  const [issuer, setIssuer] = useState("");
  const [clientId, setClientId] = useState("");
  const [scopes, setScopes] = useState("openid profile email");
  const [error, setError] = useState<string | null>(null);

  async function add() {
    setError(null);
    try {
      await window.harness.addConnection({
        label,
        baseUrl,
        authMode,
        oidc: authMode === "oidc" ? { issuer, clientId, scopes } : undefined,
      });
      setLabel("");
      setBaseUrl("");
    } catch (e) {
      setError((e as Error).message);
    }
  }

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button size="sm" variant="outline">
          Connections
        </Button>
      </DialogTrigger>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Connections</DialogTitle>
        </DialogHeader>
        <div className="space-y-2">
          {connections.map((c) => (
            <div key={c.id} className="flex items-center gap-2 rounded border p-2 text-sm">
              <span className="flex-1 truncate">
                {c.label} {c.active && <span className="text-green-500">●</span>}
                <span className="block text-xs text-muted-foreground">{c.baseUrl}</span>
              </span>
              {!c.active && (
                <Button size="sm" variant="ghost" onClick={() => void window.harness.selectConnection(c.id)}>
                  Select
                </Button>
              )}
              {c.authMode === "oidc" &&
                (c.signedIn ? (
                  <Button size="sm" variant="ghost" onClick={() => void window.harness.signOut(c.id)}>
                    Sign out
                  </Button>
                ) : (
                  <Button
                    size="sm"
                    variant="ghost"
                    onClick={async () => {
                      setError(null);
                      try {
                        await window.harness.signIn(c.id);
                      } catch (e) {
                        setError((e as Error).message);
                      }
                    }}
                  >
                    Sign in
                  </Button>
                ))}
              {c.id !== "local" && (
                <Button size="sm" variant="ghost" onClick={() => void window.harness.removeConnection(c.id)}>
                  Remove
                </Button>
              )}
            </div>
          ))}
        </div>
        <div className="space-y-2 border-t pt-3">
          <div className="text-xs uppercase tracking-wide text-muted-foreground">Add connection</div>
          <Input placeholder="label" value={label} onChange={(e) => setLabel(e.target.value)} />
          <Input placeholder="base URL (https://router.example.com)" value={baseUrl} onChange={(e) => setBaseUrl(e.target.value)} />
          <div className="flex gap-2 text-xs">
            <button
              type="button"
              className={authMode === "oidc" ? "font-semibold" : "text-muted-foreground"}
              onClick={() => setAuthMode("oidc")}
            >
              OIDC
            </button>
            <button
              type="button"
              className={authMode === "loopback" ? "font-semibold" : "text-muted-foreground"}
              onClick={() => setAuthMode("loopback")}
            >
              Loopback
            </button>
          </div>
          {authMode === "oidc" && (
            <>
              <Input placeholder="OIDC issuer" value={issuer} onChange={(e) => setIssuer(e.target.value)} />
              <Input placeholder="client ID" value={clientId} onChange={(e) => setClientId(e.target.value)} />
              <Input placeholder="scopes" value={scopes} onChange={(e) => setScopes(e.target.value)} />
            </>
          )}
          {error && <p className="text-xs text-destructive">{error}</p>}
          <Button onClick={() => void add()}>Add</Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}
