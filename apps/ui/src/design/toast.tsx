// Notifications: `toast(text, …)` posts one; one `Toaster` in the workspace frame shows them.
//
// A modal side drawer traps focus and hides the rest of the page from assistive technology, so
// while one is open the notifications render inside it (the `ToastHost` that `SideDrawer`
// mounts): they stay reachable with Tab and F8, are read out, and clicking one never dismisses
// the drawer.

import { useEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { create } from 'zustand';
import { Button } from './button.tsx';

export interface ToastMessage {
  id: number;
  text: string;
  error?: boolean;
  /**
   * What the notification is about: a new one with the same key replaces it, and
   * `dismissToast(key)` takes it away (an archive's Undo once the task is restored).
   */
  key?: string;
  action?: { label: string; run(): unknown };
}

const useToasts = create<{ messages: ToastMessage[] }>(() => ({ messages: [] }));
/** Elements notifications render into while a modal surface is open; the newest wins. */
const useHosts = create<{ hosts: HTMLElement[] }>(() => ({ hosts: [] }));
let nextId = 0;

export function toast(text: string, options: Omit<ToastMessage, 'id' | 'text'> = {}): number {
  const id = ++nextId;
  useToasts.setState((state) => ({
    messages: [
      ...state.messages.filter((message) => options.key === undefined || message.key !== options.key).slice(-3),
      { id, text, ...options },
    ],
  }));
  return id;
}

/** Takes a notification away, by its id or its `key`. */
export function dismissToast(which: number | string) {
  useToasts.setState((state) => ({
    messages: state.messages.filter((message) => (typeof which === 'number' ? message.id : message.key) !== which),
  }));
}

/** Takes every notification away (tests start from none). */
export function clearToasts() {
  useToasts.setState({ messages: [] });
}

/** Notifications show here, instead of in the frame, while this is mounted (inside a modal). */
export function ToastHost() {
  const [element, setElement] = useState<HTMLDivElement | null>(null);
  useEffect(() => {
    if (element === null) return;
    useHosts.setState((state) => ({ hosts: [...state.hosts, element] }));
    return () => useHosts.setState((state) => ({ hosts: state.hosts.filter((host) => host !== element) }));
  }, [element]);
  return <div ref={setElement} data-toast-host="" />;
}

function ToastItem({ message, paused }: { message: ToastMessage; paused: boolean }) {
  const [running, setRunning] = useState(false);
  useEffect(() => {
    if (paused || running) return;
    const timer = setTimeout(() => dismissToast(message.id), message.action === undefined ? 6000 : 20000);
    return () => clearTimeout(timer);
  }, [message, paused, running]);
  const action = message.action;
  // One run at a time: the button is disabled until the action settles. On success the
  // notification goes; on failure it stays, so the action can be tried again.
  const run = () => {
    if (action === undefined || running) return;
    setRunning(true);
    Promise.resolve()
      .then(() => action.run())
      .then(() => dismissToast(message.id))
      .catch((error: unknown) => {
        setRunning(false);
        toast(`${error instanceof Error ? error.message : String(error)} Try again.`, { error: true });
      });
  };
  return (
    <li className="flex items-center gap-3 rounded-md border border-line bg-card p-3 text-sm shadow-pop">
      <span role={message.error ? 'alert' : 'status'}>{message.text}</span>
      {action !== undefined && (
        <Button disabled={running} aria-busy={running || undefined} onClick={run}>
          {action.label}
        </Button>
      )}
      <Button variant="ghost" aria-label="Dismiss notification" onClick={() => dismissToast(message.id)}>
        ×
      </Button>
    </li>
  );
}

export function Toaster() {
  const messages = useToasts((state) => state.messages);
  const host = useHosts((state) => state.hosts.at(-1));
  const list = useRef<HTMLUListElement>(null);
  const [hovered, setHovered] = useState(false);
  const [focused, setFocused] = useState(false);
  const any = messages.length > 0;
  // F8 moves focus to the notifications.
  useEffect(() => {
    if (!any) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== 'F8' || event.altKey || event.ctrlKey || event.metaKey) return;
      event.preventDefault();
      list.current?.focus();
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [any]);
  const content = (
    <ul
      ref={list}
      tabIndex={-1}
      aria-label="Notifications"
      aria-live="polite"
      data-toaster=""
      // Timers wait while someone points at or focuses a notification.
      onPointerEnter={() => setHovered(true)}
      onPointerLeave={() => setHovered(false)}
      onFocus={() => setFocused(true)}
      onBlur={(event) => {
        if (!event.currentTarget.contains(event.relatedTarget)) setFocused(false);
      }}
      className="pointer-events-auto fixed right-4 bottom-4 z-[100] flex max-w-[calc(100vw-2rem)] flex-col gap-2 outline-none"
    >
      {messages.map((message) => (
        <ToastItem key={message.id} message={message} paused={hovered || focused} />
      ))}
    </ul>
  );
  return host === undefined ? content : createPortal(content, host);
}
