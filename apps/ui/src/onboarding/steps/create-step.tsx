// Step 8: turn the scan's suggestions into real projects and workstreams. Tick what to keep,
// rename anything, move a workstream to a different (ticked) project, and pick a template.

import { useState } from 'react';
import type { ProjectSelection, ProjectTemplate } from '../api.ts';
import { useOnboardingApi } from '../api-context.tsx';
import { StepFooter } from '../step-footer.tsx';
import { useWizard } from '../wizard-context.tsx';
import type { CreateProjectDraft, CreateWorkstreamDraft } from '../wizard-state.ts';

const TEMPLATES: { value: ProjectTemplate; label: string }[] = [
  { value: 'research', label: 'Research' },
  { value: 'software', label: 'Software' },
  { value: 'blank', label: 'Blank' },
];

function toSelection(projects: CreateProjectDraft[], workstreams: CreateWorkstreamDraft[]): ProjectSelection[] {
  return projects
    .filter((p) => p.checked)
    .map((p) => ({
      suggestionId: p.suggestionId,
      name: p.name,
      template: p.template,
      workstreams: workstreams
        .filter((w) => w.checked && w.projectId === p.suggestionId)
        .map((w) => ({ suggestionId: w.suggestionId, name: w.name })),
    }));
}

export function CreateStep() {
  const { state, patch, next, skip } = useWizard();
  const api = useOnboardingApi();
  const [busy, setBusy] = useState(false);

  function updateProject(id: string, fields: Partial<CreateProjectDraft>) {
    patch({ createProjects: state.createProjects.map((p) => (p.suggestionId === id ? { ...p, ...fields } : p)) });
  }

  function updateWorkstream(id: string, fields: Partial<CreateWorkstreamDraft>) {
    patch({
      createWorkstreams: state.createWorkstreams.map((w) => (w.suggestionId === id ? { ...w, ...fields } : w)),
    });
  }

  async function submit() {
    const selection = toSelection(state.createProjects, state.createWorkstreams);
    if (selection.length === 0) {
      next();
      return;
    }
    setBusy(true);
    try {
      const result = await api.createFromScan(selection);
      patch({ createResult: result });
      next();
    } finally {
      setBusy(false);
    }
  }

  const checkedProjects = state.createProjects.filter((p) => p.checked);

  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      {state.createProjects.length === 0 && <p className="text-sm text-ink-2">No suggestions from the scan.</p>}

      <ul className="flex flex-col gap-3">
        {state.createProjects.map((project) => {
          const workstreams = state.createWorkstreams.filter((w) => w.projectId === project.suggestionId);
          // A workstream's dropdown always offers its current project, even if that project just
          // got unchecked, so the select's value is always one of its own options.
          const moveOptions = checkedProjects.some((p) => p.suggestionId === project.suggestionId)
            ? checkedProjects
            : [project, ...checkedProjects];
          return (
            <li key={project.suggestionId} className="rounded-sm border border-line p-3">
              <div className="flex items-center gap-2">
                <input
                  type="checkbox"
                  aria-label={`Include ${project.name}`}
                  checked={project.checked}
                  onChange={(e) => updateProject(project.suggestionId, { checked: e.target.checked })}
                  className="size-4"
                />
                <input
                  type="text"
                  aria-label="Project name"
                  value={project.name}
                  disabled={!project.checked}
                  onChange={(e) => updateProject(project.suggestionId, { name: e.target.value })}
                  className="h-7 flex-1 rounded-sm border border-line-2 bg-card px-2 text-sm text-ink outline-none focus-visible:border-accent disabled:opacity-50"
                />
                <select
                  aria-label={`Template for ${project.name}`}
                  value={project.template}
                  disabled={!project.checked}
                  onChange={(e) => updateProject(project.suggestionId, { template: e.target.value as ProjectTemplate })}
                  className="h-7 rounded-sm border border-line-2 bg-card px-1.5 text-sm text-ink disabled:opacity-50"
                >
                  {TEMPLATES.map((t) => (
                    <option key={t.value} value={t.value}>
                      {t.label}
                    </option>
                  ))}
                </select>
              </div>
              {workstreams.length > 0 && (
                <ul className="mt-2 flex flex-col gap-1.5 pl-6">
                  {workstreams.map((ws) => (
                    <li key={ws.suggestionId} className="flex items-center gap-2">
                      <input
                        type="checkbox"
                        aria-label={`Include ${ws.name}`}
                        checked={ws.checked}
                        onChange={(e) => updateWorkstream(ws.suggestionId, { checked: e.target.checked })}
                        className="size-3.5"
                      />
                      <input
                        type="text"
                        aria-label="Workstream name"
                        value={ws.name}
                        disabled={!ws.checked}
                        onChange={(e) => updateWorkstream(ws.suggestionId, { name: e.target.value })}
                        className="h-6 flex-1 rounded-sm border border-line-2 bg-card px-2 text-xs text-ink outline-none focus-visible:border-accent disabled:opacity-50"
                      />
                      <span className="text-xs text-ink-2">{ws.sessionCount} sessions</span>
                      <select
                        aria-label={`Move ${ws.name} to project`}
                        value={ws.projectId}
                        disabled={!ws.checked}
                        onChange={(e) => updateWorkstream(ws.suggestionId, { projectId: e.target.value })}
                        className="h-6 rounded-sm border border-line-2 bg-card px-1 text-xs text-ink disabled:opacity-50"
                      >
                        {moveOptions.map((p) => (
                          <option key={p.suggestionId} value={p.suggestionId}>
                            {p.name}
                          </option>
                        ))}
                      </select>
                    </li>
                  ))}
                </ul>
              )}
            </li>
          );
        })}
      </ul>

      <StepFooter nextLabel="Create" onSkip={skip} skipLabel="Don't create any yet" busy={busy} />
    </form>
  );
}
