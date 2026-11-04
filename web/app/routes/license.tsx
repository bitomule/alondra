import { Link } from 'react-router';

export function meta() {
  return [
    { title: 'License: Alondra' },
    { name: 'description', content: 'Alondra is released under the Elastic License 2.0. What you may do with it and what you may not.' },
    { tagName: 'link', rel: 'canonical', href: 'https://getalondra.com/license' },
  ];
}

const REPO = 'https://github.com/bitomule/alondra';

export default function License() {
  return <article className="doc wrap">
    <header className="doc-head">
      <h1>License</h1>
      <p className="doc-lede">Source-available under the Elastic License 2.0: read it, run it, change it.</p>
    </header>

    <section aria-labelledby="may-title">
      <h2 id="may-title">What you may do</h2>
      <p>Alondra is released under the Elastic License 2.0 (ELv2). Use it, copy it, modify it and distribute it: for your own work, inside your company or for a client. Read it, change it and contribute to it.</p>
    </section>

    <section aria-labelledby="may-not-title">
      <h2 id="may-not-title">What you may not do</h2>
      <p>Offer Alondra to others as a hosted or managed service that gives them a substantial part of its features.</p>
    </section>

    <section aria-labelledby="note-title">
      <h2 id="note-title">What this license is not</h2>
      <p>ELv2 is source-available, and the Open Source Initiative has not approved it. It does not give you the Alondra name or the logo. The Claude Agent SDK, which Alondra’s build downloads and its Homebrew package bundles, belongs to Anthropic and has its own terms.</p>
    </section>

    <section aria-labelledby="ask-title">
      <h2 id="ask-title">Asking for something else</h2>
      <p>For a permission or a commercial license, write to <a className="text-link" href="mailto:licensing@getalondra.com">licensing@getalondra.com</a>.</p>
      <p>The full text is <a className="text-link" href={`${REPO}/blob/main/LICENSE.md`}>LICENSE.md</a>, and the notices for the software Alondra includes are in <a className="text-link" href={`${REPO}/blob/main/THIRD_PARTY_NOTICES.md`}>THIRD_PARTY_NOTICES.md</a>. Contributions come under the same license: see <Link className="text-link" to="/docs">the documentation</Link>.</p>
    </section>
  </article>;
}
