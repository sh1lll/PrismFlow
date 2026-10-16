import { invoke } from "@tauri-apps/api/core";

export type Protocol = "OpenAiChat" | "OpenAiResponses" | "AnthropicMessages";

export interface Provider {
  id: string;
  display_name: string;
  base_url: string;
  protocols: Protocol[];
  model_list_path: string | null;
  auth_check_path: string | null;
  docs_url: string;
  keys_url: string;
  local: boolean;
  custom: boolean;
  requires_key: boolean;
  key_optional: boolean;
  verified_at: string;
  credential_source: "keychain" | "gh_cli" | "codex_login" | "gcloud" | "copilot_cli";
  signin_available: boolean;
  has_credential: boolean;
  key_count: number;
  address: string;
  example_model: string;
  codex_direct: boolean;
  /** How each app reaches this provider; null = unsupported. */
  routes: Record<AppId, Route | null>;
}

export interface UrlCheck {
  normalized: string;
  host: string;
  loopback: boolean;
  resolves_private: boolean;
  warnings: string[];
}

export type AppId = "codex_cli" | "codex_cli_separate" | "claude_code_separate" | "vscode_claude_shared" | "vscode_separate" | "zed_separate" | "codex_desktop" | "codex_desktop_separate" | "claude_code" | "claude_desktop" | "claude_desktop_separate" | "jetbrains" | "vscode_claude" | "vscode_chat" | "aider" | "cline" | "pi" | "hermes" | "opencode" | "dsh" | "grok" | "muse" | "qwen" | "goose" | "crush" | "droid" | "continue_cli" | "copilot_cli" | "roo_code" | "kilo_code" | "zed" | "continue_ide" | "aider_separate" | "cline_shared" | "pi_shared" | "opencode_separate" | "dsh_shared" | "grok_shared" | "muse_separate" | "qwen_separate" | "goose_separate" | "crush_shared" | "droid_separate" | "continue_cli_shared" | "copilot_cli_separate";
export const APP_IDS: AppId[] = ["codex_cli", "codex_cli_separate", "codex_desktop", "codex_desktop_separate", "claude_code", "claude_code_separate", "claude_desktop", "claude_desktop_separate", "jetbrains", "vscode_claude", "vscode_claude_shared", "vscode_chat", "vscode_separate", "aider", "cline", "pi", "hermes", "roo_code", "kilo_code", "zed", "zed_separate", "continue_ide", "opencode", "dsh", "grok", "muse", "qwen", "goose", "crush", "droid", "continue_cli", "copilot_cli", "aider_separate", "cline_shared", "pi_shared", "opencode_separate", "dsh_shared", "grok_shared", "muse_separate", "qwen_separate", "goose_separate", "crush_shared", "droid_separate", "continue_cli_shared", "copilot_cli_separate"];
export type Route = "direct" | "gateway";

export const APP_NAME: Record<string, string> = {
  codex_cli: "Codex CLI", codex_cli_separate: "Codex CLI (separate)", claude_code_separate: "Claude Code CLI (separate)", vscode_claude_shared: "Claude Code for VS Code (shared)", vscode_separate: "VS Code (separate)", codex_desktop: "Codex app & VS Code", codex_desktop_separate: "Codex app (separate)", claude_code: "Claude Code CLI", claude_desktop: "Claude Desktop", claude_desktop_separate: "Claude Desktop (separate)", jetbrains: "JetBrains IDEs", vscode_claude: "Claude Code for VS Code (separate)", vscode_chat: "VS Code Chat", aider: "Aider", cline: "Cline CLI", pi: "Pi", hermes: "Hermes Agent", playground: "Try a model", opencode: "OpenCode", dsh: "DeepSeek Harness", grok: "Grok Build", muse: "Muse Code", qwen: "Qwen Code", goose: "Goose", crush: "Crush", droid: "Factory Droid", continue_cli: "Continue CLI", copilot_cli: "GitHub Copilot CLI", aider_separate: "Aider (separate)", cline_shared: "Cline CLI (shared)", pi_shared: "Pi (shared)", opencode_separate: "OpenCode (separate)", dsh_shared: "DeepSeek Harness (shared)", grok_shared: "Grok Build (shared)", muse_separate: "Muse Code (separate)", qwen_separate: "Qwen Code (separate)", goose_separate: "Goose (separate)", crush_shared: "Crush (shared)", droid_separate: "Factory Droid (separate)", continue_cli_shared: "Continue CLI (shared)", copilot_cli_separate: "GitHub Copilot CLI (separate)", roo_code: "Roo Code", kilo_code: "Kilo Code", zed: "Zed", zed_separate: "Zed (separate)", continue_ide: "Continue (VS Code / JetBrains)",
};

