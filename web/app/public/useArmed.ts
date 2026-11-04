import { useEffect, useRef, useState } from 'react';

export type Armed = 'still' | 'armed' | 'playing';

// Content is complete without scripting and under reduced motion; only a visible, motion-allowed section is armed and then played.
export function useArmed<T extends HTMLElement>() {
  const ref = useRef<T>(null);
  const [state, setState] = useState<Armed>('still');
  const [paused, setPaused] = useState(false);
  useEffect(() => {
    const node = ref.current;
    if (!node || window.matchMedia('(prefers-reduced-motion: reduce)').matches || !('IntersectionObserver' in window)) return;
    setState('armed');
    const observer = new IntersectionObserver(entries => {
      if (entries.some(entry => entry.isIntersecting)) { setState('playing'); observer.disconnect(); }
    }, { threshold: 0.35 });
    observer.observe(node);
    return () => observer.disconnect();
  }, []);
  useEffect(() => {
    ref.current?.querySelectorAll('svg').forEach(svg => paused ? svg.pauseAnimations() : svg.unpauseAnimations());
  }, [paused, state]);
  return { ref, state, paused, togglePaused: () => setPaused(value => !value) };
}
