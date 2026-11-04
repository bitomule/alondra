import { Link } from 'react-router';
import { Capture } from '../public/Capture';
import { CardJourney } from '../public/CardJourney';
import { CopyCommand } from '../public/CopyCommand';
import { Diagram } from '../public/Diagram';
import { FlowDiagram } from '../public/FlowDiagram';
import { TerminalDemo } from '../public/TerminalDemo';
import { WaitlistForm, useWaitlist } from '../public/WaitlistForm';

export function meta() {
  return [
    { title: 'Alondra: a team of agents, one inbox' },
    { name: 'description', content: 'Alondra turns a request into a plan and parallel tasks for Claude Code and Codex agents, and brings every decision to you as a card on your iPhone, your Mac or the terminal.' },
    { tagName: 'link', rel: 'canonical', href: 'https://getalondra.com/' },
  ];
}

const STEPS = [
  { title: 'The orchestrator makes the plan.', body: 'You write what you want, from the terminal app, the Mac app or the iPhone app. The orchestrator turns it into a plan and the plan into tasks. Only the orchestrator creates tasks.' },
  { title: 'Tasks wait for each other.', body: 'A task that depends on another starts once that one has landed. Tasks with nothing between them run at the same time, as many as your Mac has room for.' },
  { title: 'Each task gets its own model.', body: 'The orchestrator picks Claude or Codex, the model and the reasoning effort for each task, and writes down why. You can connect several accounts of each provider: Alondra runs each task on one of them and passes over any whose plan has run out.' },
  { title: 'Only decisions reach you.', body: 'You never read the agents’ conversations. Each task shows what its agent is doing and the last thing it said, and whatever needs you arrives as a card.' },
  { title: 'Deciding and doing stay apart.', body: 'You and the orchestrator decide what gets built. Each node, one agent session in its own git worktree, only does its task. It cannot create tasks; it can only suggest one to the orchestrator.' },
];

const MORE = [
  { title: 'The work stays on your Mac', body: 'The daemon, your repositories, your worktrees and your Claude and Codex logins stay on your Mac. Alondra Cloud only connects your devices.' },
  { title: 'Metrics for every task', body: 'Settings › Metrics on the iPhone and the Mac shows how your tasks went, by model and by project: how many were done with no rework, how long they waited for you, and the provider’s estimate of what they cost. The orchestrator reads the same numbers when it picks a model.' },
  { title: 'Your choice of model wins', body: 'Set the model and the effort of a task yourself and the orchestrator keeps them. A decider that learns from past results is coming soon.', soon: true },
  { title: 'More than one Mac', body: 'Pair your iPhone with each of your Macs and switch between them. Using several at once from one screen is coming soon.', soon: true },
  { title: 'MCP servers in one place', body: 'Settings lists the MCP servers your agents use, project by project, with their status. Authorizing a server from a device other than the Mac is coming soon.', soon: true },
];

