// Design-preview fixtures for the Relay Desktop v3 screens whose backends do
// not exist yet (providers, agents, scheduler, apps, registry, review/terminal
// panels). Mirrors the mock data shipped inside the design spec so wiring a
// real backend later is a data-source swap, not a UI rewrite.

export type AgentId = "claude" | "codex" | "gemini" | "openclaw" | "local";
export type PermMode = "plan" | "ask" | "edit" | "full";

export type AgentFixture = {
  id: AgentId;
  name: string;
  model: string;
  color: string;
  connection: string;
  initial: string;
  version: string;
  latest: string;
  binary: string;
  models: string[];
  permMode: PermMode;
  flags: string;
  appAccess: Record<string, boolean>;
  changelog: { v: string; date: string; notes: string[] }[];
};

export const AGENTS: Record<AgentId, AgentFixture> = {
  claude: {
    id: "claude",
    name: "Claude Code",
    model: "Opus 4.5",
    color: "#D97757",
    connection: "Anthropic API",
    initial: "C",
    version: "2.1.4",
    latest: "2.2.0",
    binary: "~/.local/bin/claude",
    models: ["Opus 4.5", "Sonnet 4.5", "Haiku 4.5"],
    permMode: "ask",
    flags: "--output-format stream-json --max-turns 40",
    appAccess: { github: true, postgres: true, playwright: true, linear: true },
    changelog: [
      {
        v: "2.2.0",
        date: "Jul 1, 2026",
        notes: ["Parallel subagents in plan mode", "OAuth token refresh for MCP servers", "Fixes worktree cleanup after aborted runs"],
      },
      { v: "2.1.4", date: "Jun 24, 2026", notes: ["Lower token overhead in long sessions", "Fix crash on non-UTF8 diffs"] },
    ],
  },
  codex: {
    id: "codex",
    name: "OpenAI Codex",
    model: "GPT-5.2-Codex",
    color: "#0FA47F",
    connection: "ChatGPT account",
    initial: "O",
    version: "1.8.2",
    latest: "1.8.2",
    binary: "~/.local/bin/codex",
    models: ["GPT-5.2-Codex", "GPT-5.2", "o5-mini"],
    permMode: "edit",
    flags: "--sandbox workspace-write",
    appAccess: { github: true, postgres: true, playwright: true, linear: false },
    changelog: [{ v: "1.8.2", date: "Jun 30, 2026", notes: ["Faster repo indexing", "Better rate-limit backoff"] }],
  },
  gemini: {
    id: "gemini",
    name: "Gemini CLI",
    model: "3.0 Pro",
    color: "#4285F4",
    connection: "Google Cloud",
    initial: "G",
    version: "0.12.1",
    latest: "0.13.0",
    binary: "/usr/local/bin/gemini",
    models: ["3.0 Pro", "3.0 Flash"],
    permMode: "ask",
    flags: "--telemetry off",
    appAccess: { github: true, postgres: false, playwright: true, linear: false },
    changelog: [
      { v: "0.13.0", date: "Jul 2, 2026", notes: ["Checkpoint and resume for long runs", "MCP stdio transport fixes"] },
      { v: "0.12.1", date: "Jun 18, 2026", notes: ["Fix sandbox path handling on WSL"] },
    ],
  },
  openclaw: {
    id: "openclaw",
    name: "OpenClaw",
    model: "Any provider",
    color: "#E25822",
    connection: "Gateway · local",
    initial: "W",
    version: "0.9.7",
    latest: "0.9.7",
    binary: "~/.openclaw/bin/openclaw",
    models: ["Route by task", "Claude Opus 4.5", "GPT-5.2"],
    permMode: "plan",
    flags: "--profile cockpit",
    appAccess: { github: true, postgres: false, playwright: false, linear: false },
    changelog: [{ v: "0.9.7", date: "Jun 26, 2026", notes: ["Provider failover honors Cockpit quotas"] }],
  },
  local: {
    id: "local",
    name: "Ollama (local)",
    model: "Qwen3-Coder 72B",
    color: "#8B8B8B",
    connection: "localhost:11434",
    initial: "L",
    version: "0.6.4",
    latest: "0.6.4",
    binary: "/opt/homebrew/bin/ollama",
    models: ["Qwen3-Coder 72B", "Llama 4 Scout", "DeepSeek-V4-lite"],
    permMode: "full",
    flags: "",
    appAccess: { github: false, postgres: false, playwright: true, linear: false },
    changelog: [{ v: "0.6.4", date: "Jun 20, 2026", notes: ["Flash attention default on Apple Silicon"] }],
  },
};

