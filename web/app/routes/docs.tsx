import { Link } from 'react-router';

export function meta() {
  return [
    { title: 'Documentation: Alondra' },
    { name: 'description', content: 'Where to read how Alondra is built: the architecture, the iroh protocol between devices, how to build it from source and how to run its tests.' },
    { tagName: 'link', rel: 'canonical', href: 'https://getalondra.com/docs' },
  ];
}

const REPO = 'https://github.com/bitomule/alondra';
const DOCS = [
  { title: 'Architecture', body: 'How the daemon keeps its state, how the orchestrator and the nodes work, and how the screens read it.', href: `${REPO}/blob/main/docs/architecture/overview.md` },
  { title: 'The iroh protocol', body: 'How a device authenticates to a Mac, how requests travel as sealed envelopes and how the daemon reports direct and relayed connections.', href: `${REPO}/blob/main/docs/iroh-protocol.md` },
  { title: 'Installing', body: 'The install guide in the repository, with the apps and what to do when something fails.', href: `${REPO}/blob/main/INSTALL.md` },
  { title: 'Building from source', body: 'The Xcode and Bazel versions you need, and how to build the daemon and the apps with your own developer account.', href: `${REPO}/blob/main/docs/building-from-source.md` },
  { title: 'Running the tests', body: 'Tests run only through Bazel. This page lists the commands for each part of the repository.', href: `${REPO}/blob/main/docs/running-tests.md` },
  { title: 'Contributing', body: 'The terms for contributions, where pull requests go and where to report a crash.', href: `${REPO}/blob/main/CONTRIBUTING.md` },
];

export default function Docs() {
  return <article className="doc wrap">
    <header className="doc-head">
      <h1>Documentation</h1>
      <p className="doc-lede">Documentation lives in the repository, in English, next to the code.</p>
    </header>
    <ul className="rows">
      {DOCS.map(doc => <li key={doc.title}><h3><a className="text-link" href={doc.href}>{doc.title}</a></h3><p>{doc.body}</p></li>)}
    </ul>
    <section aria-labelledby="cli-title">
      <h2 id="cli-title">The command line</h2>
      <pre className="command"><code>alondra            # set up, or open the terminal app{'\n'}alondra doctor     # check what works{'\n'}alondra doctor --fix{'\n'}alondra update     # install the newest release{'\n'}alondra metrics    # how tasks went, by project or model</code></pre>
      <p>The overview on <Link className="text-link" to="/how-it-works">how Alondra works</Link> explains the pieces these commands talk to.</p>
    </section>
  </article>;
}
