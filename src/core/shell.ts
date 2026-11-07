const ARGUMENT = /"([^"]*)"|'([^']*)'|(\S+)/g;

export function parseArgs(text: string): string[] {
  return [...text.matchAll(ARGUMENT)].map((match) => match[1] ?? match[2] ?? match[3] ?? "");
}

export function formatArgs(args: string[]): string {
  return args.map((arg) => (arg === "" || /\s/.test(arg) ? `"${arg}"` : arg)).join(" ");
}