export const AGENT_IDS = Object.keys(AGENTS) as AgentId[];

export const PERM_MODES: { id: PermMode; label: string; desc: string }[] = [
  { id: "plan", label: "Plan", desc: "Proposes a plan first; every action needs approval." },
  { id: "ask", label: "Ask", desc: "Asks before edits and shell commands." },
  { id: "edit", label: "Edit", desc: "Edits files freely, asks before shell commands." },
  { id: "full", label: "Full", desc: "Full access — no approval prompts." },
];

export type Quota = { label: string; pct: number; used: string; max: string; resets: string };
export type ProviderAccount = {
  id: string;
  label: string;
  email: string;
  plan: string;
  status: "active" | "standby";
  quotas: Quota[];
};
export type ProviderFixture = {
  id: string;
  name: string;
  color: string;
  initial: string;
  kind: string;
  failover: { auto: boolean; threshold: number; returnToPrimary: boolean };
  accounts: ProviderAccount[];
  usage: { day: string; tok: number }[];
};

export const PROVIDERS: ProviderFixture[] = [
  {
    id: "anthropic",
    name: "Claude",
    color: "#D97757",
    initial: "C",
    kind: "Subscription · OAuth",
    failover: { auto: true, threshold: 95, returnToPrimary: true },
    accounts: [
      {
        id: "ac1",
        label: "Account 1",
        email: "alfin@meditap.id",
        plan: "Max 20×",
        status: "active",
        quotas: [
          { label: "Session (5h)", pct: 86, used: "4.3M", max: "5M tok", resets: "Resets in 3h 0m" },
          { label: "Weekly", pct: 77, used: "30.8M", max: "40M tok", resets: "Resets in 2d 15h" },
        ],
      },
      {
        id: "ac2",
        label: "Account 2",
        email: "nexus@meditap.id",
        plan: "Pro",
        status: "standby",
        quotas: [
          { label: "Session (5h)", pct: 12, used: "0.5M", max: "4M tok", resets: "Resets in 4h 40m" },
          { label: "Weekly", pct: 34, used: "3.1M", max: "9M tok", resets: "Resets in 5d 2h" },
        ],
      },
    ],
    usage: [
      { day: "Thu", tok: 6.2 },
      { day: "Fri", tok: 8.4 },
      { day: "Sat", tok: 2.1 },
      { day: "Sun", tok: 1.4 },
      { day: "Mon", tok: 7.8 },
      { day: "Tue", tok: 9.6 },
      { day: "Wed", tok: 5.2 },
      { day: "Today", tok: 4.3 },
    ],
  },
  {
    id: "openai",
    name: "OpenAI",
    color: "#0FA47F",
    initial: "O",
    kind: "Subscription · OAuth",
    failover: { auto: false, threshold: 95, returnToPrimary: true },
    accounts: [
      {
        id: "oc1",
        label: "Account 1",
        email: "alfin@meditap.id",
        plan: "Pro",
        status: "active",
        quotas: [{ label: "Daily", pct: 59, used: "89", max: "150 msgs", resets: "Resets in 9h 12m" }],
      },
    ],
    usage: [
      { day: "Thu", tok: 2.4 },
      { day: "Fri", tok: 3.1 },
      { day: "Sat", tok: 0.8 },
      { day: "Sun", tok: 0.4 },
      { day: "Mon", tok: 2.9 },
      { day: "Tue", tok: 1.8 },
      { day: "Wed", tok: 2.2 },
      { day: "Today", tok: 1.1 },
    ],
  },
  {
    id: "google",
    name: "Google",
    color: "#4285F4",
    initial: "G",
    kind: "API key",
    failover: { auto: false, threshold: 95, returnToPrimary: true },
    accounts: [
      {
        id: "gc1",
        label: "API key",
        email: "AIza…9f2c",
        plan: "Pay-as-you-go",
        status: "active",
        quotas: [{ label: "Daily requests", pct: 80, used: "48", max: "60 req", resets: "Resets in 14h 5m" }],
      },
    ],
    usage: [
      { day: "Thu", tok: 1.2 },
      { day: "Fri", tok: 1.9 },
      { day: "Sat", tok: 0.3 },
      { day: "Sun", tok: 0.2 },
      { day: "Mon", tok: 2.4 },
      { day: "Tue", tok: 1.5 },
      { day: "Wed", tok: 0.9 },
      { day: "Today", tok: 0.7 },
    ],
  },
  {
    id: "local",
    name: "Ollama",
    color: "#8B8B8B",
    initial: "L",
    kind: "Local runtime",
    failover: { auto: false, threshold: 95, returnToPrimary: false },
    accounts: [],
    usage: [],
  },
];

