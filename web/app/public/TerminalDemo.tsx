import { MotionToggle } from './MotionToggle';
import { useArmed } from './useArmed';

const REQUEST = 'Ship Lumen 2.4: release notes, dark mode, offline mode and the Harbor refresh rate';

type Row = { name: string; before: string; after: string; note?: string; tone: 'ok' | 'ask' | 'busy' | 'wait' };
const ROWS: Row[] = [
  { name: '2.4 release notes', before: 'In the background', after: 'In the background', note: 'make test', tone: 'busy' },
  { name: 'Publish 2.4', before: 'Waiting', after: 'Waiting', note: 'after the release notes', tone: 'wait' },
  { name: 'Dark mode', before: 'Working', after: 'Ready for review', note: 'PR #12', tone: 'ok' },
  { name: 'Offline mode', before: 'Working', after: 'Ready for review', tone: 'ok' },
  { name: 'Orders refresh rate', before: 'Working', after: 'Needs you', note: '“How often should Harbor refresh the order list?”', tone: 'ask' },
];

export function TerminalDemo() {
  const { ref, state, paused, togglePaused } = useArmed<HTMLElement>();
  return (
    <figure className={`demo demo-${state}${paused ? ' is-paused' : ''}`} ref={ref}>
      <div className="demo-screen" role="img" aria-label={`A replay with demo data. The request “${REQUEST}” becomes five tasks. The release notes run in the background, publishing waits for them, dark mode and offline mode reach pull requests ready for review, and the refresh rate task asks you how often Harbor should refresh the order list.`}>
        <p className="demo-line demo-request" aria-hidden="true"><span className="demo-prompt">&gt; </span><span className="demo-typed">{REQUEST}</span></p>
        <p className="demo-line demo-plan" aria-hidden="true">Plan: 5 tasks, 1 waits for another</p>
        {ROWS.map((row, index) => (
          <p key={row.name} className={`demo-line demo-row demo-${row.tone}`} style={{ ['--row' as string]: index }} aria-hidden="true">
            <span className="demo-name">{row.name}</span>
            <span className="demo-states"><span className="demo-before">{row.before}</span><span className="demo-after">{row.after}</span></span>
            <span className="demo-note">{row.note}</span>
          </p>
        ))}
      </div>
      <figcaption>A replay of the demo world in the captures. Each task runs in its own worktree, and only the one that needs a decision reaches you. <MotionToggle state={state} paused={paused} onToggle={togglePaused} what="replay"/></figcaption>
    </figure>
  );
}
