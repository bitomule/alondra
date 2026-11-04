import { Link } from 'react-router';
import { Capture } from '../public/Capture';
import { Diagram } from '../public/Diagram';
import { FlowDiagram } from '../public/FlowDiagram';

export function meta() {
  return [
    { title: 'How Alondra works: orchestrator, cards and architecture' },
    { name: 'description', content: 'How a request becomes tasks, how cards reach you, and how your iPhone talks to your Mac: the daemon, the control plane, the relay and the end-to-end encryption between devices.' },
    { tagName: 'link', rel: 'canonical', href: 'https://getalondra.com/how-it-works' },
  ];
}

function Soon({ children }: { children: React.ReactNode }) {
  return <p className="soon-note"><span className="soon">Coming soon</span>{children}</p>;
}

const SECTIONS = [
  ['orchestrator', 'Requests become tasks'],
  ['inbox', 'One decision per card'],
  ['review', 'Reviewing a pull request'],
  ['architecture', 'The architecture'],
  ['remote', 'Reaching your Mac'],
  ['encryption', 'Encryption'],
  ['macs', 'More than one Mac'],
  ['models', 'Models and effort'],
  ['metrics', 'Metrics'],
  ['mcp', 'MCP servers'],
] as const;

export default function HowItWorks() {
  return <article className="doc doc-toc wrap">
    <header className="doc-head">
      <h1>How Alondra works</h1>
      <p className="doc-lede">The daemon does the work. Everything else is how you reach it.</p>
    </header>
    <nav className="toc" aria-label="On this page">
      <p className="toc-title">On this page</p>
      <ol>{SECTIONS.map(([id, label]) => <li key={id}><a href={`#${id}`}>{label}</a></li>)}</ol>
    </nav>

    <section id="orchestrator" aria-labelledby="orchestrator-title">
      <h2 id="orchestrator-title">The orchestrator turns a request into tasks</h2>
      <p>You write a request in the terminal app, the Mac app or the iPhone app. The orchestrator is an agent session on one of your accounts, so it uses your tokens like any other session. Alondra starts it when there is work and stops it when there is not. It reads the request, writes a plan and splits it into tasks. When the work calls for it, the plan comes back to you as a card to approve before any code changes.</p>
      <FlowDiagram/>
      <h3>Tasks wait for each other</h3>
      <p>A task can depend on others. It starts once they have landed, and a pull request built on top of another task’s pull request is merged after it. Tasks with nothing between them run at the same time.</p>
      <p>Before it starts a node, Alondra looks at the memory, the processor and the disk of your Mac, and at how many nodes may work at once: a number you set in Settings, or one Alondra works out from your Mac’s cores and what its nodes take. When there is no room, the task waits and says why. Memory and processor load only hold new starts: a running node is never paused or stopped for them. When the disk runs low, a node’s next turn waits until there is room.</p>
      <h3>A model and an account for each task</h3>
      <p>Each task gets a node: an agent session, Claude Code or Codex with your own login, running in a git worktree that Alondra creates for that task. The orchestrator picks its model and its reasoning effort, as <a className="text-link" href="#models">Which model, and how much effort</a> explains, and Alondra runs it on one of your accounts.</p>
      <h3>Decisions reach you, conversations do not</h3>
      <p>You never read the agents’ conversations. A task shows what its agent is doing right now and the last thing it said. Whatever needs you, a question, a plan, a permission or a finished pull request, arrives as a card in the inbox.</p>
      <h3>Deciding and doing stay apart</h3>
      <p>You and the orchestrator decide what gets built: only the orchestrator creates tasks and sets what each one waits for. A node only does its own task. When it sees something outside it, it suggests it to the orchestrator, which folds it into the task, turns it down with a reason or makes it a new task.</p>
      <p>When you accept a delivery, Alondra merges the pull request and removes the worktree. You can also let it merge by itself once the checks pass.</p>
      <Capture name="tui-tasks" className="doc-capture" alt="The terminal app: the request “Ship Lumen 2.4” split into five tasks. One needs you with a question, two pull requests are ready for review, one task runs in the background and one waits for it."/>
    </section>

    <section id="inbox" aria-labelledby="inbox-title">
      <h2 id="inbox-title">The inbox holds one decision per card</h2>
      <p>Everything that needs you arrives as a card: a question with its answers, a plan or a document to review, a permission, a pull request, a request for a file or a value, a notice. A card is one thing and carries what you need to decide it, so a plan to approve is never mixed with a question about its details.</p>
      <p>You see the same cards in the terminal app, the Mac app and the iPhone app, and an answer from any of them reaches the node that asked. The iPhone app notifies you, and the first answers of a question show on its notification. Using them needs the phone unlocked.</p>
      <p>Cards can carry images and videos. A video shows as a tile with its size and downloads only when you tap it.</p>
      <Capture name="iphone-card" className="doc-capture doc-capture-phone" alt="The iPhone app: a question from an agent as a card, “How often should Harbor refresh the order list?”, with two answers as buttons and a field to reply in words."/>
    </section>

    <section id="review" aria-labelledby="review-title">
      <h2 id="review-title">Reviewing a pull request</h2>
      <p>A finished task arrives as a pull request card with how its checks went. In the iPhone and Mac apps it opens the changes file by file, and you tap or click a line to comment on it. In the terminal app the diff opens in tuicr, which Homebrew installs with Alondra.</p>
      <p>Accept it and Alondra merges the pull request. Request changes and the node gets your line comments and your note in one message, and carries on in the same worktree. If the pull request moves on before you send them, the comments on old lines are dropped.</p>
      <Capture name="mac-review" className="doc-capture" alt="The Mac app showing the changes of a pull request: two added files, each with its lines marked as added."/>
    </section>

    <section id="architecture" aria-labelledby="architecture-title">
      <h2 id="architecture-title">The architecture</h2>
      <p>The daemon, <code>alondrad</code>, runs on your Mac. It keeps the state, runs the orchestrator and the nodes, and serves your devices. The terminal app, the iPhone app and the Mac app are its clients.</p>
      <Diagram/>
      <p>Alondra Cloud is a server with two jobs. The control plane handles accounts, pairing, the list of your devices and push notifications. The relay passes encrypted traffic between your devices when they cannot connect directly. The server does not run agents and holds no tasks, cards, diffs or transcripts: the routes that once carried content now answer that they have been removed.</p>
    </section>

    <section id="remote" aria-labelledby="remote-title">
      <h2 id="remote-title">Reaching your Mac from anywhere</h2>
      <p>On the same Wi-Fi, a device finds your Mac with Bonjour and talks to it over the local network. Anywhere else a connection starts through the Alondra Cloud relay and moves to a direct connection as soon as one opens. You pair a device once, in person, and after that you install nothing on the network: no Tailscale, no VPN, no port forwarding to set up.</p>
      <p>A direct path does not always open. A phone on mobile data usually sits behind a router that blocks it, and then a direct path opens only if your Mac’s network allows it: a router port mapping, a forwarded port or public IPv6. Otherwise the relay carries the session. It works the same, with a little more latency. When we measured it from a phone on mobile data, 0 of 30 connections went direct, so away from home expect the relay. Settings in the iPhone and Mac apps show whether each Mac is reached directly or through the relay, and why a direct path does not open; in the terminal, <code>alondra doctor</code> says why.</p>
      <p>On the same Wi-Fi your iPhone connects straight to your Mac and needs no account. Without one it gets no notifications.</p>
    </section>

    <section id="encryption" aria-labelledby="encryption-title">
      <h2 id="encryption-title">Sealed between your devices</h2>
      <p>A request or an answer is sealed on the device that sends it and opened only by the device that receives it, on the local network and through the relay alike. Each envelope uses a fresh P-256 key agreed with the receiver’s key, HKDF-SHA256 and ChaCha20-Poly1305, and the sender signs it with ECDSA over P-256. Push notifications are sealed the same way for each device. The relay moves envelopes it cannot open.</p>
      <p>Pairing happens in person, and your Mac has to approve it. On the same Wi-Fi your iPhone and your Mac each show the same six-character code, and you approve it in the Mac app, the terminal app or the notification. Elsewhere you run <code>alondra device invite</code> on the Mac, scan the QR code it prints, and type the code your iPhone shows back on the Mac. An Alondra Cloud account alone cannot read what your Mac holds.</p>
      <p>What the server does see, and what it never receives, is on the <Link className="text-link" to="/security">security and privacy page</Link>.</p>
    </section>

    <section id="macs" aria-labelledby="macs-title">
      <h2 id="macs-title">More than one Mac</h2>
      <p>Pair the iPhone app, the Mac app and the terminal app with each of your Macs and switch between them; in the terminal app the <code>]</code> key moves to the next Mac. While the iPhone app is open it keeps a connection to every paired Mac, so switching does not wait for one.</p>
      <Soon>Showing the inbox and the tasks of several Macs together on one screen.</Soon>
    </section>

    <section id="models" aria-labelledby="models-title">
      <h2 id="models-title">Which model, and how much effort</h2>
      <p>Alondra runs Claude Code and Codex, alone or together, with several accounts of each. For each task the orchestrator picks a model and a reasoning effort from what your connected accounts offer. It takes the strongest model for risky work such as concurrency, security or data migrations, and otherwise the cheapest one that fits. It follows written rules over a table of the models your accounts offer: each model’s capability sheet and, once a model has finished enough of your tasks, how they went, such as how many were done the first time and what they cost on average. It records its reason.</p>
      <p>Alondra then runs the task on one of your accounts that offers that model, in the order you set, and passes over any whose plan has run out. You can pin a task to one account in its form.</p>
      <p>You can set the model and the effort yourself in the apps and in the terminal. A choice you make wins.</p>
      <Soon>A decider that learns from the results of past tasks and adjusts its choices.</Soon>
    </section>

    <section id="metrics" aria-labelledby="metrics-title">
      <h2 id="metrics-title">Metrics: how your tasks went</h2>
      <p>Settings › Metrics on the iPhone and the Mac shows how your tasks went. In the terminal app <kbd>Shift</kbd>+<kbd>M</kbd> opens the same view, and <code>alondra metrics</code> prints it.</p>
      <p>For the tasks that ended, it counts how many were done with no rework, the rounds of changes and the times each one needed you, and how many were reopened, relaunched or cancelled. It adds up their cost in total, per task, per merged pull request and per changed line; their input, output and cache tokens; their steps, and how many of those only looked or waited; and the time they spent working, waiting for you, queued and blocked.</p>
      <p>You pick the last 7, 30 or 90 days or all time, and compare them with the period before, or with the days since a version or a date. The results split by model and effort and by project, and choosing a row filters by it. <code>alondra metrics</code> can also split them by provider and by kind of task.</p>
      <p>The cost is the provider’s own estimate, not your bill. Codex reports tokens but no price, so its tasks show no cost.</p>
      <p>The numbers are there for you to see which model does your kind of work well and what it costs, and the orchestrator reads them too: its table of models carries, for each model, how many of your tasks it finished, how many were done the first time and what they cost on average.</p>
    </section>

    <section id="mcp" aria-labelledby="mcp-title">
      <h2 id="mcp-title">MCP servers in one place</h2>
      <p>Settings on the Mac, the iPhone and in the terminal list the MCP servers your agents use, your own and each project’s, with the status of each. On the Mac you can connect a server that needs a sign-in, reconnect it, sign out, check it again and delete one you added to your own settings. Connecting goes through a node that uses the server, so a server no task uses yet waits until one does.</p>
      <Soon>Authorizing an MCP server from a device other than the Mac.</Soon>
    </section>

    <section id="more" aria-labelledby="more-title">
      <h2 id="more-title">Also in the box</h2>
      <ul className="plain">
        <li>Scheduled runs that start work on a timetable.</li>
        <li>The apps and the terminal app in English and Spanish.</li>
      </ul>
    </section>

    <p className="doc-next"><Link className="button" to="/install">Install Alondra</Link></p>
  </article>;
}