export const PROVIDER_CATALOG = [
  { id: "anthropic", name: "Anthropic", kind: "Claude · Max or API", color: "#D97757", initial: "A" },
  { id: "openai", name: "OpenAI", kind: "ChatGPT or platform API", color: "#0FA47F", initial: "O" },
  { id: "google", name: "Google", kind: "Gemini · AI Studio", color: "#4285F4", initial: "G" },
  { id: "xai", name: "xAI", kind: "Grok API", color: "#9CA3AF", initial: "X" },
  { id: "mistral", name: "Mistral", kind: "La Plateforme API", color: "#FA5111", initial: "M" },
  { id: "openrouter", name: "OpenRouter", kind: "Multi-provider router", color: "#6E56CF", initial: "R" },
];

const isMac = typeof navigator !== "undefined" && /Mac/i.test(navigator.userAgent);

export type WorkspaceFixture = { id: string; name: string; badge: string; detail: string; status: "connected" | "offline" };

export const WORKSPACES: WorkspaceFixture[] = [
  isMac
    ? { id: "local", name: "This Mac", badge: "MAC", detail: "macOS · Apple Silicon", status: "connected" }
    : { id: "local", name: "This PC", badge: "WIN", detail: "Windows 11 · x64", status: "connected" },
  { id: "wsl", name: "wsl · ubuntu", badge: "WSL", detail: "Ubuntu 22.04 · localhost", status: "connected" },
  { id: "vps", name: "prod-sg1", badge: "VPS", detail: "Hetzner · 128.140.42.7", status: "connected" },
  { id: "devbox", name: "devbox", badge: "SSH", detail: "ssh · 10.0.0.4:22", status: "offline" },
];

export type JobRun = {
  id: string;
  status: "success" | "failed" | "running";
  started: string;
  duration: string;
  add?: number;
  del?: number;
  note?: string;
  error?: string;
  log?: string[];
  session?: string;
};
export type JobFixture = {
  id: string;
  name: string;
  cron: string;
  mode: "natural" | "visual" | "cron";
  natural: string;
  project: string;
  branch: string;
  agent: AgentId;
  workspace: string;
  next: string;
  on: boolean;
  prompt: string;
  notify: { success: boolean; fail: boolean };
  history: JobRun[];
};

