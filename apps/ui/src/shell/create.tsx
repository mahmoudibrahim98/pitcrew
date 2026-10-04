// "+ New": a menu of the registered create items, each opening its dialog. The palette's "New …"
// commands open the same dialogs.

import { useParams } from '@tanstack/react-router';
import { Suspense, useId } from 'react';
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
import type { ResolvedCreate } from './registry.ts';
import { useShell } from './store.ts';

const TRIGGER_ID = 'shell-new';

/**
 * One "+ New" item. A `disabled` entry stays focusable, so its reason (an accessible description)
 * is reachable from the keyboard, but selecting it does nothing and the menu stays open.
 */
function NewMenuItem({ entry, onOpen }: { entry: ResolvedCreate; onOpen(id: string): void }) {
  const reasonId = useId();
  const disabled = entry.disabled !== undefined;
  return (
    <MenuItem
      aria-disabled={disabled || undefined}
      aria-describedby={disabled ? reasonId : undefined}
      className={disabled ? 'opacity-50' : undefined}
      onSelect={(event) => {
        if (disabled) {
          event.preventDefault();
          return;
        }
        onOpen(entry.id);
      }}
    >
      {entry.label}
      {disabled && (
        // aria-hidden keeps it out of the item's accessible *name* (computed from content); the
        // aria-describedby above still exposes it as the *description* once the item is focused.
        <span id={reasonId} aria-hidden className="sr-only">
          {entry.disabled}
        </span>
      )}
    </MenuItem>
  );
}

export function NewMenu() {
  const { project }: { project?: string } = useParams({ strict: false });
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
        {registry.create.filter((entry) => !entry.projectContext || project !== undefined).map((entry) => (
          <NewMenuItem
            key={entry.id}
            entry={entry}
            onOpen={(id) => setCreating(id, document.getElementById(TRIGGER_ID))}
          />
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
  // Defensive: the menu and the palette never open a disabled entry's dialog.
  if (entry === undefined || entry.disabled !== undefined) return null;
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