export default function Landing() {
  const waitlist = useWaitlist();
  return <>
    <section className="hero">
      <div className="wrap hero-grid">
        <div className="hero-text">
          <h1>A team of agents. One inbox.</h1>
          <p className="hero-lede">Alondra plans the work in parallel and brings you only the decisions.</p>
          <div className="actions">
            <Link className="button" to="/install">Install with Homebrew</Link>
            <Link className="text-link" to="/#cloud">Join the Alondra Cloud waitlist</Link>
          </div>
          <CopyCommand command="brew install bitomule/tap/alondra"/>
        </div>
        <div className="hero-stack">
          <Capture name="mac-review" tone="dark" eager className="hero-mac" alt="The Mac app showing the changes of a pull request, file by file."/>
          <Capture name="tui-tasks" tone="dark" eager className="hero-tui" alt="The terminal app: a task that needs you with a question, two pull requests ready for review, one task running and one waiting for another."/>
          <Capture name="iphone-card" tone="dark" eager className="hero-phone" alt="The iPhone app: a question from an agent as a card, with two answers as buttons."/>
        </div>
      </div>
    </section>

    <section className="band wrap" aria-labelledby="flow-title">
      <h2 id="flow-title">How Alondra works</h2>
      <p className="band-lede">You and the orchestrator decide. The nodes do the work, each on the model its task needs.</p>
      <FlowDiagram/>
      <div className="split">
        <ol className="steps">
          {STEPS.map(step => <li key={step.title}><h3>{step.title}</h3><p>{step.body}</p></li>)}
        </ol>
        <TerminalDemo/>
      </div>
      <p><Link className="text-link" to="/how-it-works#orchestrator">How the orchestrator works</Link></p>
    </section>

    <section className="band wrap" aria-labelledby="card-title">
      <h2 id="card-title">A decision, end to end.</h2>
      <p className="band-lede">A card is one thing: a question with answer buttons, a plan to approve, a permission or a pull request to review. It carries its own context, so you decide without opening anything else.</p>
      <CardJourney/>
      <p className="band-note">The iPhone notifies you. A question’s answers also appear on its notification, and answering from there needs the phone unlocked. <Link className="text-link" to="/how-it-works#inbox">How the inbox works</Link></p>
    </section>

    <section className="feature feature-flip wrap" aria-labelledby="review-title">
      <div className="feature-text">
        <h2 id="review-title">Review the diff where you are.</h2>
        <p>A pull request card opens its changes, file by file, in the iPhone and Mac apps. Tap or click a line to comment on it, and your comments go back to the agent when you request changes. The card shows how the checks went.</p>
        <p>In the terminal the diff opens in tuicr, which Homebrew installs with Alondra. When you accept a delivery, Alondra merges the pull request and removes the task’s worktree.</p>
      </div>
      <Capture name="mac-review" className="feature-mac" alt="The Mac app showing the changes of a pull request: two added files, each with its lines marked as added."/>
    </section>

    <section className="band wrap" aria-labelledby="remote-title">
      <h2 id="remote-title">Away from your desk, nothing to set up.</h2>
      <p className="band-lede">Pair your iPhone with your Mac once, in person. From then on it reaches your Mac directly when the network allows it, and through the Alondra Cloud relay when it does not. There is no Tailscale, no VPN and no port to forward. Requests and answers are sealed on one device and opened on the other, so the relay carries bytes it cannot read.</p>
      <Diagram/>
      <p><Link className="text-link" to="/how-it-works#architecture">The architecture in detail</Link></p>
    </section>

    <section className="band wrap" aria-labelledby="more-title">
      <h2 id="more-title">What else is different</h2>
      <ul className="rows">
        {MORE.map(item => <li key={item.title}><h3>{item.title}</h3><p>{item.body}{item.soon && <span className="soon">Coming soon</span>}</p></li>)}
      </ul>
    </section>

    <section className="cloud" id="cloud" aria-labelledby="cloud-title">
      <div className="wrap cloud-grid">
        <div>
          <h2 id="cloud-title">Alondra Cloud connects your devices.</h2>
          <p>Accounts, pairing, push notifications and the relay that reaches your Mac when your iPhone cannot connect to it directly. Your agents, your code and your tasks stay on your Mac.</p>
          <p>Access opens little by little. Leave your email and you will be invited in a small batch. Teams are coming soon.</p>
        </div>
        <WaitlistForm waitlist={waitlist} name="cloud" tone="stage"/>
      </div>
    </section>

    <section className="band wrap license-note" aria-labelledby="license-title">
      <h2 id="license-title">The source is there to read.</h2>
      <p>Alondra is released under FSL-1.1-MIT. Use it for your own work, inside your company, for non-commercial research or to provide services to a client. You may not sell it as a competing product, and every version becomes MIT two years after it is published.</p>
      <p><Link className="text-link" to="/license">What the license allows</Link></p>
    </section>
  </>;
}
