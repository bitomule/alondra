const captures = import.meta.glob<string>('./captures/*.png', { eager: true, query: '?no-inline', import: 'default' });

const SIZES = { 'tui-tasks': [1100, 694], 'mac-review': [1100, 797], 'iphone-card': [420, 913] } as const;

export type CaptureName = keyof typeof SIZES;

export function Capture({ name, alt, eager = false, tone = 'auto', className = '' }: { name: CaptureName; alt: string; eager?: boolean; tone?: 'auto' | 'dark'; className?: string }) {
  const [width, height] = SIZES[name];
  return (
    <picture className={`capture capture-${name} ${className}`}>
      {tone === 'auto' && <source media="(prefers-color-scheme: dark)" srcSet={captures[`./captures/${name}-dark.png`]}/>}
      <img src={captures[`./captures/${name}-${tone === 'dark' ? 'dark' : 'light'}.png`]} alt={alt} width={width} height={height} loading={eager ? 'eager' : 'lazy'} decoding="async"/>
    </picture>
  );
}
