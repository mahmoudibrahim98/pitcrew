// The Back / Skip / primary-action row every step ends with. Wrapping a step's fields in a
// `<form onSubmit>` with this as its last child gives Enter-to-advance for free (the browser's own
// form submission), and the primary button stays a real `type="submit"` so it does too.

import { Button } from '../design/index.ts';
import { useWizard } from './wizard-context.tsx';

export function StepFooter({
  nextLabel = 'Continue',
  nextDisabled = false,
  onSkip,
  skipLabel = 'Skip for now',
  busy = false,
}: {
  nextLabel?: string;
  nextDisabled?: boolean;
  /** Present only on a skippable step; calling it advances without running the step's action. */
  onSkip?: () => void;
  skipLabel?: string;
  busy?: boolean;
}) {
  const { stepIndex, back } = useWizard();
  return (
    <div className="mt-6 flex items-center justify-between gap-2 border-t border-line pt-4">
      <div>
        {stepIndex > 0 && (
          <Button type="button" variant="ghost" onClick={back}>
            Back
          </Button>
        )}
      </div>
      <div className="flex items-center gap-2">
        {onSkip && (
          <Button type="button" variant="ghost" onClick={onSkip}>
            {skipLabel}
          </Button>
        )}
        <Button type="submit" variant="primary" disabled={nextDisabled || busy}>
          {busy ? 'Working…' : nextLabel}
        </Button>
      </div>
    </div>
  );
}
