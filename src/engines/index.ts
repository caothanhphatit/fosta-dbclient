/**
 * Engine UI modules. Each `src/engines/<id>/index.ts` default-exports an `EngineUiModule`; they are found with
 * `import.meta.glob`, so adding an engine needs no edit here, and each is a lazy chunk loaded the first time a connection
 * of that engine needs a view (`ensureEngine`). Shared registrations (`shared.ts`) are installed on import.
 *
 * Only this folder may know engine ids; shared UI (`src/core`, `src/features`, `src/app`) never imports it.
 */
import type { IconName } from "../core/dom";
import { defaultLanguage, engineInfo } from "../core/engines";
import { defaultLanguageSupport, sqlLanguageSupport, type LanguageSupport } from "../core/language";
import { registerViews, setEngineLoader } from "../core/views";
import { sharedViews } from "./shared";

export interface EngineUiModule {
  id: string;
  /** Adds the engine's own views; they win over the shared ones for the same kind and role. */
  register(views: typeof registerViews): void;
  /** Query languages by `LanguageSpec.id`. */
  languages?: Record<string, () => Promise<LanguageSupport>>;
  /** Icons for `ObjectNode.icon` values of the form "engine:<id>/<name>". */
  icons?: Record<string, IconName>;
}

const modules = import.meta.glob<{ default: EngineUiModule }>("./*/index.ts");
const loaded = new Map<string, Promise<EngineUiModule | null>>();
const resolvedIcons = new Map<string, IconName>();

registerViews(sharedViews);
setEngineLoader(async (id) => void (await ensureEngine(id)));

/** Loads and registers the engine's UI module once; null when the engine has none (it then uses the shared views). */
export function ensureEngine(id: string): Promise<EngineUiModule | null> {
  let pending = loaded.get(id);
  if (!pending) {
    const load = modules[`./${id}/index.ts`];
    pending = load
      ? load().then(({ default: module }) => {
          module.register(registerViews);
          for (const [name, icon] of Object.entries(module.icons ?? {})) resolvedIcons.set(`engine:${module.id}/${name}`, icon);
          return module;
        })
      : Promise.resolve(null);
    pending.catch(() => loaded.delete(id));
    loaded.set(id, pending);
  }
  return pending;
}

/** The editor support for one of the engine's languages (default: the engine's default language). */
export async function languageSupport(engineId: string, languageId?: string): Promise<LanguageSupport> {
  const module = await ensureEngine(engineId);
  const descriptor = engineInfo(engineId);
  const spec = descriptor.languages.find((language) => language.id === languageId) ?? defaultLanguage(descriptor);
  const id = spec?.id ?? languageId ?? "text";
  const custom = module?.languages?.[id];
  if (custom) return custom();
  return spec?.editor === "sql" ? sqlLanguageSupport(spec.dialect ?? descriptor.sqlDialect) : defaultLanguageSupport(id);
}

/** Maps an "engine:<id>/<name>" node icon through the engine module's `icons` (once loaded); other values pass through. */
export function engineIcon(icon: string): string {
  return icon.startsWith("engine:") ? (resolvedIcons.get(icon) ?? "database") : icon;
}
