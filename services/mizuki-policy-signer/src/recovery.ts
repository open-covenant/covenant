export class RecoveryRunner {
  private task: Promise<void> | null = null;

  constructor(
    private readonly recover: (limit?: number) => Promise<void>,
    private readonly onFailure: () => void,
  ) {}

  run(limit?: number): Promise<void> {
    if (this.task) return this.task;
    this.task = Promise.resolve()
      .then(() => this.recover(limit))
      .catch(() => this.onFailure())
      .finally(() => {
        this.task = null;
      });
    return this.task;
  }

  active(): Promise<void> | null {
    return this.task;
  }
}
