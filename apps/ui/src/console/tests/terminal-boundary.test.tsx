// @vitest-environment happy-dom

// The terminal's error boundary: a chunk that fails to load (or a view that throws) leaves an
// alert with "Try again", which renders the next attempt.

import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { TerminalBoundary } from '../terminal-boundary.tsx';

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

function Terminal({ attempt }: { attempt: number }) {
  if (attempt === 0) throw new Error('Failed to fetch dynamically imported module');
  return <p>terminal, attempt {attempt}</p>;
}

it('says the terminal could not be shown, and tries again with the next attempt', () => {
  // React reports the caught error; it is expected here.
  vi.spyOn(console, 'error').mockImplementation(() => {});
  render(<TerminalBoundary>{(attempt) => <Terminal attempt={attempt} />}</TerminalBoundary>);
  expect(screen.getByRole('alert').textContent).toMatch(/The terminal could not be shown/);
  fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
  expect(screen.queryByRole('alert')).toBeNull();
  expect(screen.getByText('terminal, attempt 1')).toBeTruthy();
});
