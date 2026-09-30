// "+ New": a menu of the registered create items, each opening its dialog. The palette's "New …"
// commands open the same dialogs.

import { Suspense } from 'react';
import {
  Button,
  Dialog,
  DialogContent,
  Menu,
  MenuContent,
  MenuItem,
  MenuLabel,
  MenuTrigger,
  PlusIcon,
} from '../design/index.ts';
import { useRegistry } from './context.ts';
import { useShell } from './store.ts';

const TRIGGER_ID = 'shell-new';

export function NewMenu() {
  const registry = useRegistry();
  const setCreating = useShell((s) => s.setCreating);
  return (
    <Menu>
      <MenuTrigger asChild>
        <Button id={TRIGGER_ID} variant="primary">
          <PlusIcon />
          New
        </Button>
      </MenuTrigger>
      <MenuContent
        align="end"
        // The dialog an item opens takes focus; do not pull it back to the trigger.
        onCloseAutoFocus={(event) => {
          if (useShell.getState().creating !== null) event.preventDefault();
        }}
      >
        <MenuLabel>Create</MenuLabel>
        {registry.create.map((entry) => (
          <MenuItem key={entry.id} onSelect={() => setCreating(entry.id, document.getElementById(TRIGGER_ID))}>
            {entry.label}
          </MenuItem>
        ))}
      </MenuContent>
    </Menu>
  );
}

/** Where focus goes when a "+ New" dialog closes: what opened it, else the "+ New" button. */
function focusAfterCreate(from: Element | null): void {
  const opener = from instanceof HTMLElement && from.isConnected && from !== document.body ? from : null;
  (opener ?? document.getElementById(TRIGGER_ID))?.focus();
}

export function CreateDialog() {
  const registry = useRegistry();
  const creating = useShell((s) => s.creating);
  const from = useShell((s) => s.creatingFrom);
  const setCreating = useShell((s) => s.setCreating);
  const entry = registry.create.find((e) => e.id === creating);
  if (entry === undefined) return null;
  const Body = entry.dialog;
  const close = () => setCreating(null);
  return (
    <Dialog open onOpenChange={(open) => !open && close()}>
      <DialogContent
        title={entry.title ?? `New ${entry.label.toLowerCase()}`}
        // The dialog has no trigger element of its own, so it hands focus back itself.
        onCloseAutoFocus={(event) => {
          event.preventDefault();
          focusAfterCreate(from);
        }}
      >
        <Suspense fallback={<p className="px-4 py-4 text-sm text-ink-2">Loading…</p>}>
          <Body close={close} />
        </Suspense>
      </DialogContent>
    </Dialog>
  );
}