export const SCHEDULE_JOBS: JobFixture[] = [
  {
    id: "j1",
    name: "Nightly dependency audit",
    cron: "0 2 * * *",
    mode: "natural",
    natural: "every day at 2am",
    project: "sentinel",
    branch: "main",
    agent: "claude",
    workspace: "vps",
    next: "Tonight 02:00",
    on: true,
    prompt:
      "Run npm audit. Patch every non-breaking advisory, run the test suite, and open a PR only if something changed. Summarize any skipped majors.",
    notify: { success: false, fail: true },
    history: [
      { id: "r128", status: "success", started: "Today 02:00", duration: "4m 12s", add: 12, del: 4, session: "srun1" },
      {
        id: "r127",
        status: "success",
        started: "Yesterday 02:00",
        duration: "3m 58s",
        add: 0,
        del: 0,
        note: "No advisories — nothing to change",
      },
      {
        id: "r126",
        status: "failed",
        started: "Jul 1, 02:00",
        duration: "1m 04s",
        error: "npm ERR! code E401 — registry token expired on prod-sg1",
        log: [
          "$ npm audit --json",
          "npm ERR! code E401",
          "npm ERR! Incorrect or missing password.",
          "npm ERR! A complete log of this run can be found in ~/.npm/_logs/2026-07-01.log",
          "exit status 1",
        ],
      },
      { id: "r125", status: "success", started: "Jun 30, 02:00", duration: "5m 40s", add: 31, del: 9 },
      { id: "r124", status: "success", started: "Jun 29, 02:00", duration: "2m 51s", add: 0, del: 0, note: "No advisories" },
    ],
  },
  {
    id: "j2",
    name: "Sync design tokens",
    cron: "0 */6 * * *",
    mode: "cron",
    natural: "",
    project: "web",
    branch: "main",
    agent: "codex",
    workspace: "local",
    next: "in 3h 12m",
    on: true,
    prompt:
      "Pull the latest tokens.json from the design repo, regenerate tailwind.config.ts and the CSS variables, and commit only if the output differs.",
    notify: { success: true, fail: true },
    history: [
      { id: "r89", status: "success", started: "Today 12:00", duration: "1m 44s", add: 6, del: 6 },
      { id: "r88", status: "success", started: "Today 06:00", duration: "1m 39s", add: 0, del: 0, note: "Tokens unchanged" },
      { id: "r87", status: "success", started: "Yesterday 18:00", duration: "1m 51s", add: 2, del: 2 },
    ],
  },
  {
    id: "j3",
    name: "Weekly changelog draft",
    cron: "0 9 * * 1",
    mode: "natural",
    natural: "every monday at 9am",
    project: "sentinel",
    branch: "main",
    agent: "gemini",
    workspace: "wsl",
    next: "Mon 09:00",
    on: false,
    prompt: "Collect merged PRs from the past week and draft CHANGELOG.md entries grouped by area. Open a draft PR for review.",
    notify: { success: false, fail: false },
    history: [
      { id: "r12", status: "success", started: "Mon Jun 29, 09:00", duration: "6m 02s", add: 48, del: 0 },
      {
        id: "r11",
        status: "failed",
        started: "Mon Jun 22, 09:01",
        duration: "0m 40s",
        error: "Gateway wsl · ubuntu was offline — run skipped after 3 retries",
        log: [
          "[scheduler] connect wsl: timeout after 30s",
          "[scheduler] retry 1/3 …",
          "[scheduler] retry 2/3 …",
          "[scheduler] retry 3/3 …",
          "[scheduler] giving up — gateway offline",
        ],
      },
    ],
  },
];

export type AppTool = { name: string; desc: string; perm: "allow" | "ask" | "deny" };
export type AppAuth =
  | { type: "oauth"; account: string; status: "connected" | "expired"; expires: string; lastRefresh: string }
  | { type: "env"; env: string; account: string; status: "connected" }
  | { type: "none" };
export type AppFixture = {
  id: string;
  name: string;
  kind: string;
  initial: string;
  color: string;
  desc: string;
  scope: "global" | "select";
  scopeWs: Record<string, boolean>;
  status: "connected" | "error";
  version: string;
  publisher: string;
  auth: AppAuth;
  tools: AppTool[];
  agentAccess: Record<AgentId, boolean>;
};

