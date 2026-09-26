# Explicit offline demo

AlondraDemoWorkspace depends on DTOs and feature contracts, never APIClient,
Auth, pairing, notification services or production composition. DemoWorkspace
stores its fixture, attachments and mutations in memory and invalidates every
captured dependency on close. Reset creates a new identity and actor.

Root hooks (same on iOS and macOS):

1. Add the Demo library dependency; it also requires the new Attachments feature
   already integrated by root in c1bef56.
2. Own a scene-local StoreOf<DemoModeFeature> at the launch presentation boundary.
   Show DemoEntryButton(store: demoMode) in the signed-out onboarding state. Do
   not launch login, restore a pairing, or allocate CloudAppServices for the demo.
   Do not offer entry during an in-flight authentication/pairing operation.
3. When demoMode.sessionID is nonnil, show DemoWorkspaceScreen(mode: demoMode)
   with the existing workspace container, passing the supplied ServiceRootFeature
   store and DemoAccountView as its account content. Refactor only the container
   account slot; reuse ServiceTriage/Tasks/Fleet/NewRequest/PullRequest views.
   No production owner/account callback belongs inside this content closure.
4. DemoWorkspaceScreen provides a persistent sample-data label, reset and exit.
   It closes the session on reset/exit/disappearance and prevents external URL
   opens inside the demo, including URLs entered in synthetic messages.

Synthetic mutations emit updated Overviews: answer/read/review, PR merge or
feedback, task cancel/reopen/archive, node message/close, new task and schedule
operations. Attachments are stored only in memory by stable draft UUID. No real
GitHub PR is opened, merged or messaged. The service identity uses demo://.

The synthetic workspace has no persistent database, UserDefaults, Keychain,
notification calls, HTTP client, sockets, or timers. Explicit file/photo selection
uses the shared local importer through Data/Attachments; uploaded bytes stay in
the demo actor and are never sent to the service. All feature dependencies are explicitly replaced,
including attachment uploads and command recovery. The integration tests check
an action's visible snapshot, isolation of simultaneous demo sessions, and the
rejection of stale work after reset/exit. They do not establish UI integration or
prove a real backend workflow.

Localizable labels to add to both app catalogs:
- Explore the demo / Explorar la demo
- Demo · Sample data / Demo · Datos de ejemplo
- Reset demo / Reiniciar demo
- Exit demo / Salir de la demo
- Opening demo… / Abriendo demo…
- Demo workspace / Espacio de ejemplo
- Everything here is sample data. Actions stay in memory and never reach a real account. Use Reset demo to start over or Exit demo to connect your workspace.
  / Todo lo que ves son datos de ejemplo. Las acciones se guardan en memoria y nunca llegan a una cuenta real. Usa Reiniciar demo para empezar de nuevo o Salir de la demo para conectar tu espacio de trabajo.
