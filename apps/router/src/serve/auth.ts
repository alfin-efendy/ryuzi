// apps/router/src/serve/auth.ts
import { jwtVerify, createLocalJWKSet, createRemoteJWKSet, type JWTPayload, type JSONWebKeySet } from "jose";
import type { SettingsStore } from "../config/store";

export interface AuthResult {
  actor: string;
}

export interface Authenticator {
  authenticate(authHeader: string | null): Promise<AuthResult | null>;
  issueTicket(actor: string): string;
  consumeTicket(ticket: string): AuthResult | null;
}

interface AuthDeps {
  settings: SettingsStore;
  localToken: string;
  /** Test-only: use a fixed JWKS instead of fetching from the issuer. */
  jwksOverride?: JSONWebKeySet;
}

const TICKET_TTL_MS = 30_000;

function bearer(authHeader: string | null): string | null {
  if (!authHeader) return null;
  const m = /^Bearer (.+)$/.exec(authHeader);
  return m ? (m[1] ?? null) : null;
}

export function createAuthenticator(deps: AuthDeps): Authenticator {
  const tickets = new Map<string, { actor: string; expires: number }>();

  // Lazily-built JWKS verifier for OIDC mode.
  type JWKS = ReturnType<typeof createLocalJWKSet>;
  let jwks: JWKS | undefined;
  function getJwks(): JWKS {
    if (jwks) return jwks;
    if (deps.jwksOverride) {
      jwks = createLocalJWKSet(deps.jwksOverride);
      return jwks;
    }
    const explicit = deps.settings.get("oidc.jwks_uri");
    const issuer = deps.settings.get("oidc.issuer");
    const uri = explicit || (issuer ? `${issuer.replace(/\/$/, "")}/.well-known/jwks.json` : "");
    if (!uri) throw new Error("oidc.issuer or oidc.jwks_uri must be set for oidc auth mode");
    jwks = createRemoteJWKSet(new URL(uri)) as unknown as JWKS;
    return jwks;
  }

  async function authenticate(authHeader: string | null): Promise<AuthResult | null> {
    const token = bearer(authHeader);
    if (!token) return null;
    const mode = deps.settings.get("serve.auth_mode") ?? "loopback";
    if (mode === "loopback") {
      return token === deps.localToken ? { actor: "local" } : null;
    }
    // oidc
    try {
      const issuer = deps.settings.get("oidc.issuer") ?? undefined;
      const audience = deps.settings.get("oidc.audience") ?? undefined;
      const { payload } = await jwtVerify(token, getJwks(), { issuer, audience });
      return { actor: actorFromClaims(payload) };
    } catch {
      return null;
    }
  }

  function actorFromClaims(p: JWTPayload): string {
    const email = typeof p.email === "string" ? p.email : undefined;
    return email ?? (typeof p.sub === "string" ? p.sub : "unknown");
  }

  function issueTicket(actor: string): string {
    const ticket = crypto.randomUUID();
    tickets.set(ticket, { actor, expires: Date.now() + TICKET_TTL_MS });
    return ticket;
  }

  function consumeTicket(ticket: string): AuthResult | null {
    const entry = tickets.get(ticket);
    if (!entry) return null;
    tickets.delete(ticket);
    if (Date.now() > entry.expires) return null;
    return { actor: entry.actor };
  }

  return { authenticate, issueTicket, consumeTicket };
}
