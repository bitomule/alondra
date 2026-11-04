import { MotionToggle } from './MotionToggle';
import { useArmed } from './useArmed';

type Task = { name: string; runs: string; waits?: boolean };
const TASKS: Task[] = [
  { name: 'Dark mode', runs: 'Claude · Opus · high' },
  { name: 'Offline mode', runs: 'Codex · high' },
  { name: 'Orders refresh rate', runs: 'Codex · medium' },
  { name: '2.4 release notes', runs: 'Claude · Sonnet · low · account 2' },
  { name: 'Publish 2.4', runs: 'Waits for the release notes', waits: true },
];

function Wide() {
  return (
    <svg className="flow-art flow-wide" viewBox="0 0 1000 556" aria-hidden="true" focusable="false">
      <g className="lane"><rect x="10" y="10" width="980" height="170" rx="14"/><text className="lane-title" x="30" y="40">Decides: you and the orchestrator</text></g>
      <g className="lane"><rect x="10" y="210" width="980" height="336" rx="14"/><text className="lane-title" x="30" y="240">Does the work: one node per task</text></g>
      <path className="flow-line" d="M230 112 H282"/>
      <path className="flow-line" d="M400 158 V250"/>
      <path className="flow-line" d="M400 250 V502 M400 278 H440 M400 334 H440 M400 390 H440 M400 446 H440 M400 502 H440"/>
      <path className="flow-line" d="M890 390 H930 V170 M890 278 H930"/>
      <path className="flow-line flow-wait" d="M890 502 H910 V446 H890"/>
      <path className="flow-arrow" d="M276 106 L284 112 L276 118 Z M924 172 L930 164 L936 172 Z"/>
      <g className="flow-stage flow-stage-1"><rect x="30" y="62" width="200" height="100" rx="8"/><rect className="flow-lit" x="30" y="62" width="200" height="100" rx="8"/><text className="node-title" x="48" y="96">You ask</text><text x="48" y="122">“Ship Lumen 2.4”</text><text x="48" y="144">from any device</text></g>
      <g className="flow-stage flow-stage-2"><rect x="284" y="62" width="300" height="96" rx="8"/><rect className="flow-lit" x="284" y="62" width="300" height="96" rx="8"/><text className="node-title" x="302" y="96">The orchestrator plans</text><text x="302" y="122">5 tasks, 1 waits for another,</text><text x="302" y="144">a model and an effort for each</text></g>
      <g className="flow-stage flow-stage-5"><rect x="640" y="56" width="330" height="108" rx="8"/><rect className="flow-lit" x="640" y="56" width="330" height="108" rx="8"/><text className="node-title" x="658" y="88">Cards for you</text><text x="658" y="114">A question: how often should</text><text x="658" y="134">Harbor refresh the order list?</text><text x="658" y="154">A pull request: dark mode</text></g>
      {TASKS.map((task, index) => {
        const y = 256 + index * 56;
        return (
          <g key={task.name} className={`flow-stage ${task.waits ? 'flow-stage-4 flow-task-waits' : 'flow-stage-3'}`}>
            <rect x="440" y={y} width="450" height="44" rx="8"/><rect className="flow-lit" x="440" y={y} width="450" height="44" rx="8"/>
            <text className="node-title" x="456" y={y + 28}>{task.name}</text>
            <text className="flow-runs" x="874" y={y + 28} textAnchor="end">{task.runs}</text>
          </g>
        );
      })}
      <text className="flow-note" x="30" y="300">Tasks with no link</text>
      <text className="flow-note" x="30" y="322">between them run at once.</text>
      <text className="flow-note" x="30" y="356">You see what each node</text>
      <text className="flow-note" x="30" y="378">is doing. Only decisions</text>
      <text className="flow-note" x="30" y="400">go up to you.</text>
    </svg>
  );
}

function Narrow() {
  return (
    <svg className="flow-art flow-narrow" viewBox="0 0 360 746" aria-hidden="true" focusable="false">
      <g className="lane"><rect x="6" y="6" width="348" height="250" rx="14"/><text className="lane-title" x="20" y="30">Decides: you and the orchestrator</text></g>
      <g className="lane"><rect x="6" y="284" width="348" height="352" rx="14"/><text className="lane-title" x="20" y="308">Does the work: one node per task</text></g>
      <g className="lane"><rect x="6" y="664" width="348" height="76" rx="14"/><text className="lane-title" x="20" y="688">Back to you</text></g>
      <path className="flow-line" d="M180 110 V132 M180 236 V284 M180 636 V664"/>
      <path className="flow-line flow-wait" d="M36 578 H24 V522 H36"/>
      <g className="flow-stage flow-stage-1"><rect x="20" y="44" width="320" height="66" rx="8"/><rect className="flow-lit" x="20" y="44" width="320" height="66" rx="8"/><text className="node-title" x="34" y="70">You ask</text><text x="34" y="94">“Ship Lumen 2.4”</text></g>
      <g className="flow-stage flow-stage-2"><rect x="20" y="132" width="320" height="104" rx="8"/><rect className="flow-lit" x="20" y="132" width="320" height="104" rx="8"/><text className="node-title" x="34" y="158">The orchestrator plans</text><text x="34" y="182">5 tasks, 1 waits for another,</text><text x="34" y="204">a model and an effort</text><text x="34" y="224">for each</text></g>
      {TASKS.map((task, index) => {
        const y = 330 + index * 56;
        return (
          <g key={task.name} className={`flow-stage ${task.waits ? 'flow-stage-4 flow-task-waits' : 'flow-stage-3'}`}>
            <rect x="36" y={y} width="304" height="48" rx="8"/><rect className="flow-lit" x="36" y={y} width="304" height="48" rx="8"/>
            <text className="node-title" x="48" y={y + 20}>{task.name}</text>
            <text className="flow-runs" x="48" y={y + 39}>{task.runs}</text>
          </g>
        );
      })}
      <g className="flow-stage flow-stage-5"><rect x="20" y="700" width="320" height="32" rx="8"/><rect className="flow-lit" x="20" y="700" width="320" height="32" rx="8"/><text className="node-title" x="34" y="721">A question and a pull request</text></g>
    </svg>
  );
}

export function FlowDiagram() {
  const { ref, state, paused, togglePaused } = useArmed<HTMLElement>();
  return (
    <figure className={`flow flow-${state}${paused ? ' is-paused' : ''}`} ref={ref}>
      <div role="img" aria-label="You ask for “Ship Lumen 2.4”. The orchestrator plans five tasks and picks a model and an effort for each: dark mode on Claude Opus with high effort, offline mode on Codex with high effort, the orders refresh rate on Codex with medium effort, the release notes on Claude Sonnet with low effort on a second account, and publishing, which waits for the release notes. The nodes do the work. Only two cards come back to you: a question about how often Harbor should refresh the order list, and the dark mode pull request.">
        <Wide/><Narrow/>
      </div>
      <figcaption>
        Demo data. You and the orchestrator decide what gets built; each node only does its task.
        <MotionToggle state={state} paused={paused} onToggle={togglePaused} what="flow"/>
      </figcaption>
    </figure>
  );
}
