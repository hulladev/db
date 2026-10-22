// Runs before any application dependency in a fresh raw QuickJS realm. There are
// no OS, Node/Bun, native-module or network capabilities in this embedding.
(() => {
  const denied = () => { throw new TypeError("Ambient nondeterminism is unavailable"); };
  Object.defineProperty(Math, "random", { value: denied, writable: false, configurable: false });
  Object.freeze(Math);
  const NativeDate = Date;
  function explicit(value) {
    if (typeof value === "number") return value;
    if (typeof value === "string" && /(?:Z|[+-]\d{2}:?\d{2})$/.test(value)) return value;
    throw new TypeError("Date requires an explicit timestamp or timezone");
  }
  function DeterministicDate(value) {
    if (!new.target || arguments.length !== 1) return denied();
    return Reflect.construct(NativeDate, [explicit(value)], new.target);
  }
  DeterministicDate.prototype = NativeDate.prototype;
  Object.defineProperty(DeterministicDate.prototype, "constructor", { value: DeterministicDate, writable: false, configurable: false });
  Object.defineProperties(DeterministicDate, {
    now: { value: denied },
    parse: { value: value => NativeDate.parse(explicit(value)) },
    UTC: { value: NativeDate.UTC },
  });
  for (const key of ["getDate", "getDay", "getFullYear", "getHours", "getMilliseconds", "getMinutes", "getMonth", "getSeconds", "getTimezoneOffset", "getYear", "setDate", "setFullYear", "setHours", "setMilliseconds", "setMinutes", "setMonth", "setSeconds", "setYear", "toDateString", "toLocaleDateString", "toLocaleString", "toLocaleTimeString", "toString", "toTimeString"]) {
    Object.defineProperty(NativeDate.prototype, key, { value: denied, configurable: false, writable: false });
  }
  Object.freeze(NativeDate.prototype);
  Object.freeze(DeterministicDate);
  Object.defineProperty(globalThis, "Date", { value: DeterministicDate, configurable: false, writable: false });
})();
