/**
 * Times the attempts of a gateway call that may pause for a confirmation dialog: only the
 * last attempt counts, so the time the user spends reading the dialog is not query time.
 */
export class AttemptTimer {
  private started: number | null = null;

  constructor(private readonly now: () => number = () => performance.now()) {}

  /** Wraps one attempt: restarts the clock and reports when the attempt is in flight. */
  async attempt<T>(call: () => Promise<T>, inFlight: (running: boolean) => void = () => undefined): Promise<T> {
    this.started = this.now();
    inFlight(true);
    try {
      return await call();
    } finally {
      inFlight(false);
    }
  }

  /** Milliseconds since the last attempt started; 0 when nothing ran. */
  elapsed(): number {
    return this.started === null ? 0 : Math.max(0, this.now() - this.started);
  }
}
