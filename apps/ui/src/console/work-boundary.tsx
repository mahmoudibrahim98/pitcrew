// Keeps a Work view that fails - to load its chunk (src/projects' `SessionWork`) or to render -
// from taking the console with it. "Try again" renders it afresh with the next attempt number, as
// `TerminalBoundary` does for the terminal.

import { Component, type ReactNode } from 'react';
import { Button } from '../design/index.ts';

interface Props {
  /** The Work view, for this attempt (0, then 1, 2, … after each "Try again"). */
  children: (attempt: number) => ReactNode;
}

interface State {
  failed: boolean;
  attempt: number;
}

export class WorkBoundary extends Component<Props, State> {
  override state: State = { failed: false, attempt: 0 };

  static getDerivedStateFromError(): Partial<State> {
    return { failed: true };
  }

  override render(): ReactNode {
    if (!this.state.failed) return this.props.children(this.state.attempt);
    return (
      <div role="alert" className="flex flex-col items-start gap-2 p-4 text-sm">
        <p>The session's work could not be shown.</p>
        <Button onClick={() => this.setState((state) => ({ failed: false, attempt: state.attempt + 1 }))}>
          Try again
        </Button>
      </div>
    );
  }
}