export const ROUTE_LABEL: Record<Route, string> = {
  direct: "Direct",
  gateway: "Through PrismFlow",
};

export type GatewayPhase = "stopped" | "starting" | "ready" | "restarting" | "failed";

export interface GatewayStatus {
  phase: GatewayPhase;
  url: string;
  port: number;
  engine: string;
  node: string | null;
  detail: string | null;
  restarts: number;
  requests: number;
  rejected: number;
  upstream_errors: number;
  shortcuts: number;
  connections: number;
  upstreams: string[];
}

export interface ModelInfo {
  id: string;
  name: string | null;
  context_length: number | null;
  reports_tools: boolean | null;
  price_in: number | null;
  price_out: number | null;
  coding: boolean;
}

/** "$0.15 / $0.60 per 1M" style text, or "" when unknown. */
export function priceText(m: ModelInfo): string {
  if (m.price_in == null || m.price_out == null) return "";
  if (m.price_in === 0 && m.price_out === 0) return "free";
  const f = (n: number) => (n < 0.1 ? n.toFixed(3) : n.toFixed(2));
  return `$${f(m.price_in)} in · $${f(m.price_out)} out per 1M`;
}

export interface AppDetection {
  app_id: string;
  display_name: string;
  installed: boolean;
  path: string | null;
  version: string | null;
  version_verified: boolean;
  notes: string[];
}

export type ConnectionStatus =
  | "NotConfigured" | "NeedsCredentials" | "ConfigurationInvalid" | "AuthenticationFailed"
  | "ProviderUnavailable" | "ModelUnavailable" | "PartiallyCompatible" | "ConnectedAndVerified"
  | "Disconnected" | "RequiresAttention" | "Unverified";

export type StageOutcome = "Pass" | "Warn" | "Fail" | "Skipped";

/** How an app starts with a connection: the command to type ("" when there is none) and the button PrismFlow offers. */
/** What a skipped test reports (TEST_SKIPPED in src-tauri/src/lib.rs). */
export const TEST_SKIPPED = "Test skipped.";

export interface LaunchInfo { line: string; opens: "Terminal" | "App" | "Nothing" }

export interface StageResult { stage: number; name: string; outcome: StageOutcome; detail: string }

/** Plain names for the test steps. */
export const STEP_NAME: Record<string, string> = {
  Configuration: "Setup",
  Credential: "Key",
  Endpoint: "Provider reachable",
  Model: "Model",
  Gateway: "PrismFlow service",
  "End-to-end": "Reply",
  "Tool calling": "Tools",
  "In Claude Desktop": "In Claude Desktop",
};

export interface VerificationReport {
  started_at: number;
  finished_at: number;
  stages: StageResult[];
  status: ConnectionStatus;
}

export interface BotPlatform { enabled: boolean; allowed_users: string[]; allowed_channels: string[] }
export interface BotSettings {
  telegram: BotPlatform;
  discord: BotPlatform;
  folder: string;
  connection_id: string;
  allow_edits: boolean;
  messages_per_minute: number;
  proxy: string;
  voice: { backend: "off" | "local" | "provider"; model_path: string; provider_id: string; model: string };
}
export interface BotPlatformStatus { running: boolean; detail: string; messages: number }
export interface BotsView {
  settings: BotSettings;
  status: { telegram: BotPlatformStatus; discord: BotPlatformStatus };
  has_telegram_token: boolean;
  has_discord_token: boolean;
  whisper_installed: boolean;
  ffmpeg_installed: boolean;
}

export interface Connection {
  id: string;
  provider_id: string;
  app_id: string;
  model: string;
  profile: string;
  route: Route;
  managed_file: string;
  created_at: number;
  status: ConnectionStatus;
  last_report: VerificationReport | null;
  routing: Routing;
}

