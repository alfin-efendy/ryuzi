// apps/ide/src/main/connection-manager.ts
import type { RemoteControlPlane } from "@harness/client";
import { CONNECTIONS_CHANNEL, type ConnectionSummary, type AddConnectionInput } from "../shared/ipc-contract";
import type { ConnectionsStore } from "./connections";
import type { TokenStore } from "./token-store";
import type { OidcClient } from "./oidc";
import { runLoopbackAuth } from "./oidc";
import type { ClientHandle } from "./client";
import { discoverLocalRouter, type RouterInfo } from "./discover";

interface Deps {
  store: ConnectionsStore;
  tokens: TokenStore;
  oidc: OidcClient;
  send: (channel: string, payload: unknown) => void;
  makeClient: (info: RouterInfo, send: (c: string, p: unknown) => void) => ClientHandle;
  openExternal: (url: string) => void;
  discoverLocal?: () => RouterInfo | null;
}

export class ConnectionManager {
  private handle: ClientHandle | null = null;
  private discoverLocal: () => RouterInfo | null;

  constructor(private d: Deps) {
    this.discoverLocal = d.discoverLocal ?? discoverLocalRouter;
  }

  getClient(): RemoteControlPlane | null {
    return this.handle?.client ?? null;
  }

  list(): ConnectionSummary[] {
    return this.d.store.summaries((id) => this.d.tokens.has(id));
  }

  private emit(): void {
    this.d.send(CONNECTIONS_CHANNEL, this.list());
  }

  async startup(): Promise<void> {
    const localInfo = this.discoverLocal();
    this.d.store.setLocal(localInfo ? { url: localInfo.url } : null);
    const activeId = this.d.store.getActiveId();
    const target = activeId ?? (this.d.store.get("local") ? "local" : null);
    if (target) await this.select(target).catch(() => {});
    else this.emit();
  }

  async add(input: AddConnectionInput): Promise<void> {
    this.d.store.add(input);
    this.emit();
  }

  async remove(id: string): Promise<void> {
    const wasActive = this.d.store.getActiveId() === id;
    this.d.tokens.clear(id);
    this.d.store.remove(id);
    if (wasActive && this.handle) {
      this.handle.dispose();
      this.handle = null;
    }
    this.emit();
  }

  private async tokenFor(profileId: string): Promise<string | null> {
    const p = this.d.store.get(profileId);
    if (!p) return null;
    if (p.authMode === "loopback") {
      return this.discoverLocal()?.token ?? null;
    }
    return this.d.tokens.getAccessToken(p.id, (rt) =>
      this.d.oidc.refresh({ issuer: p.oidc!.issuer, clientId: p.oidc!.clientId, scopes: p.oidc!.scopes }, rt),
    );
  }

  async select(id: string): Promise<void> {
    const p = this.d.store.get(id);
    if (!p) return;
    this.d.store.setActive(id);
    this.handle?.dispose();
    this.handle = null;
    const token = await this.tokenFor(id);
    if (token === null) {
      // oidc profile needs sign-in
      this.emit();
      return;
    }
    this.handle = this.d.makeClient({ url: p.baseUrl, token }, this.d.send);
    await this.handle.connect().catch((e) => console.error("connect failed:", e));
    this.emit();
  }

  async signIn(id: string): Promise<void> {
    const p = this.d.store.get(id);
    if (!p || p.authMode !== "oidc" || !p.oidc) return;
    const set = await runLoopbackAuth(this.d.oidc, p.oidc, this.d.openExternal);
    this.d.tokens.save(id, set);
    this.emit();
    if (this.d.store.getActiveId() === id) await this.select(id);
  }

  async signOut(id: string): Promise<void> {
    this.d.tokens.clear(id);
    if (this.d.store.getActiveId() === id) {
      this.handle?.dispose();
      this.handle = null;
    }
    this.emit();
  }
}
