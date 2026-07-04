//! OAuth provider connections: PKCE, authorize-URL + token exchange, loopback
//! callback, and live-traffic token refresh.
//! Ported from 9router (MIT, (c) 2024-2026 decolua and contributors) —
//! flows from src/lib/oauth/* and open-sse/services/tokenRefresh/*.
pub mod pkce;
