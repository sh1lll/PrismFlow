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
  has_credential: boolean;
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

export type AppId = "codex_cli" | "codex_desktop" | "claude_code" | "claude_desktop" | "aider" | "cline" | "pi" | "hermes";
export const APP_IDS: AppId[] = ["codex_cli", "codex_desktop", "claude_code", "claude_desktop", "aider", "cline", "pi", "hermes"];
export const TERMINAL_AGENTS: AppId[] = ["aider", "cline", "pi", "hermes"];
export type Route = "direct" | "gateway";

export const APP_NAME: Record<string, string> = {
  codex_cli: "Codex CLI", codex_desktop: "Codex Desktop", claude_code: "Claude Code CLI", claude_desktop: "Claude Desktop", aider: "Aider", cline: "Cline CLI", pi: "Pi", hermes: "Hermes Agent",
};

export const ROUTE_LABEL: Record<Route, string> = {
  direct: "Direct",
  gateway: "Via PrismFlow gateway",
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
  connections: number;
  upstreams: string[];
}

export interface ModelInfo {
  id: string;
  name: string | null;
  context_length: number | null;
  reports_tools: boolean | null;
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

export interface StageResult { stage: number; name: string; outcome: StageOutcome; detail: string }

export interface VerificationReport {
  started_at: number;
  finished_at: number;
  stages: StageResult[];
  status: ConnectionStatus;
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
}
export interface RuntimeSettings {
  rate_limit_per_minute: number;
  max_concurrency_per_provider: number;
  upstream_retries: number;
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
  disconnect: (id: string) => invoke<string | null>("disconnect", { id }),
  connections: () => invoke<Connection[]>("list_connections"),
  checkCustomUrl: (url: string) => invoke<UrlCheck>("check_custom_url", { url }),
  addCustomProvider: (name: string, url: string, protocol: Protocol, requiresKey: boolean, consent: boolean) =>
    invoke<Provider>("add_custom_provider", { name, url, protocol, requiresKey, consent }),
  removeCustomProvider: (id: string) => invoke<void>("remove_custom_provider", { id }),
  setRouting: (id: string, routing: Routing) => invoke<Connection>("set_routing", { id, routing }),
  getSettings: () => invoke<RuntimeSettings>("get_settings"),
  setSettings: (settings: RuntimeSettings) => invoke<RuntimeSettings>("set_settings", { settings }),
  checkUpdate: () => invoke<{ current: string; available: string | null; notes: string | null }>("check_update"),
  installUpdate: () => invoke<void>("install_update"),
  guidedSetup: (id: string) => invoke<{ gateway_url: string; auth_scheme: string; token: string; model: string; steps: string[] }>("guided_setup", { id }),
  companionState: () => invoke<CompanionView>("companion_state"),
  companionEnable: (address: string, port: number, keepOn: boolean) => invoke<CompanionView>("companion_enable", { address, port, keepOn }),
  companionDisable: () => invoke<CompanionView>("companion_disable"),
  companionPair: () => invoke<PairingView>("companion_pair"),
  companionRevoke: (id: string) => invoke<CompanionView>("companion_revoke", { id }),
  companionAllowActions: (id: string, allow: boolean) => invoke<CompanionView>("companion_allow_actions", { id, allow }),
  uninstall: (dryRun: boolean) => invoke<UninstallReport>("uninstall", { dryRun }),
  runningSessions: (id: string) => invoke<number[]>("running_sessions", { id }),
  launch: (id: string) => invoke<void>("launch_connection", { id }),
  diagnostics: () => invoke<DiagEvent[]>("get_diagnostics"),
  exportDiagnostics: () => invoke<string>("export_diagnostics"),
  gatewayStatus: () => invoke<GatewayStatus>("gateway_status"),
  restartGateway: () => invoke<GatewayStatus>("restart_gateway"),
};

/** The command a user runs to start the app with a connection. */
export function launchHint(c: Connection): string {
  if (TERMINAL_AGENTS.includes(c.app_id as AppId)) return c.managed_file;
  if (c.app_id === "codex_desktop") return "open the Codex app (restart it after changes)";
  if (c.app_id === "claude_desktop") return "Claude Desktop → Configure Third-Party Inference";
  return c.app_id === "claude_code" ? `claude --settings ${c.managed_file}` : `codex -p ${c.profile}`;
}

export const STATUS_LABEL: Record<ConnectionStatus, string> = {
  NotConfigured: "Not configured",
  NeedsCredentials: "Needs credentials",
  ConfigurationInvalid: "Configuration invalid",
  AuthenticationFailed: "Authentication failed",
  ProviderUnavailable: "Provider unavailable",
  ModelUnavailable: "Model unavailable",
  PartiallyCompatible: "Partially compatible",
  ConnectedAndVerified: "Connected and verified",
  Disconnected: "Disconnected",
  RequiresAttention: "Requires attention",
  Unverified: "Configured — not verified",
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
