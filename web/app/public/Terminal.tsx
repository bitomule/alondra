type Run = { t: string; fg: string; bg: string; b?: boolean; d?: boolean; r?: boolean };
type Screen = { name: string; width: number; height: number; rows: Run[][] };

const screens = import.meta.glob<Screen>('./tui/*.json', { eager: true, import: 'default' });

function tone(color: string): string | undefined {
  const rgb = /^Rgb\((\d+), (\d+), (\d+)\)$/.exec(color);
  if (rgb) return `rgb(${rgb[1]} ${rgb[2]} ${rgb[3]})`;
  return color === 'Reset' ? undefined : `var(--tui-${color.toLowerCase()})`;
}

// Rendered from the real TUI (ratatui TestBackend) with synthetic data, cell styles preserved.
export function Terminal({ name, label, className = '' }: { name: string; label: string; className?: string }) {
  const screen = screens[`./tui/${name}.json`];
  if (!screen) return null;
  return (
    <figure className={`tui ${className}`} style={{ '--cols': screen.width } as React.CSSProperties}>
      <pre className="tui-screen" role="img" aria-label={label}>
        {screen.rows.map((row, y) => (
          <span className="tui-row" key={y}>
            {row.map((run, x) => {
              let fg = tone(run.fg), bg = tone(run.bg);
              if (run.r) [fg, bg] = [bg ?? 'var(--tui-bg)', fg ?? 'var(--tui-fg)'];
              return <span key={x} style={{ color: fg, background: bg, fontWeight: run.b ? 700 : undefined, opacity: run.d ? 0.62 : undefined }}>{run.t}</span>;
            })}
            {'\n'}
          </span>
        ))}
      </pre>
    </figure>
  );
}
