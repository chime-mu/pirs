// Minimal typebox shim: builds plain JSON Schema objects. Only the schema
// constructors commonly used by pi extensions are provided.
function withOpts(schema, opts) { return Object.assign(schema, opts || {}); }
const OPTIONAL = Symbol.for("pirs.optional");
export const Kind = Symbol.for("TypeBox.Kind");
export const Type = {
  Object(props, opts) {
    const required = [];
    const properties = {};
    for (const [k, v] of Object.entries(props || {})) {
      if (v && v[OPTIONAL]) {
        const { ...rest } = v;
        delete rest[OPTIONAL];
        properties[k] = rest;
      } else {
        properties[k] = v;
        required.push(k);
      }
    }
    const s = { type: "object", properties };
    if (required.length) s.required = required;
    return withOpts(s, opts);
  },
  String(opts) { return withOpts({ type: "string" }, opts); },
  Number(opts) { return withOpts({ type: "number" }, opts); },
  Integer(opts) { return withOpts({ type: "integer" }, opts); },
  Boolean(opts) { return withOpts({ type: "boolean" }, opts); },
  Null(opts) { return withOpts({ type: "null" }, opts); },
  Any(opts) { return withOpts({}, opts); },
  Unknown(opts) { return withOpts({}, opts); },
  Literal(v, opts) { return withOpts({ const: v, type: typeof v }, opts); },
  Array(items, opts) { return withOpts({ type: "array", items }, opts); },
  Union(items, opts) {
    // Union of literals of one type collapses to an enum (Google-compatible).
    if (items.length && items.every((i) => "const" in i && i.type === "string")) {
      return withOpts({ type: "string", enum: items.map((i) => i.const) }, opts);
    }
    return withOpts({ anyOf: items }, opts);
  },
  Record(_k, v, opts) { return withOpts({ type: "object", additionalProperties: v }, opts); },
  Tuple(items, opts) { return withOpts({ type: "array", items: items, minItems: items.length, maxItems: items.length }, opts); },
  Enum(obj, opts) { return withOpts({ type: "string", enum: Object.values(obj) }, opts); },
  Optional(s) { const c = { ...s }; c[OPTIONAL] = true; return c; },
  Partial(s) {
    const c = { ...s };
    delete c.required;
    return c;
  },
  Intersect(items, opts) { return withOpts({ allOf: items }, opts); },
  Readonly(s) { return s; },
  Pick(s, keys) { const props = {}; for (const k of keys) if (s.properties?.[k]) props[k] = s.properties[k]; return { type: "object", properties: props, required: (s.required || []).filter((r) => keys.includes(r)) }; },
  Omit(s, keys) { const props = {}; for (const [k, v] of Object.entries(s.properties || {})) if (!keys.includes(k)) props[k] = v; return { type: "object", properties: props, required: (s.required || []).filter((r) => !keys.includes(r)) }; },
};
export function StringEnum(values, opts) { return withOpts({ type: "string", enum: [...values] }, opts); }
export const Value = {
  Check(schema, v) { return globalThis.__pirs.validate(schema, v) === undefined; },
  Errors(schema, v) { const e = globalThis.__pirs.validate(schema, v); return e ? [{ message: e }] : []; },
  Default(_schema, v) { return v; },
  Clean(_schema, v) { return v; },
  Cast(_schema, v) { return v; },
};
export function Compile(schema) { return { Check: (v) => Value.Check(schema, v), Errors: (v) => Value.Errors(schema, v) }; }
export default { Type, StringEnum, Value, Compile, Kind };
