import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";

export interface TokenSet {
  accessToken: string;
  refreshToken?: string;
  expiresAt: number;
  idToken?: string;
}

export interface Vault {
  isAvailable(): boolean;
  encrypt(s: string): Buffer;
  decrypt(b: Buffer): string;
}

const SKEW_MS = 60_000;

export class TokenStore {
  private mem = new Map<string, TokenSet>();

  constructor(
    private dir: string,
    private vault: Vault,
  ) {}

  private file(id: string): string {
    return join(this.dir, `${id}.enc`);
  }

  save(id: string, set: TokenSet): void {
    this.mem.set(id, set);
    if (this.vault.isAvailable()) {
      mkdirSync(this.dir, { recursive: true });
      writeFileSync(this.file(id), this.vault.encrypt(JSON.stringify(set)), {
        mode: 0o600,
      });
    }
  }

  load(id: string): TokenSet | null {
    const m = this.mem.get(id);
    if (m) return m;
    if (this.vault.isAvailable() && existsSync(this.file(id))) {
      try {
        const set = JSON.parse(this.vault.decrypt(readFileSync(this.file(id)))) as TokenSet;
        this.mem.set(id, set);
        return set;
      } catch {
        return null;
      }
    }
    return null;
  }

  clear(id: string): void {
    this.mem.delete(id);
    try {
      rmSync(this.file(id), { force: true });
    } catch {
      // best-effort
    }
  }

  has(id: string): boolean {
    return this.load(id) !== null;
  }

  async getAccessToken(id: string, refresh: (refreshToken: string) => Promise<TokenSet>): Promise<string | null> {
    const set = this.load(id);
    if (!set) return null;
    if (set.expiresAt - Date.now() > SKEW_MS) return set.accessToken;
    if (!set.refreshToken) {
      this.clear(id);
      return null;
    }
    try {
      const next = await refresh(set.refreshToken);
      this.save(id, next);
      return next.accessToken;
    } catch {
      this.clear(id);
      return null;
    }
  }
}

// Production Vault backed by Electron safeStorage (imported lazily so tests can avoid electron).
export function safeStorageVault(): Vault {
  const { safeStorage } = require("electron");
  return {
    isAvailable: () => safeStorage.isEncryptionAvailable(),
    encrypt: (s) => safeStorage.encryptString(s),
    decrypt: (b) => safeStorage.decryptString(b),
  };
}
