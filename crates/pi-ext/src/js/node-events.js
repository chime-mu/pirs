export class EventEmitter {
  constructor() { this.listeners = new Map(); }
  on(ev, fn) { (this.listeners.get(ev) ?? this.listeners.set(ev, []).get(ev)).push(fn); return this; }
  once(ev, fn) { const w = (...a) => { this.off(ev, w); fn(...a); }; return this.on(ev, w); }
  off(ev, fn) { this.listeners.set(ev, (this.listeners.get(ev) || []).filter((f) => f !== fn)); return this; }
  emit(ev, ...args) { const l = this.listeners.get(ev) || []; for (const f of [...l]) f(...args); return l.length > 0; }
  removeAllListeners(ev) { if (ev) this.listeners.delete(ev); else this.listeners.clear(); return this; }
  addListener(ev, fn) { return this.on(ev, fn); }
  removeListener(ev, fn) { return this.off(ev, fn); }
}
export default EventEmitter;
