import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";

export interface ConnectionProfile {
  id: string;
  label: string;
  baseUrl: string;
  authMode: "loopback" | "oidc";
  oidc?: { issuer: string; clientId: string; scopes: string };
}

interface Persisted {
  profiles: ConnectionProfile[];
  activeId: string | null;
}

export interface ConnectionSummaryRow {
  id: string;
  label: string;
  baseUrl: string;
  authMode: "loopback" | "oidc";
  active: boolean;
  signedIn: boolean;
}

export class ConnectionsStore {
  private state: Persisted = { profiles: [], activeId: null };
  private localUrl: string | null = null;

  constructor(private filePath: string) {
    if (existsSync(filePath)) {
      try {
        this.state = JSON.parse(readFileSync(filePath, "utf8")) as Persisted;
      } catch {
        this.state = { profiles: [], activeId: null };
      }
    }
  }

  private save(): void {
    mkdirSync(dirname(this.filePath), { recursive: true });
    writeFileSync(this.filePath, JSON.stringify(this.state), { mode: 0o600 });
  }

  add(input: Omit<ConnectionProfile, "id">): ConnectionProfile {
    const p: ConnectionProfile = { id: crypto.randomUUID(), ...input };
    this.state.profiles.push(p);
    this.save();
    return p;
  }

  remove(id: string): void {
    this.state.profiles = this.state.profiles.filter((p) => p.id !== id);
    if (this.state.activeId === id) this.state.activeId = null;
    this.save();
  }

  list(): ConnectionProfile[] {
    return this.state.profiles;
  }

  setActive(id: string): void {
    this.state.activeId = id;
    this.save();
  }

  getActiveId(): string | null {
    return this.state.activeId;
  }

  setLocal(info: { url: string } | null): void {
    this.localUrl = info?.url ?? null;
  }

  private localProfile(): ConnectionProfile | null {
    return this.localUrl
      ? {
          id: "local",
          label: "Local (hr serve)",
          baseUrl: this.localUrl,
          authMode: "loopback",
        }
      : null;
  }

  get(id: string): ConnectionProfile | undefined {
    if (id === "local") return this.localProfile() ?? undefined;
    return this.state.profiles.find((p) => p.id === id);
  }

  summaries(signedIn: (id: string) => boolean): ConnectionSummaryRow[] {
    const rows: ConnectionProfile[] = [];
    const local = this.localProfile();
    if (local) rows.push(local);
    rows.push(...this.state.profiles);
    return rows.map((p) => ({
      id: p.id,
      label: p.label,
      baseUrl: p.baseUrl,
      authMode: p.authMode,
      active: this.state.activeId === p.id,
      signedIn: p.authMode === "oidc" ? signedIn(p.id) : true,
    }));
  }
}
