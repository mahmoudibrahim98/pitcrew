// @vitest-environment happy-dom

// An Orchestrator answer is untrusted text: it renders as text, never as markup, and only the
// references the hub checked become links, to app routes.

import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { AnswerReference } from '../../data/index.ts';
import { Answer, blocks, inlines, referencePath } from '../answer.tsx';
import { duration, usageLine } from '../orchestrator-chat.tsx';

const WS = '01JB000000000000000WSP0001';
const SES = '01JB000000000000000SES0001';
const WST = '01JB000000000000000WST0001';
const PRJ = '01JB000000000000000PRJ0001';

const refs: AnswerReference[] = [
  { text: 'PAP-1', target: { kind: 'task', id: '01JB000000000000000TSK0001', key: 'PAP-1' }, label: 'PAP-1 Draft the method section' },
  { text: `ses_${SES}`, target: { kind: 'session', id: SES }, label: 'Draft method section' },
  { text: `recap:wst_${WST}@2026-09-30`, target: { kind: 'recap', project: PRJ, workstream: WST, date: '2026-09-30' }, label: 'Recap of Submission, 2026-09-30' },
];

afterEach(cleanup);

function show(text: string, open = vi.fn()) {
  const { container } = render(<Answer text={text} references={refs} ws={WS} open={open} />);
  return { container, open };
}

describe('an answer', () => {
  it('is text, never markup', () => {
    const hostile = '<img src=x onerror="alert(1)"> <script>alert(2)</script> <a href="javascript:alert(3)">x</a> &amp;';
    const { container } = show(hostile);
    expect(container.querySelector('img, script, a, iframe')).toBeNull();
    expect(container.textContent).toContain('<img src=x onerror="alert(1)">');
    expect(container.textContent).toContain('&amp;');
  });

  it('links the references the hub checked, as whole words, to app routes', () => {
    const { open } = show(`**PAP-1** moved in ses_${SES}: see recap:wst_${WST}@2026-09-30.\nPAP-10 and xPAP-1 are not PAP-1-2.`);
    const links = screen.getAllByRole('link');
    expect(links.map((a) => [a.textContent, a.getAttribute('href')])).toEqual([
      ['PAP-1', `/w/${WS}/tasks/PAP-1`],
      ['Draft method section', `/w/${WS}/console/${SES}`],
      ['Recap of Submission, 2026-09-30', `/w/${WS}/projects/${PRJ}/workstreams/${WST}`],
    ]);
    fireEvent.click(screen.getByRole('link', { name: 'Draft method section' }));
    expect(open).toHaveBeenCalledWith(`/w/${WS}/console/${SES}`);
  });

  it('keeps every other link as text, and never opens a URL from an answer', () => {
    show('[the method](PAP-1) and [a page](https://example.invalid/x) and [run](javascript:alert(1))');
    const links = screen.getAllByRole('link');
    expect(links.map((a) => a.textContent)).toEqual(['the method']);
    expect(document.body.textContent).toContain('a page (https://example.invalid/x)');
    expect(document.body.textContent).toContain('run (javascript:alert(1)');
  });

  it('renders a small, safe subset: lists, code, headings', () => {
    expect(blocks('# Today\n- one\n- `PAP-1`\n\n```\n<b>x</b>\n```\n1. a', refs).map((b) => b.t)).toEqual([
      'heading',
      'list',
      'code',
      'list',
    ]);
    expect(inlines('`PAP-1`', refs)).toEqual([{ t: 'ref', ref: refs[0] }]);
    const { container } = show('```\n<b>bold?</b>\n```');
    expect(container.querySelector('b')).toBeNull();
    expect(container.querySelector('pre')?.textContent).toBe('<b>bold?</b>');
  });

  it('opens a recap without a workstream at its project', () => {
    expect(referencePath(WS, { kind: 'recap', project: PRJ })).toBe(`/w/${WS}/projects/${PRJ}`);
  });
});

describe('what an answer took', () => {
  it('says it briefly, or why it ended', () => {
    const turn = { question: 'Q', asked: 0, session: SES, answer: 'A', references: [], suggestions: [] };
    expect(usageLine({ ...turn, state: 'answering' })).toBeUndefined();
    expect(usageLine({ ...turn, state: 'answered', usage: { duration_ms: 12_400, tool_runs: 1, answer_bytes: 2048 } })).toBe(
      'Answered in 12 s · 1 tool run · 2.0 KB',
    );
    expect(usageLine({ ...turn, state: 'timed_out', usage: { duration_ms: 300_000, tool_runs: 9, answer_bytes: 10 } })).toBe(
      'Timed out after 5 min · 9 tool runs · 10 B',
    );
    expect(usageLine({ ...turn, state: 'failed' })).toBe('No answer');
    expect(duration(2 * 3600 * 1000)).toBe('2 h');
  });
});
