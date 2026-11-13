/**
 * What the assistant learned about a database ("Learn this database"): the shapes of the `profile_*` commands, camelCase
 * as `src-tauri/src/ai/profile/model.rs` and `src-tauri/src/commands/profile.rs` serialise them.
 */
import type { NodeKind, ValueKind } from "./contract";
import type { AiProvider } from "./types";

export interface ColumnProfile {
  name: string;
  typeName: string;
  kind: ValueKind;
  nullable: boolean;
  primary?: boolean;
  comment?: string;
  default?: string;
  /** The name looks like a credential or personal data: no comment, default or values are kept. */
  sensitive?: boolean;
  /** MongoDB: the share of sampled documents holding the field, in percent. */
  presence?: number;
}

export interface IndexProfile {
  name: string;
  columns: string[];
  unique?: boolean;
  primary?: boolean;
  method?: string;
}

/** Values a column takes (at most 20) or the format they follow. */
export interface ValueHint {
  column: string;
  values?: string[];
  format?: string;
}

export interface Description {
  text: string;
  source: "llm" | "user";
  updatedAt: number;
  fingerprint?: string;
}

export interface ObjectProfile {
  schema?: string;
  name: string;
  kind: NodeKind;
  fingerprint: string;
  detailed: boolean;
  comment?: string;
  columns: ColumnProfile[];
  primaryKey?: string[];
  uniqueKeys?: string[][];
  indexes?: IndexProfile[];
  rowEstimate?: number;
  extra?: [string, string][];
  valueHints?: ValueHint[];
  description?: Description;
}

export type RelationSource = "fk" | "inferredName" | "mongoRef";

export interface RelationProfile {
  name?: string;
  from: string;
  columns: string[];
  to: string;
  toColumns: string[];
  toDatabase?: string;
  source: RelationSource;
  confidence: number;
  /** A declared key's referential actions as the engine reports them ("CASCADE", "SET NULL", "NO ACTION"…). */
  onDelete?: string;
  onUpdate?: string;
}

export interface RoutineProfile {
  schema?: string;
  name: string;
  kind: NodeKind;
  signature?: string;
}

export interface ProfileSummary {
  text: string;
  model: string;
  createdAt: number;
}

/** One entry of the user's glossary: free text, optionally about one object (`table` or `schema.table`). */
export interface ProfileNote {
  id: string;
  text: string;
  object?: string | null;
  createdAt: number;
  updatedAt: number;
}

export interface DatabaseProfile {
  version: number;
  engine: string;
  connectionId: string;
  database: string;
  fingerprint: string;
  firstLearnedAt: number;
  learnedAt: number;
  dataRead: boolean;
  model?: string;
  inputTokens: number;
  outputTokens: number;
  objects: ObjectProfile[];
  totalObjects: number;
  detailedObjects: number;
  truncated: boolean;
  excludedObjects: number;
  relations: RelationProfile[];
  routines: RoutineProfile[];
  summary?: ProfileSummary;
  notes: ProfileNote[];
  extras?: Record<string, unknown>;
}

/** `profile_status`: whether a profile exists and whether the structure changed since (null: not checked). */
export interface ProfileStatus {
  exists: boolean;
  learnedAt: number | null;
  objects: number;
  detailedObjects: number;
  dataRead: boolean;
  stale: boolean | null;
  checkedAt: number | null;
}

export interface ObjectDiff {
  added: string[];
  changed: string[];
  removed: string[];
  unchanged: number;
}

export interface ValuesOutcome {
  columns: number;
  hinted: number;
  failed: number;
  skipped?: string;
}

/** `profile_learn`. */
export interface LearnOutcome {
  status: ProfileStatus;
  refreshed: boolean;
  diff: ObjectDiff;
  values?: ValuesOutcome;
}

export type ProfileStep = "listing" | "describing" | "relations" | "sampling" | "values" | "summary" | "saving";

/** The `profile-progress` event: `done` of `total` in `step` (0 of 0 while the total is unknown). */
export interface ProfileProgress {
  runId: string;
  connectionId: string;
  database: string;
  step: ProfileStep;
  done: number;
  total: number;
  object?: string | null;
}

/** `profile_summary_estimate`: what a summary run would send, and with which model. */
export interface SummaryCost {
  objects: number;
  requests: number;
  inputTokens: number;
  outputTokens: number;
  dataRead: boolean;
  model: string;
  provider: AiProvider;
  /** None of the providers' settings carry prices today. */
  price: number | null;
}

/** `profile_summarize`. */
export interface SummaryOutcome {
  objects: number;
  described: number;
  requests: number;
  inputTokens: number;
  outputTokens: number;
  model: string;
  dataRead: boolean;
  summary: string | null;
}
