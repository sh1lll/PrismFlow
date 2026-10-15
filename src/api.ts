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
  has_credential: boolean;
  codex_direct: boolean;
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
  managed_file: string;
  created_at: number;
  status: ConnectionStatus;
  last_report: VerificationReport | null;
}

export interface ConnectPreview {
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
  preview: (providerId: string, model: string) => invoke<ConnectPreview>("preview_connection", { providerId, model }),
  connect: (providerId: string, model: string) => invoke<Connection>("connect", { providerId, model }),
  verify: (id: string) => invoke<VerificationReport>("verify_connection", { id }),
  disconnect: (id: string) => invoke<string | null>("disconnect", { id }),
  connections: () => invoke<Connection[]>("list_connections"),
  launch: (id: string) => invoke<void>("launch_connection", { id }),
  diagnostics: () => invoke<DiagEvent[]>("get_diagnostics"),
  exportDiagnostics: () => invoke<string>("export_diagnostics"),
};

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