export type Tier = "default" | "fable" | "opus" | "sonnet" | "haiku" | "subagent";
export type Effort = "inherit" | "off" | "from_client" | "low" | "medium" | "high" | "xhigh" | "max";
export interface ModelRef { provider_id: string; model: string }
export interface Routing {
  tiers: Partial<Record<Tier, ModelRef>>;
  reasoning: Effort;
  tier_reasoning: Partial<Record<Tier, Effort>>;
  fallbacks: ModelRef[];
  fallback_consent: boolean;
  output_filter?: boolean;
}
export interface WebToolsSettings { enabled: boolean; backend: "none" | "searxng" | "brave"; searxng_url: string; allow_private: boolean }
export interface RuntimeSettings {
  rate_limit_per_minute: number;
  max_concurrency_per_provider: number;
  upstream_retries: number;
  shortcuts: boolean;
  web_tools: WebToolsSettings;
  budgets: Record<string, number>;
  ollama_context: number;
}

export interface FoundKey { provider_id: string; provider_name: string; variable: string; source: string; masked: string; already_saved: boolean }
export interface ProfileEntry { app_id: string; provider_id: string; model: string }
export interface Profiles { active: string | null; list: Record<string, ProfileEntry[]> }

export interface McpServer { name: string; command: string; args: string[]; env: Record<string, string>; url: string }
export interface Skill { name: string; apps: string[]; description: string }
export interface ToolsView { servers: McpServer[]; applied: Record<string, string[]>; targets: [string, string, boolean][]; rules: string; skills: Skill[] }
export interface RestoreReport { restored: string[]; problems: string[] }

export interface UsageTotals { requests: number; errors: number; input_tokens: number; output_tokens: number; cost: number; unpriced: number }
export interface UsageRequest {
  ts: number; connection: string; provider: string; model: string; path: string; status: number; ms: number;
  input_tokens: number; output_tokens: number; cost: number | null; error: string | null;
}
export interface UsageView {
  days: Record<string, Record<string, UsageTotals>>;
  month: Record<string, { spent: number; limit: number | null }>;
  requests: UsageRequest[];
  today: string;
}

export interface UninstallItem { what: string; outcome: "planned" | "done" | "failed"; detail: string }
export interface UninstallReport { dry_run: boolean; complete: boolean; items: UninstallItem[] }

export interface CompanionDevice { id: string; name: string; allow_actions: boolean; created_at: number; last_seen: number | null }
export interface CompanionView {
  enabled: boolean;
  url: string | null;
  fingerprint: string | null;
  settings: { address: string; port: number; keep_on: boolean };
  devices: CompanionDevice[];
  suggested_address: string | null;
}
export interface PairingView { uri: string; qr_svg: string; code: string; url: string; fingerprint: string; expires_in_secs: number }

export type Sandbox = "read_only" | "workspace_write";
export interface SessionTurn { connection_id: string; model: string; prompt: string; status: "running" | "completed" | "failed" | "stopped"; started_at: number; first_event: number }
export interface CodeSession { id: string; title: string; folder: string; sandbox: Sandbox; thread_id: string | null; created_at: number; turns: SessionTurn[] }
export interface EventsPage { session: CodeSession; events: any[]; next: number }

export interface ConnectPreview {
  app_id: AppId;
  route: Route;
  file: string;
  before: string;
  after: string;
  summary: string[];
  warnings: string[];
  launch_command: string;
}

export interface DiagEvent { ts: number; level: "info" | "warn" | "error"; code: string; message: string }

