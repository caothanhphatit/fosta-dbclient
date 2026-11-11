/**
 * What an engine workspace tab keeps across restarts and tells the assistant, kept small. Pure (no DOM), so the size
 * bounds and the parsing of saved files are unit-tested; `sectionWorkspace.ts` builds the tab on top of it.
 */

/** A workspace tab's saved view: the section shown and each section's own state (by section id, plain JSON). */
export interface WorkspaceViewState {
  section: string;
  state: Record<string, unknown>;
}

/** The most a workspace tab's saved view may take as UTF-8 JSON. */
export const MAX_VIEW_BYTES = 32 * 1024;
/** The longest section id kept (ids are short names such as "documents"). */
const MAX_SECTION_ID = 64;
/** The most context text one section gives the assistant (characters). */
export const MAX_SECTION_CONTEXT = 4000;
/** The most context text a whole workspace tab gives the assistant (characters). */
export const MAX_WORKSPACE_CONTEXT = 12_000;

const encoder = new TextEncoder();
const isRecord = (value: unknown): value is Record<string, unknown> => typeof value === "object" && value !== null && !Array.isArray(value);

function bytes(value: unknown): number {
  return encoder.encode(JSON.stringify(value)).length;
}

/** `value` as plain JSON (what a file keeps); undefined when it cannot be written (a cycle, a BigInt) or is nothing. */
function plainJson(value: unknown): unknown {
  if (value === undefined) return undefined;
  try {
    const text = JSON.stringify(value);
    return text === undefined ? undefined : (JSON.parse(text) as unknown);
  } catch {
    return undefined;
  }
}

/**
 * `view` as it is saved: plain JSON only, at most `maxBytes`. Too large, it keeps only the shown section's state; still
 * too large, no state at all (the tab opens on that section with its defaults).
 */
export function boundViewState(view: WorkspaceViewState, maxBytes = MAX_VIEW_BYTES): WorkspaceViewState {
  const state: Record<string, unknown> = {};
  for (const [id, value] of Object.entries(view.state)) {
    const plain = plainJson(value);
    if (plain !== undefined) state[id] = plain;
  }
  const full = { section: view.section, state };
  if (bytes(full) <= maxBytes) return full;
  const own = view.section in state ? { section: view.section, state: { [view.section]: state[view.section] } } : null;
  if (own && bytes(own) <= maxBytes) return own;
  return { section: view.section, state: {} };
}

/** Reads a saved view (from a file written by any version); anything malformed gives undefined. */
export function parseViewState(value: unknown, maxBytes = MAX_VIEW_BYTES): WorkspaceViewState | undefined {
  if (!isRecord(value) || typeof value.section !== "string") return undefined;
  const section = value.section;
  if (section === "" || section.length > MAX_SECTION_ID) return undefined;
  return boundViewState({ section, state: isRecord(value.state) ? value.state : {} }, maxBytes);
}

/** One section's part of the assistant context. */
export interface SectionContextPart {
  title: string;
  text: string | null;
  active: boolean;
}

function cut(text: string, max: number): string {
  return text.length <= max ? text : `${text.slice(0, Math.max(0, max - 12))}\n… (cut)`;
}

/**
 * What the assistant is told about a workspace tab: `header` (the object and the section shown), then the context
 * of each section that gave some, the shown section first, each bounded and the whole within `MAX_WORKSPACE_CONTEXT`.
 */
export function workspaceContextText(header: string, parts: SectionContextPart[]): string {
  const ordered = [...parts.filter((part) => part.active), ...parts.filter((part) => !part.active)];
  let text = header;
  for (const part of ordered) {
    const body = part.text?.trim();
    if (!body) continue;
    const left = MAX_WORKSPACE_CONTEXT - text.length - part.title.length - 8;
    if (left < 200) break;
    text += `\n\n[${part.title}]\n${cut(body, Math.min(MAX_SECTION_CONTEXT, left))}`;
  }
  return cut(text, MAX_WORKSPACE_CONTEXT);
}
