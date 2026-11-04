import { Capture } from './Capture';
import { MotionToggle } from './MotionToggle';
import { useArmed } from './useArmed';

const STEPS = [
  'An agent has a question. The card appears in the terminal app.',
  'The same card reaches your iPhone.',
  'You tap an answer.',
  'The answer goes back to the agent, which carries on.',
];

export function CardJourney() {
  const { ref, state, paused, togglePaused } = useArmed<HTMLDivElement>();
  return (
    <div className={`journey journey-${state}${paused ? ' is-paused' : ''}`} ref={ref}>
      <div className="journey-side">
        <ol className="journey-steps">
          {STEPS.map((text, i) => <li key={text} className={`journey-step journey-step-${i + 1}`}>{text}</li>)}
        </ol>
        <MotionToggle state={state} paused={paused} onToggle={togglePaused} what="highlights"/>
      </div>
      <div className="journey-tui">
        <Capture name="tui-tasks" alt="The terminal app with a task marked as needing you: “How often should Harbor refresh the order list?”"/>
        <span className="mark mark-1" aria-hidden="true"/>
        <span className="mark mark-4" aria-hidden="true"/>
      </div>
      <div className="journey-phone">
        <Capture name="iphone-card" alt="The same question as a card on the iPhone, with two answers as buttons."/>
        <span className="mark mark-2" aria-hidden="true"/>
        <span className="mark mark-3" aria-hidden="true"/>
      </div>
    </div>
  );
}
