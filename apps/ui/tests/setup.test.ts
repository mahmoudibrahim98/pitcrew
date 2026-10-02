import { afterEach, describe, expect, it, vi } from 'vitest';
import { PINNED_NOW } from './setup.ts';

// tests/setup.ts pins what rendered dates depend on, whatever the machine's settings.
describe('the test setup', () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it('pins the time zone to UTC', () => {
    expect(new Date(PINNED_NOW).getTimezoneOffset()).toBe(0);
    expect(new Intl.DateTimeFormat().resolvedOptions().timeZone).toBe('UTC');
  });

  it('pins the locale for dates to en-GB', () => {
    const at = Date.UTC(2026, 9, 10, 14, 5);
    expect(new Intl.DateTimeFormat(undefined, { day: 'numeric', month: 'short' }).format(at)).toBe('10 Oct');
    expect(Intl.DateTimeFormat(undefined, { day: 'numeric', month: 'short' }).format(at)).toBe('10 Oct');
    expect(new Date(at).toLocaleDateString(undefined, { day: 'numeric', month: 'short' })).toBe('10 Oct');
    expect(new Date(at).toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' })).toBe('14:05');
    expect(new Date(at).toLocaleString()).toBe('10/10/2026, 14:05:00');
    // A locale asked for by name is kept.
    expect(new Intl.DateTimeFormat('en-US', { day: 'numeric', month: 'short' }).format(at)).toBe('Oct 10');
    expect(new Intl.DateTimeFormat()).toBeInstanceOf(Intl.DateTimeFormat);
  });

  it('starts the clock at the pinned time and lets it run', async () => {
    const start = Date.now();
    expect(start - PINNED_NOW).toBeGreaterThanOrEqual(0);
    expect(start - PINNED_NOW).toBeLessThan(10 * 60_000);
    expect(new Date().getTime() - start).toBeGreaterThanOrEqual(0);
    expect(new Date(0).getTime()).toBe(0);
    expect(new Date()).toBeInstanceOf(Date);
    await new Promise((done) => setTimeout(done, 20));
    expect(Date.now()).toBeGreaterThan(start);
  });

  it('starts fake timers from the pinned clock', () => {
    vi.useFakeTimers();
    expect(Date.now() - PINNED_NOW).toBeLessThan(10 * 60_000);
    vi.advanceTimersByTime(60_000);
    expect(Date.now() - PINNED_NOW).toBeGreaterThanOrEqual(60_000);
    vi.useRealTimers();
    expect(Date.now() - PINNED_NOW).toBeLessThan(10 * 60_000);
  });
});
