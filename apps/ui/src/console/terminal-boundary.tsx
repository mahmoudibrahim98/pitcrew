// Keeps a terminal that fails, to load its chunk or to render, from taking the console with it.
// "Try again" renders it afresh with the next attempt number, so a lazy import that failed (React
// keeps its error) can be made again (see `terminalView` in console-page.tsx).

import { Component, type ReactNode } from 'react';
import { Button } from '../design/index.ts';

interface Props {
  /** The terminal, for this attempt (0, then 1, 2, … after each "Try again"). */
  children: (attempt: number) => ReactNode;
}

interface State {
  failed: boolean;
  attempt: number;
}

export class TerminalBoundary extends Component<Props, State> {
  override state: State = { failed: false, attempt: 0 };

  static getDerivedStateFromError(): Partial<State> {
    return { failed: true };
  }

  override render(): ReactNode {
    if (!this.state.failed) return this.props.children(this.state.attempt);
    return (
      <div role="alert" className="flex flex-col items-start gap-2 p-4 text-sm">
        <p>The terminal could not be shown. The network may have dropped while it loaded.</p>
        <Button onClick={() => this.setState((state) => ({ failed: false, attempt: state.attempt + 1 }))}>
          Try again
        </Button>
      </div>
    );
  }
}
