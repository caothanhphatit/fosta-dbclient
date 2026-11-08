/**
 * Keeps only the newest of overlapping async requests: a slow page that arrives after a newer one (a quick
 * second page change, a sort, a filter) must not overwrite what the newer request showed.
 */
export class LatestRequest {
  private counter = 0;
  private closed = false;

  /** Starts a request; pass the returned ticket to `isCurrent` once its answer arrives. */
  begin(): number {
    this.counter += 1;
    return this.counter;
  }

  /** True while `ticket` is the newest request and the owner has not closed. */
  isCurrent(ticket: number): boolean {
    return !this.closed && ticket === this.counter;
  }

  /** The owner closed (a tab): every pending answer is dropped. */
  close(): void {
    this.closed = true;
  }
}
