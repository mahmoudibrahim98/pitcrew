// The tests' clock: it starts at PINNED_NOW and runs at the real rate, so relative times ("3h")
// are fixed while timeouts and back-offs still see time pass. setup.ts installs it in every test
// file, and hub-process.ts preloads it into the mock hubs tests start as child processes, so the
// hub and the UI agree on the time. `vi.useFakeTimers` starts from it too.

/** 1 October 2026, 09:00 UTC: the morning after the demo data's last event. */
export const PINNED_NOW = Date.UTC(2026, 9, 1, 9, 0);

const RealDate = Date;
const offset = PINNED_NOW - RealDate.now();
const now = () => RealDate.now() + offset;
globalThis.Date = new Proxy(RealDate, {
  construct: (target, args, newTarget) => Reflect.construct(target, args.length === 0 ? [now()] : args, newTarget),
  apply: () => new RealDate(now()).toString(),
  get: (target, key, receiver) => (key === 'now' ? now : Reflect.get(target, key, receiver)),
});
