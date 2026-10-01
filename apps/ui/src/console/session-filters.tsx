// Facet filters for the session list: machine, engine, state, project and workstream. Controlled:
// the caller keeps the value and passes it to `SessionList`.

import { useId, type ReactNode } from 'react';
import { useMachines, type Engine, type SessionState } from '../data/index.ts';
import { cx } from '../lib/cx.ts';
import { useConsoleSessions } from './data.ts';
import { hasFacets, NO_FACETS, placeOf, UNSORTED, type SessionFacets } from './facets.ts';
import { ENGINE_LABEL, ENGINES, LIVENESS, SESSION_STATES, STATE } from './format.ts';

type FacetName = keyof SessionFacets;

interface Option {
  value: string;
  label: ReactNode;
  count: number;
}

function toggle<T extends string>(values: readonly T[], value: T): T[] {
  return values.includes(value) ? values.filter((v) => v !== value) : [...values, value];
}

export interface SessionFiltersProps {
  value: SessionFacets;
  onChange: (value: SessionFacets) => void;
  className?: string;
}

export function SessionFilters({ value, onChange, className }: SessionFiltersProps) {
  const { all, places } = useConsoleSessions();
  const machines = useMachines();

  const counts = (name: FacetName) => {
    const map = new Map<string, number>();
    for (const session of all) {
      const { project, workstream } = placeOf(session, places);
      const key =
        name === 'project'
          ? (project?.id ?? UNSORTED)
          : name === 'workstream'
            ? workstream?.id
            : session[name];
      if (key !== undefined) map.set(key, (map.get(key) ?? 0) + 1);
    }
    return map;
  };

  const machineCounts = counts('machine');
  const engineCounts = counts('engine');
  const stateCounts = counts('state');
  const projectCounts = counts('project');
  const workstreamCounts = counts('workstream');

  const sections: { name: FacetName; title: string; options: Option[] }[] = [
    {
      name: 'machine',
      title: 'Machine',
      options: (machines.data ?? []).map((m) => ({
        value: m.id,
        label: (
          <span className="flex items-center gap-1.5">
            {m.name}
            {m.liveness !== 'live' && <span className="text-xs text-risk">{LIVENESS[m.liveness].label}</span>}
          </span>
        ),
        count: machineCounts.get(m.id) ?? 0,
      })),
    },
    {
      name: 'engine',
      title: 'Engine',
      options: ENGINES.map((e: Engine) => ({ value: e, label: ENGINE_LABEL[e], count: engineCounts.get(e) ?? 0 })),
    },
    {
      name: 'state',
      title: 'State',
      options: SESSION_STATES.map((s: SessionState) => ({
        value: s,
        label: STATE[s].label,
        count: stateCounts.get(s) ?? 0,
      })),
    },
    {
      name: 'project',
      title: 'Project',
      options: [
        ...places.projects.map((p) => ({ value: p.id, label: p.name, count: projectCounts.get(p.id) ?? 0 })),
        { value: UNSORTED, label: 'Unsorted', count: projectCounts.get(UNSORTED) ?? 0 },
      ],
    },
    {
      name: 'workstream',
      title: 'Workstream',
      options: places.workstreams
        .filter((w) => value.project.length === 0 || value.project.includes(w.project))
        .map((w) => ({ value: w.id, label: w.name, count: workstreamCounts.get(w.id) ?? 0 })),
    },
  ];

  return (
    <div className={cx('flex flex-col gap-4 p-3 text-sm', className)} role="group" aria-label="Session filters">
      <div className="flex items-center justify-between">
        <span className="text-xs font-semibold tracking-wide text-ink-2 uppercase">Filters</span>
        {hasFacets(value) && (
          <button
            type="button"
            className="rounded-sm px-1.5 text-xs text-accent-text hover:bg-hover"
            onClick={() => onChange(NO_FACETS)}
          >
            Clear
          </button>
        )}
      </div>
      {sections.map((section) => (
        <FacetSection
          key={section.name}
          title={section.title}
          options={section.options}
          selected={value[section.name]}
          onToggle={(option) =>
            onChange({ ...value, [section.name]: toggle<string>(value[section.name], option) } as SessionFacets)
          }
        />
      ))}
    </div>
  );
}

function FacetSection(props: {
  title: string;
  options: Option[];
  selected: readonly string[];
  onToggle: (value: string) => void;
}) {
  const id = useId();
  if (props.options.length === 0) return null;
  return (
    <fieldset aria-labelledby={id} className="flex flex-col gap-0.5">
      <legend id={id} className="mb-1 text-xs text-muted">
        {props.title}
      </legend>
      {props.options.map((option) => (
        <label
          key={option.value}
          className="flex cursor-pointer items-center gap-2 rounded-sm px-1.5 py-0.5 hover:bg-hover"
        >
          <input
            type="checkbox"
            checked={props.selected.includes(option.value)}
            onChange={() => props.onToggle(option.value)}
            className="accent-(--pc-accent)"
          />
          <span className="min-w-0 flex-1 truncate">{option.label}</span>
          <span className="text-xs text-muted tabular-nums">{option.count}</span>
        </label>
      ))}
    </fieldset>
  );
}
