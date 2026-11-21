/** The longest an owner waits for a command's receipt: its I/O timeout is at most 300 seconds. */
const LONGEST_WAIT_MS = 300_000;
/** The wall clock running ahead of the monotonic one by less than this is drift, not a sleep. */
const SLEEP_MS = 1_000;
const KEPT_SLEEPS = 4_096;

type Sleep = { woke: number; slept: number };

/**
 * Whether a command's deadline passed, counted in time the Mac was awake. The owner writes the
 * deadline on the wall clock, which goes on while the Mac sleeps; its own wait for the receipt
 * does not, and neither does this process, frozen like the owner. A sleep is the wall clock
 * running ahead of the monotonic one between two looks. One that ended after the command could
 * have been written is not lateness.
 */
export class AwakeClock {
  private sleeps: Sleep[] = [];
  private last: { wall: number; monotonic: number };

  constructor(private wall: () => number = () => Date.now(), private monotonic: () => number = () => Number(process.hrtime.bigint() / 1_000_000n)) {
    this.last = { wall: this.wall(), monotonic: this.monotonic() };
  }

  /** Notes a sleep since the last look. */
  observe(): void {
    const now = { wall: this.wall(), monotonic: this.monotonic() };
    const slept = (now.wall - this.last.wall) - (now.monotonic - this.last.monotonic);
    if (slept >= SLEEP_MS) {
      this.sleeps.push({ woke: now.wall, slept });
      if (this.sleeps.length > KEPT_SLEEPS) this.sleeps.shift();
    }
    this.last = now;
  }

  passed(deadline: number): boolean {
    this.observe();
    const written = deadline - LONGEST_WAIT_MS;
    const slept = this.sleeps.filter((sleep) => sleep.woke > written).reduce((total, sleep) => total + sleep.slept, 0);
    return deadline < this.last.wall - slept;
  }
}
