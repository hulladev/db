import { captureInput } from './function-input.ts';

function argumentsSnapshot(args: readonly unknown[]): unknown[] {
  return args.map(value => {
    if (typeof value === 'function') return value;
    // Query/subscription options may carry a live cancellation signal, which
    // belongs to the call rather than its submitted function data.
    if (value && typeof value === 'object' && Object.getPrototypeOf(value) === Object.prototype) {
      const descriptors = Object.getOwnPropertyDescriptors(value);
      if (descriptors.signal && 'value' in descriptors.signal && descriptors.signal.value instanceof AbortSignal) {
        return Object.fromEntries(Object.entries(descriptors).map(([key, property]) => {
          if (!('value' in property)) throw new TypeError('Call arguments must contain ordinary values');
          return [key, key === 'signal' ? property.value : captureInput(property.value)];
        }));
      }
    }
    return captureInput(value);
  });
}

/** A catalogue loader must not force callers to wait before authoring a call. */
export function deferredFunctions(load: () => Promise<Record<string, unknown>>, path: readonly string[] = [], transforms: readonly { name: string; args: unknown[] }[] = []): unknown {
  const resolve = async (): Promise<unknown> => {
    let value: unknown = await load();
    for (const key of path) {
      if (!value || (typeof value !== "object" && typeof value !== "function") || !Object.hasOwn(value, key)) throw new TypeError(`Unknown function ${path.join(".")}`);
      value = Reflect.get(value, key);
    }
    for (const transform of transforms) value = Reflect.apply(Reflect.get(value as object, transform.name), value, transform.args);
    return value;
  };
  const children = new Map<string, unknown>();
  return new Proxy(() => {}, {
    get(_target, key) {
      if (key === "then" || typeof key !== "string") return undefined;
      if (key === "offline" || key === "optimistic") return (...args: unknown[]) => deferredFunctions(load, path, [...transforms, { name: key, args }]);
      if (key === "subscribe") return (...args: unknown[]) => {
        args = argumentsSnapshot(args);
        let cancelled = false, stop: (() => void) | undefined;
        void resolve().then(value => { if (!cancelled) stop = Reflect.apply(Reflect.get(value as object, key), value, args); }).catch(error => {
          if (cancelled) return;
          const listener = args.findLast(value => typeof value === "function") as ((state: unknown) => void) | undefined;
          listener?.({ status: "error", error });
        });
        return () => { cancelled = true; stop?.(); };
      };
      if (!children.has(key)) children.set(key, deferredFunctions(load, [...path, key]));
      return children.get(key);
    },
    apply(_target, _receiver, args) {
      const preparation = (() => {
        try { return { args: argumentsSnapshot(args) }; }
        catch (error) { return { error }; }
      })();
      let handle: Record<string, unknown> | undefined;
      let error: unknown;
      let failed = false;
      // Keep the handle inside a plain envelope: resolving a Promise directly
      // with it would assimilate its then() and lose subscription/state methods.
      const loading = ('error' in preparation ? Promise.reject(preparation.error) : resolve()).then(value => {
        if (typeof value !== "function") throw new TypeError(`Expected a function at ${path.join(".")}`);
        handle = Reflect.apply(value, undefined, preparation.args!);
        return { handle: handle! };
      }).catch(reason => { failed = true; error = reason; throw reason; });
      void loading.catch(() => {});
      let outcomePromise: Promise<unknown> | undefined;
      const outcome = () => outcomePromise ??= loading.then(({ handle }) => handle);
      return new Proxy(Object.create(null), {
        get(_handle, key) {
          if (key === "then" || key === "catch" || key === "finally") return (...args: unknown[]) => Reflect.apply(Reflect.get(outcome(), key), outcome(), args);
          if (key === "status") return handle?.status ?? (failed ? "error" : "pending");
          if (key === "error") return handle?.error ?? error;
          if (key === "data" || key === "isStale" || key === "isOptimistic") return handle?.[key];
          if (key === "subscribe") return (...args: unknown[]) => {
            let cancelled = false, stop: (() => void) | undefined;
            void loading.then(({ handle }) => { if (!cancelled) stop = Reflect.apply(handle.subscribe as (...args: unknown[]) => () => void, handle, args); }).catch(error => {
              if (!cancelled && typeof args[0] === "function") args[0]({ status: "error", error });
            });
            return () => { cancelled = true; stop?.(); };
          };
          return undefined;
        },
      });
    },
  });
}
