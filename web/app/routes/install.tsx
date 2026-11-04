import { useQuery } from '@tanstack/react-query';
import { Link } from 'react-router';
import { api, type Session } from '../lib/api';

export function meta() {
  return [
    { title: 'Install Alondra on your Mac' },
    { name: 'description', content: 'Install the Alondra daemon, CLI and terminal app with Homebrew on an Apple Silicon Mac, then pair the iPhone or Mac app.' },
    { tagName: 'link', rel: 'canonical', href: 'https://getalondra.com/install' },
  ];
}

export default function Install() {
  const session = useQuery({ queryKey: ['session'], queryFn: ({ signal }) => api<Session>('/v1/session', { signal }) });
  const testflightUrl = session.data?.account.status === 'active' ? session.data.testflight_url : undefined;
  return <article className="doc wrap">
    <header className="doc-head">
      <h1>Install Alondra</h1>
      <p className="doc-lede">Homebrew installs the daemon and the terminal app. The apps come after.</p>
    </header>

    <section aria-labelledby="needs-title">
      <h2 id="needs-title">What you need</h2>
      <ul className="plain">
        <li>A Mac with Apple Silicon and macOS 26 or later.</li>
        <li>Claude Code (version 2.1.287 or later), Codex or both, installed and signed in. Alondra uses the login that those tools already hold and never asks you for it.</li>
        <li>Homebrew.</li>
      </ul>
    </section>

    <section aria-labelledby="mac-title">
      <h2 id="mac-title">On your Mac</h2>
      <pre className="command"><code>brew install bitomule/tap/alondra{'\n'}alondra</code></pre>
      <p>The first time, <code>alondra</code> asks for the folder with your projects, starts the background service and registers its Claude Code plugin, which loads only in the sessions Alondra starts. Run it again whenever you like: it only changes what is missing. Alondra brings Node and the Agent SDK it needs, and Homebrew installs tuicr, which the terminal app uses to show diffs.</p>
    </section>

    <section aria-labelledby="apps-title">
      <h2 id="apps-title">The iPhone and Mac apps</h2>
      <p>The apps are in alpha and are distributed through TestFlight. They come with access to Alondra Cloud, which opens little by little: <Link className="text-link" to="/#cloud">join the waitlist</Link> and you will get an invitation.</p>
      <p>To pair an app, open it on the same Wi-Fi as your Mac. It finds the Mac, both screens show the same code, and you allow it in the Mac app, in the terminal app or from the notification. Away from that network, run <code>alondra device invite</code> on the Mac and scan the QR code it prints.</p>
      {testflightUrl && <p><a className="button" href={testflightUrl} rel="noopener noreferrer">Get the app on TestFlight</a></p>}
    </section>

    <section aria-labelledby="fail-title">
      <h2 id="fail-title">If something fails</h2>
      <pre className="command"><code>alondra doctor{'\n'}alondra doctor --fix</code></pre>
      <p><code>alondra doctor</code> tells you what works and what is missing, with the command that fixes each thing. With <code>--fix</code> it repairs what it can. <code>alondra update</code> installs the newest release. The daemon writes its log to <code>~/.alondra/alondrad.log</code>.</p>
    </section>

    <p>Building from source and signing the apps with your own Apple developer account are covered in the <Link className="text-link" to="/docs">documentation</Link>.</p>
  </article>;
}
