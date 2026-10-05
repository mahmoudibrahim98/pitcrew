// @vitest-environment happy-dom
// @vitest-environment-options {"url":"http://localhost:5173/"}
import { fireEvent, screen, waitFor, within } from '@testing-library/react';
import { createMemoryHistory, RouterProvider } from '@tanstack/react-router';
import { afterEach, beforeEach, expect, it } from 'vitest';
import { createAppRouter } from '../shell/routes.tsx';
import { initialShellState, useShell } from '../shell/store.ts';
import { useAppearance } from './appearance.ts';
import { renderWithHub, startHub, stopHub, type Hub } from '../projects/tests/harness.tsx';
import './page.tsx';
let hub: Hub;
beforeEach(async () => { hub = await startHub(); useShell.setState(initialShellState); });
afterEach(async () => { await stopHub(hub); useAppearance.getState().setDensity('comfortable'); localStorage.clear(); });
function open(section: string) {
  const router=createAppRouter([], { history: createMemoryHistory({ initialEntries: [`/w/01JB000000000000000WSP0001/settings/${section}`] }) });
  return renderWithHub(<RouterProvider router={router} />, hub);
}
it('changes its profile and live footer, showing handle conflicts inline', async () => {
  const view=open('profile');
  const name=await screen.findByLabelText('Name');
  fireEvent.change(name,{target:{value:'Sam Updated'}});
  fireEvent.change(screen.getByLabelText('Avatar initials'),{target:{value:'SU'}});
  fireEvent.change(screen.getByLabelText('Avatar colour'),{target:{value:'#abcdef'}});
  fireEvent.click(screen.getByRole('button',{name:'Save changes'}));
  await screen.findByText('Saved.');
  await waitFor(()=>expect(screen.getByTestId('me').textContent).toBe('Sam Updated'));
  expect((await view.api.me()).avatar).toEqual({initials:'SU',colour:'#abcdef'});
  fireEvent.change(screen.getByLabelText('Handle'),{target:{value:'@writer'}});
  fireEvent.click(screen.getByRole('button',{name:'Save changes'}));
  await screen.findByRole('alert');
  expect((await view.api.me()).handle).toBe('@sam');
});
it('deep links to appearance and keeps the footer gear when collapsed', async () => {
  open('appearance');
  const density=await screen.findByLabelText('Density');
  fireEvent.change(density,{target:{value:'compact'}});
  await waitFor(()=>expect(document.documentElement.dataset.density).toBe('compact'));
  fireEvent.click(within(screen.getByRole('region',{name:'Appearance'})).getByRole('radio',{name:'Dark'}));
  await waitFor(()=>expect(document.documentElement.dataset.theme).toBe('dark'));
  fireEvent.click(screen.getByRole('button',{name:'Collapse sidebar'}));
  const footer=screen.getByRole('complementary',{name:'Sidebar'});
  expect(within(footer).getByRole('link',{name:'Settings'}).getAttribute('href')).toContain('settings/profile');
  expect(within(screen.getByRole('navigation',{name:'Settings sections'})).queryByRole('link',{name:'Updates'})).toBeNull();
  expect(screen.queryByRole('link',{name:'Notifications'})).toBeNull();
});
it('saves workspace naming and safety without restarting',async()=>{
  const view=open('workspace');
  fireEvent.change(await screen.findByLabelText('Workspace name'),{target:{value:'Updated workspace'}});
  fireEvent.click(screen.getByRole('button',{name:'Save changes'}));
  await screen.findByText('Saved.');
  expect((await view.api.workspace()).workspace.name).toBe('Updated workspace');
  fireEvent.click(within(screen.getByRole('navigation',{name:'Settings sections'})).getByRole('link',{name:'Safety'}));
  fireEvent.change(await screen.findByLabelText('Default permission mode'),{target:{value:'plan'}});
  fireEvent.click(screen.getByLabelText('Let PitCrew accept low-risk changes automatically'));
  fireEvent.change(screen.getByLabelText(/Up to this many/),{target:{value:'7'}});
  fireEvent.click(screen.getByRole('button',{name:'Save changes'}));
  await screen.findByText('Saved.');
  const safety=await view.api.request<{permission_mode:string;back_office_caps:{max_auto_accept_per_hour:number}}>('GET','/v1/safety');
  expect(safety.permission_mode).toBe('plan'); expect(safety.back_office_caps.max_auto_accept_per_hour).toBe(7);
});
it('reviews hook installation before confirmation and shows installed status',async()=>{
  open('hooks');
  const review=await screen.findByRole('button',{name:'Review and install'});
  expect(screen.queryByRole('button',{name:'Install reviewed hooks'})).toBeNull();
  fireEvent.click(review);
  const install=await screen.findByRole('button',{name:'Install reviewed hooks'});
  expect(screen.getAllByLabelText(/Diff:/).length).toBeGreaterThan(0);
  fireEvent.click(install);
  await screen.findByText('Hooks installed. Conflicting engines were skipped.');
  expect(screen.queryByRole('button',{name:'Install reviewed hooks'})).toBeNull();
});
it('edits default agents and offers section commands in the palette',async()=>{
  const view=open('agents');
  const fields=await screen.findAllByLabelText('Default agent name');
  fireEvent.change(fields[0]!,{target:{value:'Updated recipe'}});
  const form=fields[0]!.closest('form')!;
  fireEvent.click(within(form).getByRole('button',{name:'Save changes'}));
  await screen.findByText('Saved.');
  expect((await view.api.personas()).some(p=>p.name==='Updated recipe')).toBe(true);
  useShell.getState().setPaletteOpen(true);
  const palette=await screen.findByRole('dialog');
  fireEvent.change(within(palette).getByRole('combobox'),{target:{value:'Open settings: Appearance'}});
  fireEvent.keyDown(within(palette).getByRole('combobox'),{key:'Enter'});
  await screen.findByLabelText('Density');
});