export const api = {
  providers: () => invoke<Provider[]>("list_providers"),
  setCredential: (providerId: string, secret: string) => invoke<void>("set_credential", { providerId, secret }),
  deleteCredential: (providerId: string) => invoke<void>("delete_credential", { providerId }),
  discoverModels: (providerId: string) => invoke<ModelInfo[]>("discover_models", { providerId }),
  applications: () => invoke<AppDetection[]>("list_applications"),
  preview: (appId: AppId, providerId: string, model: string) => invoke<ConnectPreview>("preview_connection", { appId, providerId, model }),
  connect: (appId: AppId, providerId: string, model: string) => invoke<Connection>("connect", { appId, providerId, model }),
  verify: (id: string) => invoke<VerificationReport>("verify_connection", { id }),
  launchInfo: (id: string) => invoke<LaunchInfo>("launch_info", { id }),
  /** Stops a running test; its `verify` call then fails with TEST_SKIPPED. */
  cancelVerify: (id: string) => invoke<void>("cancel_verify", { id }),
  verifyProgress: (id: string) => invoke<StageResult[] | null>("verify_progress", { id }),
  disconnect: (id: string) => invoke<string | null>("disconnect", { id }),
  connections: () => invoke<Connection[]>("list_connections"),
  checkCustomUrl: (url: string) => invoke<UrlCheck>("check_custom_url", { url }),
  addCustomProvider: (name: string, url: string, protocol: Protocol, requiresKey: boolean, consent: boolean, gcloud = false) =>
    invoke<Provider>("add_custom_provider", { name, url, protocol, requiresKey, consent, gcloud }),
  removeCustomProvider: (id: string) => invoke<void>("remove_custom_provider", { id }),
  setRouting: (id: string, routing: Routing) => invoke<Connection>("set_routing", { id, routing }),
  getSettings: () => invoke<RuntimeSettings>("get_settings"),
  setSettings: (settings: RuntimeSettings) => invoke<RuntimeSettings>("set_settings", { settings }),
  checkUpdate: () => invoke<{ current: string; available: string | null; notes: string | null }>("check_update"),
  installUpdate: () => invoke<void>("install_update"),
  guidedSetup: (id: string) => invoke<{ gateway_url: string; auth_scheme: string; token: string; model: string; steps: string[] }>("guided_setup", { id }),
  companionState: () => invoke<CompanionView>("companion_state"),
  keychainWaiting: () => invoke<boolean>("keychain_waiting"),
  editorGuide: (id: string) => invoke<string[]>("editor_guide", { id }),
  setProviderUrl: (providerId: string, url: string) => invoke<string>("set_provider_url", { providerId, url }),
  ollama: () => invoke<any>("ollama_view"),
  ollamaPull: (name: string) => invoke<void>("ollama_pull", { name }),
  ollamaDelete: (name: string) => invoke<void>("ollama_delete", { name }),
  tools: () => invoke<ToolsView>("tools_get"),
  mcpApply: (servers: McpServer[], apps: string[]) => invoke<string[]>("mcp_apply", { servers, apps }),
  rulesApply: (rules: string, project: string | null) => invoke<string[]>("rules_apply", { rules, project }),
  skillShare: (name: string) => invoke<string[]>("skill_share", { name }),
  backupExport: () => invoke<string>("backup_export"),
  backupRestore: (path: string) => invoke<RestoreReport>("backup_restore", { path }),
  addKey: (providerId: string, secret: string) => invoke<number>("keys_add", { providerId, secret }),
  removeExtraKeys: (providerId: string) => invoke<void>("keys_remove_extra", { providerId }),
  scanKeys: () => invoke<FoundKey[]>("keys_scan"),
  importKeys: (chosen: [string, string, string][]) => invoke<number>("keys_import", { chosen }),
  profiles: () => invoke<Profiles>("profiles_get"),
  saveProfile: (name: string) => invoke<Profiles>("profile_save", { name }),
  deleteProfile: (name: string) => invoke<Profiles>("profile_delete", { name }),
  switchProfile: (name: string) => invoke<Profiles>("profile_switch", { name }),
  tryChat: (providerId: string, model: string, messages: { role: string; content: string }[]) => invoke<string>("try_chat", { providerId, model, messages }),
  usage: (since: string, limit: number) => invoke<UsageView>("usage_view", { since, limit }),
  autostartGet: () => invoke<boolean>("autostart_get"),
  autostartSet: (on: boolean) => invoke<void>("autostart_set", { on }),
  bots: () => invoke<BotsView>("bots_get"),
  saveBots: (settings: BotSettings) => invoke<void>("bots_save", { settings }),
  setBotToken: (platform: "telegram" | "discord", token: string) => invoke<void>("bots_set_token", { platform, token }),
  stopBots: () => invoke<void>("bots_stop"),
  openrouterSignIn: () => invoke<void>("openrouter_sign_in"),
  companionEnable: (address: string, port: number, keepOn: boolean) => invoke<CompanionView>("companion_enable", { address, port, keepOn }),
  companionDisable: () => invoke<CompanionView>("companion_disable"),
  companionPair: () => invoke<PairingView>("companion_pair"),
  companionRevoke: (id: string) => invoke<CompanionView>("companion_revoke", { id }),
  companionAllowActions: (id: string, allow: boolean) => invoke<CompanionView>("companion_allow_actions", { id, allow }),
  uninstall: (dryRun: boolean) => invoke<UninstallReport>("uninstall", { dryRun }),
  setSigninConsent: (providerId: string, consent: boolean) => invoke<void>("set_signin_consent", { providerId, consent }),
  setWebSearchKey: (key: string) => invoke<void>("set_web_search_key", { key }),
  hasWebSearchKey: () => invoke<boolean>("has_web_search_key"),
  sessionList: () => invoke<CodeSession[]>("session_list"),
  sessionCreate: (folder: string, sandbox: Sandbox, title: string) => invoke<CodeSession>("session_create", { folder, sandbox, title }),
  sessionSend: (id: string, connectionId: string, prompt: string) => invoke<void>("session_send", { id, connectionId, prompt }),
  sessionEvents: (id: string, since: number) => invoke<EventsPage>("session_events", { id, since }),
  sessionStop: (id: string) => invoke<void>("session_stop", { id }),
  sessionDelete: (id: string) => invoke<void>("session_delete", { id }),
  runningSessions: (id: string) => invoke<number[]>("running_sessions", { id }),
  launch: (id: string) => invoke<void>("launch_connection", { id }),
  diagnostics: () => invoke<DiagEvent[]>("get_diagnostics"),
  exportDiagnostics: () => invoke<string>("export_diagnostics"),
  gatewayStatus: () => invoke<GatewayStatus>("gateway_status"),
  restartGateway: () => invoke<GatewayStatus>("restart_gateway"),
};