export const APPS: AppFixture[] = [
  {
    id: "github",
    name: "GitHub",
    kind: "MCP server",
    initial: "G",
    color: "#24292F",
    desc: "Pull requests, issues, and repo search for the active project.",
    scope: "global",
    scopeWs: {},
    status: "connected",
    version: "1.4.2",
    publisher: "github · verified",
    auth: { type: "oauth", account: "alfin-dev", status: "connected", expires: "Aug 2, 2026", lastRefresh: "2h ago" },
    tools: [
      { name: "search_code", desc: "Search code across accessible repos", perm: "allow" },
      { name: "list_issues", desc: "List and filter issues", perm: "allow" },
      { name: "create_pr", desc: "Open a pull request from the working branch", perm: "ask" },
      { name: "create_issue", desc: "File a new issue", perm: "ask" },
      { name: "merge_pr", desc: "Merge a pull request", perm: "deny" },
    ],
    agentAccess: { claude: true, codex: true, gemini: true, openclaw: false, local: false },
  },
  {
    id: "postgres",
    name: "Postgres",
    kind: "MCP server",
    initial: "P",
    color: "#336791",
    desc: "Read-only queries against the staging and prod databases.",
    scope: "select",
    scopeWs: { vps: true },
    status: "connected",
    version: "0.9.1",
    publisher: "community",
    auth: { type: "env", env: "DATABASE_URL", account: "readonly@prod-sg1", status: "connected" },
    tools: [
      { name: "query", desc: "Run a read-only SQL query", perm: "allow" },
      { name: "explain", desc: "EXPLAIN ANALYZE a statement", perm: "allow" },
      { name: "list_schemas", desc: "Inspect schemas and tables", perm: "allow" },
    ],
    agentAccess: { claude: true, codex: true, gemini: false, openclaw: false, local: false },
  },
  {
    id: "playwright",
    name: "Playwright",
    kind: "Tool",
    initial: "Pw",
    color: "#2EAD33",
    desc: "Headless browser for end-to-end tests and screenshots.",
    scope: "select",
    scopeWs: { local: true },
    status: "connected",
    version: "1.52.0",
    publisher: "microsoft · verified",
    auth: { type: "none" },
    tools: [
      { name: "navigate", desc: "Open a URL in the headless browser", perm: "allow" },
      { name: "screenshot", desc: "Capture page or element screenshots", perm: "allow" },
      { name: "click", desc: "Interact with page elements", perm: "allow" },
      { name: "evaluate", desc: "Run JavaScript in the page context", perm: "ask" },
    ],
    agentAccess: { claude: true, codex: true, gemini: true, openclaw: true, local: true },
  },
  {
    id: "linear",
    name: "Linear",
    kind: "MCP server",
    initial: "L",
    color: "#5E6AD2",
    desc: "Create and update issues from agent runs.",
    scope: "global",
    scopeWs: {},
    status: "error",
    version: "2.1.0",
    publisher: "linear · verified",
    auth: { type: "oauth", account: "alfin@meditap.id", status: "expired", expires: "Jun 28, 2026", lastRefresh: "6d ago" },
    tools: [
      { name: "search_issues", desc: "Search issues across teams", perm: "allow" },
      { name: "create_issue", desc: "Create an issue", perm: "ask" },
      { name: "update_issue", desc: "Edit status, assignee or labels", perm: "ask" },
    ],
    agentAccess: { claude: true, codex: false, gemini: false, openclaw: false, local: false },
  },
];

export type RegistryEntry = {
  id: string;
  name: string;
  initial: string;
  color: string;
  cat: string;
  publisher: string;
  verified: boolean;
  installs: string;
  desc: string;
};

