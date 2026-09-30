// The shell's well-known paths. Features may register routes at these paths to replace the
// shell's placeholders; links anywhere in the app can rely on them.

const seg = encodeURIComponent;

export const paths = {
  workspace: (ws: string) => `/w/${seg(ws)}`,
  /** A path under the workspace: `under(ws, 'inbox')`. */
  under: (ws: string, path: string) => `/w/${seg(ws)}/${path.replace(/^\/+/, '')}`,
  home: (ws: string) => `/w/${seg(ws)}/home`,
  inbox: (ws: string) => `/w/${seg(ws)}/inbox`,
  myTasks: (ws: string) => `/w/${seg(ws)}/my-tasks`,
  project: (ws: string, project: string) => `/w/${seg(ws)}/projects/${seg(project)}`,
  workstream: (ws: string, project: string, workstream: string) =>
    `/w/${seg(ws)}/projects/${seg(project)}/workstreams/${seg(workstream)}`,
  /** By key (`PAP-4`) or id. */
  task: (ws: string, task: string) => `/w/${seg(ws)}/tasks/${seg(task)}`,
  console: (ws: string) => `/w/${seg(ws)}/console`,
  session: (ws: string, session: string) => `/w/${seg(ws)}/console/${seg(session)}`,
};
