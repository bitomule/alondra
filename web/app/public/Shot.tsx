const shots = import.meta.glob<string>('./shots/*.png', { eager: true, query: '?no-inline', import: 'default' });

export const SHOT_WIDTH = 600;
export const SHOT_HEIGHT = 1304;

// Real captures of the iPhone app's demo (synthetic data), one per appearance.
export function Shot({ name, alt, tone = 'auto', className = '', eager = false }: { name: string; alt: string; tone?: 'auto' | 'light' | 'dark'; className?: string; eager?: boolean }) {
  const light = shots[`./shots/${name}-light.png`];
  const dark = shots[`./shots/${name}-dark.png`] ?? light;
  if (!light) return null;
  return (
    <picture className={`shot ${className}`}>
      {tone === 'auto' && <source media="(prefers-color-scheme: dark)" srcSet={dark}/>}
      <img src={tone === 'dark' ? dark : light} alt={alt} width={SHOT_WIDTH} height={SHOT_HEIGHT} loading={eager ? 'eager' : 'lazy'} decoding="async"/>
    </picture>
  );
}