// The registry list in the fetched v3 spec was cut off after Sentry/Notion/
// Slack/Supabase; the remaining entries follow the same pattern.
export const REGISTRY: RegistryEntry[] = [
  {
    id: "sentry",
    name: "Sentry",
    initial: "Se",
    color: "#6C5FC7",
    cat: "Monitoring",
    publisher: "getsentry",
    verified: true,
    installs: "61k",
    desc: "Query issues, stack traces and release health from agent runs.",
  },
  {
    id: "notion",
    name: "Notion",
    initial: "N",
    color: "#8B8B8B",
    cat: "Docs",
    publisher: "makenotion",
    verified: true,
    installs: "44k",
    desc: "Read specs and write run reports to shared pages.",
  },
  {
    id: "slack",
    name: "Slack",
    initial: "Sl",
    color: "#E01E5A",
    cat: "Communication",
    publisher: "slack",
    verified: true,
    installs: "54k",
    desc: "Post run summaries and ask for approvals in channels.",
  },
  {
    id: "supabase",
    name: "Supabase",
    initial: "Su",
    color: "#3ECF8E",
    cat: "Database",
    publisher: "supabase",
    verified: true,
    installs: "38k",
    desc: "Query and migrate hosted Postgres, storage and auth.",
  },
  {
    id: "figma",
    name: "Figma",
    initial: "F",
    color: "#A259FF",
    cat: "Design",
    publisher: "figma",
    verified: true,
    installs: "29k",
    desc: "Read frames, tokens and comments from design files.",
  },
  {
    id: "stripe",
    name: "Stripe",
    initial: "St",
    color: "#635BFF",
    cat: "Payments",
    publisher: "stripe",
    verified: true,
    installs: "22k",
    desc: "Inspect payments, customers and webhook events safely.",
  },
  {
    id: "vercel",
    name: "Vercel",
    initial: "V",
    color: "#9CA3AF",
    cat: "Deploy",
    publisher: "vercel",
    verified: true,
    installs: "33k",
    desc: "Trigger deploys and read build logs for the active project.",
  },
  {
    id: "grafana",
    name: "Grafana",
    initial: "Gr",
    color: "#F46800",
    cat: "Monitoring",
    publisher: "grafana",
    verified: false,
    installs: "12k",
    desc: "Query dashboards and alerts from agent runs.",
  },
];

export const REGISTRY_CATS = ["All", "Monitoring", "Docs", "Communication", "Database", "Design", "Payments", "Deploy"];

// ---- Session-view preview content (terminal / review / file tabs) ----------

export const TERM_LINES: { text: string; color: string }[] = [
  { text: "PS C:\\dev\\sentinel> npm test", color: "var(--foreground)" },
  { text: "", color: "var(--muted-foreground)" },
  { text: "> sentinel@2.4.1 test", color: "var(--muted-foreground)" },
  { text: "> vitest run", color: "var(--muted-foreground)" },
  { text: "", color: "var(--muted-foreground)" },
  { text: " ✓ src/auth/retry.test.ts (12 tests) 842ms", color: "#22C55E" },
  { text: " ✓ src/auth/session.test.ts (9 tests) 310ms", color: "#22C55E" },
  { text: " ✓ src/api/webhooks.test.ts (21 tests) 1.2s", color: "#22C55E" },
  { text: "", color: "var(--muted-foreground)" },
  { text: " Test Files  3 passed (3)", color: "var(--foreground)" },
  { text: "      Tests  42 passed (42)", color: "var(--foreground)" },
  { text: "   Duration  2.41s", color: "var(--muted-foreground)" },
  { text: "", color: "var(--muted-foreground)" },
  { text: "PS C:\\dev\\sentinel> _", color: "var(--foreground)" },
];

export type DiffLine = ["hunk" | "ctx" | "add" | "del", number | "", string];

