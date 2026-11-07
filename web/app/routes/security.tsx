import { Link } from 'react-router';

export function meta() {
  return [
    { title: 'Security and privacy: Alondra' },
    { name: 'description', content: 'What stays on your Mac, what Alondra Cloud sees, how devices are paired and encrypted, and how to report a vulnerability.' },
    { tagName: 'link', rel: 'canonical', href: 'https://getalondra.com/security' },
  ];
}

export default function Security() {
  return <article className="doc wrap">
    <header className="doc-head">
      <h1>Security and privacy</h1>
      <p className="doc-lede">Alondra Cloud connects your devices. It does not read your work.</p>
    </header>

    <section aria-labelledby="stays-title">
      <h2 id="stays-title">What Alondra Cloud never sees</h2>
      <ul className="plain">
        <li>Your code, your repositories and your worktrees.</li>
        <li>The contents of tasks and cards: plans, diffs, transcripts and attachments. They travel between your own devices as sealed envelopes that the relay carries and cannot open.</li>
        <li>Your Claude and Codex credentials. Alondra never signs in to a provider: you run <code>claude</code> or <code>codex</code> on your Mac, and the sessions Alondra starts use that login.</li>
      </ul>
      <p>Your code goes to the providers you already use, Anthropic or OpenAI, and to GitHub, exactly as if you ran those tools yourself. It never goes to Alondra Cloud.</p>
    </section>

    <section aria-labelledby="sees-title">
      <h2 id="sees-title">What Alondra Cloud sees</h2>
      <ul className="plain">
        <li>Your account: the sign-in identity, your email, the name you chose, and a log of changes to your account and invitations.</li>
        <li>The list of your devices: device and Mac identifiers, the names you give them, their public keys, the endpoint identifiers they use to connect, and the devices you revoked.</li>
        <li>Push notification tokens, and the push messages sealed for each device. A device that cannot open one shows a generic text instead.</li>
        <li>For the iPhone, the latest sealed counts of cards and tasks of each Mac, and three flags beside them in the clear: whether something waits or works on that Mac, whether the phone is outside its quiet hours, and whether the inbox grew.</li>
        <li>Your web session while you are signed in to this site, with its sign-in tokens sealed.</li>
        <li>What any relay sees when it carries a connection: the network addresses of both ends, when they talk and how much they send. It cannot read what they send.</li>
      </ul>
      <p>Apple delivers the push notifications, so Apple sees that a notification was sent to a device and when. Alert and background pushes also carry, in the clear, the identifiers of the Mac and the device, the request identifier, the kind of message and its expiry. Pushes that update the status shown on the phone carry those headers for up to four Macs, whether the update starts, changes or ends that status, and the same three flags listed above. The content stays sealed.</p>
    </section>

    <section aria-labelledby="pairing-title">
      <h2 id="pairing-title">Pairing and encryption</h2>
      <p>A new device is paired in person, and the Mac has to approve it. On the same Wi-Fi both screens show the same six-character code. Elsewhere the device scans a QR code that <code>alondra device invite</code> prints on the Mac, and you type the code the device shows into the Mac. Signing in to an account does not give a device access to a Mac.</p>
      <p>Requests, answers and push payloads are sealed with a fresh P-256 key agreed with the receiver’s key, HKDF-SHA256 and ChaCha20-Poly1305, and every envelope is signed with ECDSA over P-256. The Mac keeps a journal of what it has received, so a replayed request is answered from the journal and never runs twice. The receiver’s key is long-term, so a stolen device key exposes everything that was sealed to it.</p>
      <p>If you lose a device, revoke it in two places: in the Mac app’s Settings, which ends its access to that Mac on every network, and on your account page, which ends its access through the relay. Revoked only from your account, it can still reach the Mac directly, on your Wi-Fi or over a direct path.</p>
      <p>The details are in the <Link className="text-link" to="/docs">protocol documentation</Link>, and you can read the code.</p>
    </section>

    <section aria-labelledby="signin-title">
      <h2 id="signin-title">Signing in and the waitlist</h2>
      <p>Alondra Cloud sign-in uses OpenID Connect. Access is by invitation. If you join the waitlist, the service stores your email so that it can invite you. That email is not part of the sealed traffic between your devices, and no confirmation message is sent. An administrator can invite you or remove your access. They cannot read your tasks or recover the keys of your devices.</p>
      <p>Revoking access to the service does not delete files on your Mac, and it cannot take back copies that your other devices already hold.</p>
    </section>

    <section aria-labelledby="report-title">
      <h2 id="report-title">Reporting a vulnerability</h2>
      <p>Write to <a className="text-link" href="mailto:security@getalondra.com">security@getalondra.com</a> or open a private report on GitHub. Please do not open a public issue for a security problem.</p>
    </section>
  </article>;
}
