// Runs before every test file (vitest.config.ts, `setupFiles`). Rendered dates depend on the
// machine's time zone, locale and clock; these are pinned here, so a test reads the same on a
// laptop in any country, on CI, and next month:
// - the time zone is UTC;
// - the default locale for dates is en-GB ("10 Oct", "Wednesday, 30 September 2026"). Node's own
//   default comes from the system (and on Windows ignores LANG), so the date formatters are
//   wrapped instead;
// - the clock starts at 1 October 2026, 09:00 UTC (clock.ts).

import './clock.ts';

export const PINNED_LOCALE = 'en-GB';

process.env.TZ = 'UTC';

const NativeDateTimeFormat = Intl.DateTimeFormat;
function DateTimeFormat(locales?: Intl.LocalesArgument, options?: Intl.DateTimeFormatOptions) {
  return new NativeDateTimeFormat(locales ?? PINNED_LOCALE, options);
}
DateTimeFormat.prototype = NativeDateTimeFormat.prototype;
DateTimeFormat.supportedLocalesOf = NativeDateTimeFormat.supportedLocalesOf;
Intl.DateTimeFormat = DateTimeFormat as unknown as Intl.DateTimeFormatConstructor;

for (const method of ['toLocaleString', 'toLocaleDateString', 'toLocaleTimeString'] as const) {
  const native = Date.prototype[method];
  Date.prototype[method] = function (
    this: Date,
    locales?: Intl.LocalesArgument,
    options?: Intl.DateTimeFormatOptions,
  ) {
    return native.call(this, locales ?? PINNED_LOCALE, options);
  };
}