export const DIFF_CD: DiffLine[] = [
  ["hunk", "", "28 unmodified lines"],
  ["ctx", 29, ""],
  ["ctx", 30, "      steps:"],
  ["ctx", 31, "        - name: Checkout"],
  ["del", 32, "          uses: actions/checkout@v4"],
  ["add", 32, "          uses: actions/checkout@v6"],
  ["ctx", 33, "          with:"],
  ["ctx", 34, "            fetch-depth: 0"],
  // biome-ignore lint/suspicious/noTemplateCurlyInString: GitHub Actions syntax inside the diff preview
  ["ctx", 35, "            token: ${{ secrets.GITHUB_TOKEN }}"],
  ["ctx", 36, ""],
  ["ctx", 37, "        - name: Set up Node.js"],
  ["del", 38, "          uses: actions/setup-node@v4"],
  ["add", 38, "          uses: actions/setup-node@v6"],
  ["ctx", 39, "          with:"],
  ["del", 40, "            node-version: 20"],
  ["add", 40, "            node-version: 22"],
  ["ctx", 41, ""],
  ["hunk", "", "9 unmodified lines"],
  ["ctx", 51, "              @semantic-release/git@10"],
  ["ctx", 52, ""],
  ["ctx", 53, "        - name: Run Semantic Release"],
  ["del", 54, "          id: semantic"],
  ["ctx", 54, "          env:"],
  // biome-ignore lint/suspicious/noTemplateCurlyInString: GitHub Actions syntax inside the diff preview
  ["ctx", 55, "            GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}"],
  ["ctx", 56, "            GIT_AUTHOR_NAME: github-actions[bot]"],
  ["ctx", 57, "          run: npx semantic-release"],
  ["ctx", 58, ""],
  ["add", 59, "        # semantic-release does not write to $GITHUB_OUTPUT;"],
  ["add", 60, "        # detect the new tag by comparing git tags before/after"],
  ["add", 61, "        - name: Detect new release"],
  ["add", 62, "          id: semantic"],
  ["add", 63, "          run: |"],
  ["add", 64, "            git fetch --tags"],
  ["add", 65, "            NEW_TAG=$(git tag --sort=-version:refname | head -n1)"],
  ["add", 66, '            if [ -n "$NEW_TAG" ]; then'],
  ["add", 67, '              echo "new_release_published=true" >> "$GITHUB_OUTPUT"'],
  ["add", 68, "            fi"],
];

export type ReviewFile = { dir: string; name: string; add: number; del: number; lines: DiffLine[] };

export const REVIEW_FILES: ReviewFile[] = [
  { dir: ".github/workflows/", name: "cd.yml", add: 20, del: 6, lines: DIFF_CD },
  { dir: ".github/workflows/", name: "ci.yml", add: 8, del: 3, lines: DIFF_CD.slice(0, 12) },
  { dir: "", name: "package.json", add: 2, del: 2, lines: DIFF_CD.slice(1, 9) },
];

export const CODE_LINES: [string, string][] = [
  ["name: CD", "var(--foreground)"],
  ["", "var(--muted-foreground)"],
  ["on:", "var(--foreground)"],
  ["  push:", "var(--foreground)"],
  ["    branches: [main]", "var(--muted-foreground)"],
  ["", "var(--muted-foreground)"],
  ["jobs:", "var(--foreground)"],
  ["  release:", "var(--foreground)"],
  ["    runs-on: ubuntu-latest", "var(--muted-foreground)"],
  ["    steps:", "var(--foreground)"],
  ["      - name: Checkout", "var(--muted-foreground)"],
  ["        uses: actions/checkout@v6", "var(--muted-foreground)"],
  ["      - name: Set up Node.js", "var(--muted-foreground)"],
  ["        uses: actions/setup-node@v6", "var(--muted-foreground)"],
  ["        with:", "var(--muted-foreground)"],
  ["          node-version: 22", "var(--muted-foreground)"],
];

export const TREE_ITEMS: { name: string; depth: number; dir?: boolean; open?: boolean; sel?: boolean }[] = [
  { name: ".github", depth: 0, dir: true, open: true },
  { name: "workflows", depth: 1, dir: true, open: true },
  { name: "cd.yml", depth: 2, sel: true },
  { name: "ci.yml", depth: 2 },
  { name: "src", depth: 0, dir: true },
  { name: "tests", depth: 0, dir: true },
  { name: "package.json", depth: 0 },
  { name: "README.md", depth: 0 },
];

export const HOME_SUGGESTIONS = ["Fix the failing e2e suite", "Add rate limiting to the API", "Upgrade to React 20"];

// Quota bars tint by pressure: calm → amber → red as the reset nears zero headroom.
export function quotaColor(pct: number): string {
  if (pct >= 90) return "#EF4444";
  if (pct >= 75) return "#F59E0B";
  return "#22C55E";
}
