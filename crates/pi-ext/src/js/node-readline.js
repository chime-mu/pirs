export function createInterface() {
  return { question: () => Promise.reject(new Error("readline is not supported in pirs extensions; use ctx.ui.input()")), close() {}, on() { return this; }, once() { return this; }, prompt() {}, setPrompt() {}, [Symbol.asyncIterator]: async function* () {} };
}
export const promises = { createInterface };
export default { createInterface, promises };
