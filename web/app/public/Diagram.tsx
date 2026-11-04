import { MotionToggle } from './MotionToggle';
import { useArmed } from './useArmed';

function Desktop() {
  return (
    <svg className="diagram-art diagram-wide" viewBox="0 0 1000 470" aria-hidden="true" focusable="false">
      <path className="line line-direct" d="M165 60 V32 H835 V60"/>
      <path className="line line-relay" d="M290 215 H350 V315 H405 M595 315 H650 M650 185 V365 M650 185 H710 M650 275 H710 M650 365 H710"/>
      <path className="line line-control" d="M290 135 H405 M595 135 H690"/>
      <g className="group"><rect x="20" y="60" width="290" height="380" rx="14"/><text className="group-title" x="40" y="92">Your Mac</text></g>
      <g className="group"><rect x="390" y="60" width="220" height="380" rx="14"/><text className="group-title" x="410" y="92">Alondra Cloud</text></g>
      <g className="group"><rect x="690" y="60" width="290" height="380" rx="14"/><text className="group-title" x="710" y="92">Your devices</text></g>
      <g className="node"><rect x="40" y="120" width="250" height="110" rx="8"/><text className="node-title" x="58" y="156">alondrad</text><text x="58" y="182">The daemon: state,</text><text x="58" y="203">orchestrator, cards</text></g>
      <g className="node"><rect x="40" y="260" width="250" height="110" rx="8"/><text className="node-title" x="58" y="296">Agent sessions</text><text x="58" y="322">Claude Code or Codex,</text><text x="58" y="343">one worktree per task</text></g>
      <path className="line line-inside" d="M165 230 V260"/>
      <g className="node"><rect x="405" y="120" width="190" height="110" rx="8"/><text className="node-title" x="421" y="156">Control plane</text><text x="421" y="182">Accounts, pairing,</text><text x="421" y="203">device list, push</text></g>
      <g className="node"><rect x="405" y="260" width="190" height="110" rx="8"/><text className="node-title" x="421" y="296">Relay</text><text x="421" y="322">Carries sealed bytes</text><text x="421" y="343">it cannot open</text></g>
      <g className="node"><rect x="710" y="150" width="250" height="70" rx="8"/><text className="node-title" x="728" y="191">iPhone app</text></g>
      <g className="node"><rect x="710" y="240" width="250" height="70" rx="8"/><text className="node-title" x="728" y="281">Mac app</text></g>
      <g className="node"><rect x="710" y="330" width="250" height="70" rx="8"/><text className="node-title" x="728" y="371">Terminal UI</text></g>
      <g className="packets">
        <rect className="packet packet-direct" x="-8" y="-6" width="16" height="12" rx="3"><animateMotion dur="4.8s" repeatCount="indefinite" path="M165 60 V32 H835 V60"/></rect>
        <rect className="packet packet-direct" x="-8" y="-6" width="16" height="12" rx="3"><animateMotion dur="4.8s" begin="2.4s" repeatCount="indefinite" path="M835 60 V32 H165 V60"/></rect>
        <rect className="packet" x="-8" y="-6" width="16" height="12" rx="3"><animateMotion dur="4.8s" begin="0.8s" repeatCount="indefinite" path="M290 215 H350 V315 H405 H595 H650 V185 H710"/></rect>
        <rect className="packet" x="-8" y="-6" width="16" height="12" rx="3"><animateMotion dur="4.8s" begin="3.2s" repeatCount="indefinite" path="M710 275 H650 V315 H595 H405 H350 V215 H290"/></rect>
      </g>
    </svg>
  );
}

function Narrow() {
  return (
    <svg className="diagram-art diagram-narrow" viewBox="0 0 360 620" aria-hidden="true" focusable="false">
      <path className="line line-direct" d="M320 85 H342 V515 H320"/>
      <path className="line line-relay" d="M101 140 V254 M101 350 V430"/>
      <path className="line line-control" d="M240 254 V160 M240 350 V430"/>
      <g className="group"><rect x="20" y="10" width="300" height="150" rx="14"/><text className="group-title" x="36" y="34">Your Mac</text></g>
      <g className="group"><rect x="20" y="220" width="300" height="150" rx="14"/><text className="group-title" x="36" y="244">Alondra Cloud</text></g>
      <g className="group"><rect x="20" y="430" width="300" height="170" rx="14"/><text className="group-title" x="36" y="454">Your devices</text></g>
      <g className="node"><rect x="36" y="48" width="130" height="92" rx="8"/><text className="node-title" x="48" y="76">alondrad</text><text x="48" y="98">State,</text><text x="48" y="116">orchestrator</text></g>
      <g className="node"><rect x="176" y="48" width="128" height="92" rx="8"/><text className="node-title" x="188" y="76">Agent sessions</text><text x="188" y="98">Claude Code</text><text x="188" y="116">or Codex</text></g>
      <g className="node"><rect x="36" y="254" width="130" height="96" rx="8"/><text className="node-title" x="48" y="282">Relay</text><text x="48" y="304">Sealed bytes</text><text x="48" y="322">only</text></g>
      <g className="node"><rect x="176" y="254" width="128" height="96" rx="8"/><text className="node-title" x="188" y="282">Control plane</text><text x="188" y="304">Accounts,</text><text x="188" y="322">pairing, push</text></g>
      <g className="node"><rect x="36" y="466" width="268" height="36" rx="8"/><text className="node-title" x="48" y="490">iPhone app</text></g>
      <g className="node"><rect x="36" y="510" width="268" height="36" rx="8"/><text className="node-title" x="48" y="534">Mac app</text></g>
      <g className="node"><rect x="36" y="554" width="268" height="36" rx="8"/><text className="node-title" x="48" y="578">Terminal UI</text></g>
      <g className="packets">
        <rect className="packet packet-direct" x="-8" y="-6" width="16" height="12" rx="3"><animateMotion dur="4.8s" repeatCount="indefinite" path="M320 85 H342 V515 H320"/></rect>
        <rect className="packet" x="-8" y="-6" width="16" height="12" rx="3"><animateMotion dur="4.8s" begin="1s" repeatCount="indefinite" path="M101 140 V430"/></rect>
      </g>
    </svg>
  );
}

export function Diagram() {
  const { ref, state, paused, togglePaused } = useArmed<HTMLElement>();
  return (
    <figure className={`diagram diagram-${state}`} ref={ref}>
      <div role="img" aria-label="Your Mac runs the daemon and the agent sessions. Your iPhone, Mac app and terminal reach it directly when the network allows, or through the Alondra Cloud relay. The control plane only handles accounts, pairing, the device list and push.">
        <Desktop/><Narrow/>
      </div>
      <figcaption>
        <ul className="legend">
          <li><svg viewBox="0 0 40 8" aria-hidden="true"><path className="line line-direct" d="M0 4 H40"/></svg>A direct connection, when the network allows it.</li>
          <li><svg viewBox="0 0 40 8" aria-hidden="true"><path className="line line-relay" d="M0 4 H40"/></svg>Through the relay, when it does not.</li>
          <li><svg viewBox="0 0 40 8" aria-hidden="true"><path className="line line-control" d="M0 4 H40"/></svg>Pairing and push. No task content goes this way.</li>
          <li><svg viewBox="0 0 40 8" aria-hidden="true"><rect className="packet" x="12" y="0" width="16" height="8" rx="2"/></svg>A sealed envelope. The relay passes it along and cannot open it.</li>
        </ul>
        <MotionToggle state={state} paused={paused} onToggle={togglePaused} what="envelopes"/>
      </figcaption>
    </figure>
  );
}
