// Where the sign-in step shows a CLI's login: the console's own terminal view (`TerminalView`, a
// lazy chunk with xterm.js), on the terminals route (`/v1/sessions/{id}/terminal`) of the workspace
// in view, as for any session. It is reached through a context so the step can be tested without
// a hub, and so a failed chunk load stays inside the step.

import { Component, createContext, lazy, Suspense, use, type ReactNode } from 'react';

export interface SignInTerminalProps {
  /** The sign-in terminal's id (`SignIn.terminal`). */
  terminal: string;
}

const ConsoleTerminal = lazy(() =>
  import('../console/index.ts').then(({ TerminalView }) => ({
    default: function SignInTerminalView({ terminal }: SignInTerminalProps) {
      return <TerminalView sessionId={terminal} className="h-72" />;
    },
  })),
);

/** Renders the terminal for a sign-in. */
export type RenderSignInTerminal = (props: SignInTerminalProps) => ReactNode;

const SignInTerminalContext = createContext<RenderSignInTerminal>((props) => <ConsoleTerminal {...props} />);

/** Tests (and any host without the console) give their own terminal. */
export function SignInTerminalProvider(props: { render: RenderSignInTerminal; children: ReactNode }) {
  return <SignInTerminalContext value={props.render}>{props.children}</SignInTerminalContext>;
}

class Boundary extends Component<{ children: ReactNode }, { failed: boolean }> {
  override state = { failed: false };

  static getDerivedStateFromError(): { failed: boolean } {
    return { failed: true };
  }

  override render(): ReactNode {
    if (this.state.failed) {
      return (
        <p role="alert" className="text-sm text-risk">
          The terminal could not be shown. The login still runs on the machine: go back and open it again.
        </p>
      );
    }
    return this.props.children;
  }
}

/** The login's terminal. */
export function SignInTerminal({ terminal }: SignInTerminalProps) {
  const render = use(SignInTerminalContext);
  return (
    <Boundary>
      <Suspense fallback={<p className="text-sm text-ink-2">Loading the terminal…</p>}>{render({ terminal })}</Suspense>
    </Boundary>
  );
}
