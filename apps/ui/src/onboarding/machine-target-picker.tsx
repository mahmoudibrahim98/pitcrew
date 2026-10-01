// Picks a machine: this computer, a WSL distro, or an SSH host from the list `discoverHosts`
// offers (never a free-typed host: ADR-0009 deploys over the user's own SSH config). Shared by the
// first-run workspace step and the add-a-machine wizard.

import { RadioGroup } from 'radix-ui';
import { useEffect, useId, useState } from 'react';
import { cx } from '../lib/cx.ts';
import type { DiscoveredHost, MachineTarget } from './api.ts';
import { useOnboardingApi } from './api-context.tsx';

export function MachineTargetPicker({
  value,
  onChange,
  label = 'Machine',
}: {
  value: MachineTarget;
  onChange: (target: MachineTarget) => void;
  label?: string;
}) {
  const api = useOnboardingApi();
  const [hosts, setHosts] = useState<DiscoveredHost[] | null>(null);
  const groupId = useId();

  useEffect(() => {
    let live = true;
    void api.discoverHosts().then((found) => {
      if (live) setHosts(found);
    });
    return () => {
      live = false;
    };
  }, [api]);

  const wslHosts = (hosts ?? []).filter((h) => h.kind === 'wsl');
  const sshHosts = (hosts ?? []).filter((h) => h.kind === 'ssh');
  const radioValue = value.kind === 'local' ? 'local' : value.kind === 'wsl' ? `wsl:${value.distro}` : `ssh:${value.host}`;

  return (
    <fieldset className="flex flex-col gap-2">
      <legend className="text-sm font-medium text-ink">{label}</legend>
      <RadioGroup.Root
        value={radioValue}
        onValueChange={(next) => {
          if (next === 'local') {
            onChange({ kind: 'local' });
            return;
          }
          const [kind, ...rest] = next.split(':');
          const id = rest.join(':');
          if (kind === 'wsl') onChange({ kind: 'wsl', distro: id });
          else if (kind === 'ssh') onChange({ kind: 'ssh', host: id });
        }}
        className="flex flex-col gap-1.5"
      >
        <Option id={`${groupId}-local`} value="local" current={radioValue} title="This computer" />
        {wslHosts.map((h) => (
          <Option
            key={`wsl:${h.id}`}
            id={`${groupId}-wsl-${h.id}`}
            value={`wsl:${h.id}`}
            current={radioValue}
            title={`WSL: ${h.id}`}
            detail={h.detail}
          />
        ))}
        {sshHosts.map((h) => (
          <Option
            key={`ssh:${h.id}`}
            id={`${groupId}-ssh-${h.id}`}
            value={`ssh:${h.id}`}
            current={radioValue}
            title={h.id}
            detail={h.detail}
          />
        ))}
      </RadioGroup.Root>
      {hosts === null && <p className="text-xs text-ink-2">Looking for WSL distros and SSH hosts…</p>}
      {hosts !== null && wslHosts.length === 0 && sshHosts.length === 0 && (
        <p className="text-xs text-ink-2">No other machines found yet; you can add one later.</p>
      )}
    </fieldset>
  );
}

function Option({
  id,
  value,
  current,
  title,
  detail,
}: {
  id: string;
  value: string;
  current: string;
  title: string;
  detail?: string | undefined;
}) {
  return (
    <label
      htmlFor={id}
      className={cx(
        'flex cursor-pointer items-center gap-2 rounded-sm border px-3 py-2 text-sm',
        current === value ? 'border-accent bg-accent-soft text-accent-text' : 'border-line hover:bg-hover',
      )}
    >
      <RadioGroup.Item id={id} value={value} className="size-3.5 shrink-0 rounded-pill border border-line-2">
        <RadioGroup.Indicator className="block size-full scale-50 rounded-pill bg-accent" />
      </RadioGroup.Item>
      <span className="flex-1">{title}</span>
      {detail !== undefined && <span className="text-xs text-ink-2">{detail}</span>}
    </label>
  );
}
