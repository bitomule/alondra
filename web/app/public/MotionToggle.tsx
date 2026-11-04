import type { Armed } from './useArmed';

export function MotionToggle({ state, paused, onToggle, what }: { state: Armed; paused: boolean; onToggle: () => void; what: string }) {
  if (state === 'still') return null;
  return <button type="button" className="motion-toggle" onClick={onToggle} aria-label={`${paused ? 'Play' : 'Pause'} the ${what}`}>{paused ? 'Play' : 'Pause'}</button>;
}