/** A provider's display name, falling back to its id. */
export function providerName(providers: Provider[], id: string): string {
  return providers.find((p) => p.id === id)?.display_name ?? id;
}

/** Apps that come in two ways: with your usual chats and settings, or a space of their own. */
export const MODE_PAIRS: { name: string; shared: AppId; separate: AppId; sharedText?: string; separateText?: string }[] = [
  { name: "Claude Desktop", shared: "claude_desktop", separate: "claude_desktop_separate",
    sharedText: "your Claude Desktop switches to PrismFlow (not your Claude account) until you disconnect",
    separateText: "a second Claude Desktop just for PrismFlow; your usual one stays signed in" },
  { name: "Codex CLI", shared: "codex_cli", separate: "codex_cli_separate" },
  { name: "Codex app", shared: "codex_desktop", separate: "codex_desktop_separate" },
  { name: "Claude Code", shared: "claude_code", separate: "claude_code_separate" },
  { name: "Claude Code for VS Code", shared: "vscode_claude_shared", separate: "vscode_claude" },
  { name: "VS Code", shared: "vscode_chat", separate: "vscode_separate" },
  { name: "Zed", shared: "zed", separate: "zed_separate" },
  { name: "Aider", shared: "aider", separate: "aider_separate" },
  { name: "Cline CLI", shared: "cline_shared", separate: "cline" },
  { name: "Pi", shared: "pi_shared", separate: "pi" },
  { name: "OpenCode", shared: "opencode", separate: "opencode_separate" },
  { name: "DeepSeek Harness", shared: "dsh_shared", separate: "dsh" },
  { name: "Grok Build", shared: "grok_shared", separate: "grok" },
  { name: "Muse Code", shared: "muse", separate: "muse_separate" },
  { name: "Qwen Code", shared: "qwen", separate: "qwen_separate" },
  { name: "Goose", shared: "goose", separate: "goose_separate" },
  { name: "Crush", shared: "crush_shared", separate: "crush" },
  { name: "Factory Droid", shared: "droid", separate: "droid_separate" },
  { name: "Continue CLI", shared: "continue_cli_shared", separate: "continue_cli" },
  { name: "GitHub Copilot CLI", shared: "copilot_cli", separate: "copilot_cli_separate" },
];

export function modePair(id: string) {
  return MODE_PAIRS.find((p) => p.shared === id || p.separate === id);
}

export const STATUS_LABEL: Record<ConnectionStatus, string> = {
  NotConfigured: "Not set up",
  NeedsCredentials: "Needs a key",
  ConfigurationInvalid: "Setup is broken",
  AuthenticationFailed: "Key rejected",
  ProviderUnavailable: "Can't reach the provider",
  ModelUnavailable: "Model not available",
  PartiallyCompatible: "Chat works, tools don't",
  ConnectedAndVerified: "Working",
  Disconnected: "Disconnected",
  RequiresAttention: "Needs attention",
  Unverified: "Not tested yet",
};

export function statusTone(s: ConnectionStatus): "ok" | "warn" | "err" | "muted" {
  if (s === "ConnectedAndVerified") return "ok";
  if (s === "PartiallyCompatible" || s === "Unverified") return "warn";
  if (s === "Disconnected" || s === "NotConfigured") return "muted";
  return "err";
}

export function errText(e: unknown): string {
  return typeof e === "string" ? e : e instanceof Error ? e.message : JSON.stringify(e);
}

export function when(ts: number): string {
  return new Date(ts * 1000).toLocaleString();
}
