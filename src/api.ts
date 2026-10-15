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

export type AppId = "codex_cli" | "claude_code";
export type Route = "direct" | "gateway";

export const APP_NAME: Record<string, string> = { codex_cli: "Codex CLI", claude_code: "Claude Code CLI" };

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
}

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
  runningSessions: (id: string) => invoke<number[]>("running_sessions", { id }),
  launch: (id: string) => invoke<void>("launch_connection", { id }),
  diagnostics: () => invoke<DiagEvent[]>("get_diagnostics"),
  exportDiagnostics: () => invoke<string>("export_diagnostics"),
  gatewayStatus: () => invoke<GatewayStatus>("gateway_status"),
  restartGateway: () => invoke<GatewayStatus>("restart_gateway"),
};

/** The command a user runs to start the app with a connection. */
export function launchHint(c: Connection): string {
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
