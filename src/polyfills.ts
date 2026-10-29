/** Methods missing from Catalina's WKWebView (Safari 13). Vite downlevels the syntax. */

const objectAny = Object as unknown as {
  hasOwn?: (obj: object, key: PropertyKey) => boolean;
};

if (typeof objectAny.hasOwn !== "function") {
  Object.defineProperty(Object, "hasOwn", {
    configurable: true,
    writable: true,
    value(obj: object, key: PropertyKey) {
      return Object.prototype.hasOwnProperty.call(obj, key);
    },
  });
}

function atIndex(this: ArrayLike<unknown>, index: number): unknown {
  let n = Math.trunc(index) || 0;
  if (n < 0) n += this.length;
  if (n < 0 || n >= this.length) return undefined;
  return this[n];
}

const arrayProto = Array.prototype as unknown as {
  at?: unknown;
  findLast?: unknown;
  findLastIndex?: unknown;
};

if (typeof arrayProto.at !== "function") {
  Object.defineProperty(Array.prototype, "at", {
    configurable: true,
    writable: true,
    value: atIndex,
  });
}

if (typeof (String.prototype as unknown as { at?: unknown }).at !== "function") {
  Object.defineProperty(String.prototype, "at", {
    configurable: true,
    writable: true,
    value: atIndex,
  });
}

if (typeof arrayProto.findLast !== "function") {
  Object.defineProperty(Array.prototype, "findLast", {
    configurable: true,
    writable: true,
    value(
      pred: (value: unknown, index: number, list: unknown[]) => boolean,
      thisArg?: unknown
    ) {
      const list = this as unknown[];
      for (let i = list.length - 1; i >= 0; i--) {
        if (pred.call(thisArg, list[i], i, list)) return list[i];
      }
      return undefined;
    },
  });
}

if (typeof arrayProto.findLastIndex !== "function") {
  Object.defineProperty(Array.prototype, "findLastIndex", {
    configurable: true,
    writable: true,
    value(
      pred: (value: unknown, index: number, list: unknown[]) => boolean,
      thisArg?: unknown
    ) {
      const list = this as unknown[];
      for (let i = list.length - 1; i >= 0; i--) {
        if (pred.call(thisArg, list[i], i, list)) return i;
      }
      return -1;
    },
  });
}

const root = globalThis as unknown as { structuredClone?: (value: unknown) => unknown };
if (typeof root.structuredClone !== "function") {
  root.structuredClone = (value: unknown) => {
    if (typeof value === "undefined") return undefined;
    return JSON.parse(JSON.stringify(value)) as unknown;
  };
}
