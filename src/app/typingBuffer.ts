/**
 * Keeps what the user types while a lazily loaded editor is on its way (the query view pulls in
 * CodeMirror as its own chunk), so the first keystrokes after Ctrl+T are not lost.
 */
export class TypingBuffer {
  private text = "";

  /** Applies one key press; returns true when the key was taken. */
  handle(event: { key: string; ctrlKey?: boolean; metaKey?: boolean; altKey?: boolean }): boolean {
    if (event.ctrlKey || event.metaKey || event.altKey) return false;
    if (event.key === "Enter") {
      this.text += "\n";
      return true;
    }
    if (event.key === "Tab") {
      this.text += "  ";
      return true;
    }
    if (event.key === "Backspace") {
      this.text = this.text.slice(0, -1);
      return true;
    }
    // Printable characters have a one-character key (one code point, so surrogate pairs count as one).
    if ([...event.key].length === 1) {
      this.text += event.key;
      return true;
    }
    return false;
  }

  get value(): string {
    return this.text;
  }
}

/** Collects typing aimed at the page (not at an input) until the returned function is called; it returns the text. */
export function captureTyping(): () => string {
  const buffer = new TypingBuffer();
  const onKey = (event: KeyboardEvent) => {
    const target = event.target instanceof HTMLElement ? event.target : null;
    if (target?.closest("input, textarea, select, [contenteditable=true], .modal-overlay")) return;
    if (buffer.handle(event)) event.preventDefault();
  };
  document.addEventListener("keydown", onKey, true);
  document.body.classList.add("loading-editor");
  return () => {
    document.removeEventListener("keydown", onKey, true);
    document.body.classList.remove("loading-editor");
    return buffer.value;
  };
}
