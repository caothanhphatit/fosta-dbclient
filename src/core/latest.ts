/**
 * Keeps only the newest of overlapping async requests: a slow page that arrives after a newer one (a quick
 * second page change, a sort, a filter) must not overwrite what the newer request showed. A request started with a
 * `cancel` callback is cancelled when a newer one starts or the owner closes, so the backend stops its work too
 * (four quick "Last page" clicks must not keep four server connections busy).
 */
export class LatestRequest {
  private counter = 0;
  private closed = false;
  private readonly running = new Map<number, () => void>();

  /** Starts a request; pass the returned ticket to `isCurrent` once its answer arrives, and to `finish`. */
  begin(cancel?: () => void): number {
    this.cancelRunning();
    this.counter += 1;
    if (cancel) this.running.set(this.counter, cancel);
    return this.counter;
  }

  /** The request's answer (or error) arrived: there is nothing left to cancel. */
  finish(ticket: number): void {
    this.running.delete(ticket);
  }

  /** True while `ticket` is the newest request and the owner has not closed. */
  isCurrent(ticket: number): boolean {
    return !this.closed && ticket === this.counter;
  }

  /** The owner closed (a tab): every pending answer is dropped and every running request cancelled. */
  close(): void {
    this.closed = true;
    this.cancelRunning();
  }

  private cancelRunning(): void {
    for (const cancel of this.running.values()) cancel();
    this.running.clear();
  }
}
