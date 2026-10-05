import { useEffect } from 'react';
import { create } from 'zustand';
import { Button } from './button.tsx';

export interface ToastMessage {
  id: number;
  text: string;
  error?: boolean;
  action?: { label: string; run(): unknown };
}
const useToasts = create<{ messages: ToastMessage[] }>(() => ({ messages: [] }));
let nextId = 0;
export function toast(text: string, options: Omit<ToastMessage, 'id' | 'text'> = {}) {
  const id = ++nextId;
  useToasts.setState((state) => ({ messages: [...state.messages.slice(-3), { id, text, ...options }] }));
}
function dismiss(id: number) {
  useToasts.setState((state) => ({ messages: state.messages.filter((message) => message.id !== id) }));
}
function ToastItem({ message }: { message: ToastMessage }) {
  useEffect(() => {
    const timer = setTimeout(() => dismiss(message.id), message.action === undefined ? 6000 : 20000);
    return () => clearTimeout(timer);
  }, [message]);
  return <li className="flex items-center gap-3 rounded-md border border-line bg-card p-3 text-sm shadow-pop">
    <span role={message.error ? 'alert' : 'status'}>{message.text}</span>
    {message.action !== undefined && <Button onClick={() => {
      Promise.resolve().then(() => message.action?.run()).then(() => dismiss(message.id)).catch((error: unknown) => {
        toast(`${error instanceof Error ? error.message : String(error)} Try again.`, { error: true });
      });
    }}>{message.action.label}</Button>}
    <Button variant="ghost" aria-label="Dismiss notification" onClick={() => dismiss(message.id)}>×</Button>
  </li>;
}
export function Toaster() {
  const messages = useToasts((state) => state.messages);
  return <ul aria-label="Notifications" aria-live="polite" className="pointer-events-auto fixed right-4 bottom-4 z-[100] flex max-w-[calc(100vw-2rem)] flex-col gap-2">
    {messages.map((message) => <ToastItem key={message.id} message={message} />)}
  </ul>;
}
