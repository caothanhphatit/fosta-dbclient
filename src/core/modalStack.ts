/**
 * The stack of open modal dialogs. Keys go to the top-most dialog only: Esc closes one dialog,
 * not every dialog at once, Enter acts on the top dialog, and the app's own shortcuts and the
 * views behind a dialog do not see keys while one is open. DOM-free so it can be unit-tested;
 * `dialogs.ts` installs the single document listener that feeds it.
 */

/** The parts of a KeyboardEvent the stack needs. */
export interface StackKeyEvent {
  key: string;
  target: unknown;
  stopPropagation(): void;
  preventDefault(): void;
}

export interface ModalEntry {
  /** Whether a key event target is inside this dialog. */
  contains(target: unknown): boolean;
  /**
   * Handles a key meant for this dialog. `inside` is false when focus is elsewhere (for example on the page
   * body behind the dialog). Returns true when the key was used, which stops it from reaching anything else.
   */
  handleKey(event: StackKeyEvent, inside: boolean): boolean;
}

export class ModalStack {
  private readonly entries: ModalEntry[] = [];

  push(entry: ModalEntry): () => void {
    this.entries.push(entry);
    return () => this.remove(entry);
  }

  remove(entry: ModalEntry): void {
    const index = this.entries.indexOf(entry);
    if (index >= 0) this.entries.splice(index, 1);
  }

  get size(): number {
    return this.entries.length;
  }

  top(): ModalEntry | undefined {
    return this.entries[this.entries.length - 1];
  }

  /** Routes a key event (from a capturing document listener) to the top-most dialog. */
  dispatch(event: StackKeyEvent): void {
    const top = this.top();
    if (!top) return;
    const inside = top.contains(event.target);
    const used = top.handleKey(event, inside);
    if (used) {
      event.preventDefault();
      event.stopPropagation();
    } else if (!inside) {
      // Keys aimed at whatever is behind the dialog (a grid, a lower dialog, the app shortcuts) are dropped.
      event.stopPropagation();
    }
  }
}
