/** FIFO async critical sections. Failures never poison a key or block other keys. */
export class KeyedMutex {
  private readonly tails = new Map<string, Promise<void>>();
  async run<T>(key: string, fn: () => Promise<T> | T): Promise<T> {
    const previous = this.tails.get(key) ?? Promise.resolve();
    let release!: () => void;
    const tail = new Promise<void>(resolve => { release = resolve; });
    this.tails.set(key, tail);
    await previous;
    try { return await fn(); }
    finally { release(); if (this.tails.get(key) === tail) this.tails.delete(key); }
  }
}
export const listenMutex = new KeyedMutex();
export const sessionMutex = new KeyedMutex();
export const oidcMutex = new KeyedMutex();
