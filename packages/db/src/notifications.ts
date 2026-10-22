// Stage adapter and handle state before delivering any application callback.
let depth = 0;
let pending: (() => void)[] = [];
let staged: (() => void)[] = [];
/** Derived state joins the adapter transaction before any application observer runs. */
export function afterBatch(work: () => void): void {
  if (depth) staged.push(work); else work();
}
const internal = new WeakSet<Function>();
export function stagedObserver<T extends Function>(observer: T): T { internal.add(observer); return observer; }
/** Preserve internal staging through a subscription's retention wrapper. */
export function derivedObserver<T extends Function>(source: Function, observer: T): T {
  if (internal.has(source)) internal.add(observer);
  return observer;
}
export function notifyObserver(observer: Function, deliver: () => void): void {
  if (internal.has(observer)) deliver(); else notification(deliver);
}
export function notification(deliver: () => void): void {
  if (depth) pending.push(deliver); else deliver();
}
export function batchNotifications(work: () => void): void {
  depth++;
  try { work(); } finally {
    let failed = false;
    let failure: unknown;
    try {
      if (depth === 1) {
        while (staged.length) {
          const work = staged; staged = [];
          for (const stage of work) {
            try { stage(); } catch (error) {
              if (!failed) { failed = true; failure = error; }
            }
          }
        }
      }
    } finally {
      if (--depth === 0) {
        const callbacks = pending; pending = [];
        for (const callback of callbacks) callback();
      }
    }
    if (failed) throw failure;
  }
}
