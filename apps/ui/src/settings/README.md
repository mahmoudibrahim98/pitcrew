# Settings

One section per deep link in both shell layouts. Profile and default-agent recipe edits append
the existing directory events, refreshing other connected views. Workspace naming is persisted
by the daemon before its live name changes; the UI explicitly refreshes workspace data after
success. Theme and density persist locally and apply immediately. Onboarding's density picker
uses the same preference.

Machines lists the desktop's connected workspaces, links to each machine's own Settings, and
reuses the removal confirmation. Machine checks and their install-page fixes, CLI accounts and
sign-in terminals use the existing onboarding components. Sign-in cleanup still stops a login
when the section closes. Hooks shows per-engine status and an explicit retained preview before
confirmation; conflicting configurations are skipped by the installer. Safety edits use the
existing workspace policy, affecting automatic acceptance immediately and new sessions' modes.
Default-agent recipes affect new sessions; running sessions keep their chosen settings.

Integrations composes the existing GitHub/Jira page. Shortcuts reads the shell's `SHORTCUTS`.
About reads actual desktop app version via lazy IPC, daemon/protocol versions and the daemon's
log destination. Browser About identifies the development build. Updates reuses the existing
desktop updater, preserving its install confirmation.

There is no Notifications section because the hub has no notification preferences. Native
notifications remain controlled by the tray. Hook removal is deferred until the CLI supports
retained uninstall previews. Errors stay inline and forms keep their input for retry.
