// A per-editor cache of the completion schema, loaded from the backend `completion_schema` command and swappable at
// any time (the completion sources read it on every request).
import { EMPTY_SCHEMA, type CompletionSchema } from "./types";

export type SchemaLoader = () => Promise<CompletionSchema>;

export class SchemaCache {
  private schema: CompletionSchema;
  private loading: Promise<CompletionSchema> | null = null;
  private readonly listeners = new Set<(schema: CompletionSchema) => void>();

  constructor(initial: CompletionSchema = EMPTY_SCHEMA) {
    this.schema = initial;
  }

  get(): CompletionSchema {
    return this.schema;
  }

  set(schema: CompletionSchema): void {
    this.schema = schema;
    for (const listener of this.listeners) listener(schema);
  }

  /** Loads through `loader`; concurrent calls share one request. A failed load keeps the previous schema. */
  async load(loader: SchemaLoader): Promise<CompletionSchema> {
    if (!this.loading) {
      this.loading = loader()
        .then((schema) => {
          this.set(schema);
          return schema;
        })
        .catch(() => this.schema)
        .finally(() => {
          this.loading = null;
        });
    }
    return this.loading;
  }

  onChange(listener: (schema: CompletionSchema) => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }
}

/** Calls the backend `completion_schema` command (Tauri or the web server, through the transport). */
export async function fetchCompletionSchema(connectionId: string, database: string | null): Promise<CompletionSchema> {
  const { invoke } = await import("../core/transport");
  const schema = await invoke<CompletionSchema | null>("completion_schema", { connectionId, database });
  return schema ?? EMPTY_SCHEMA;
}
