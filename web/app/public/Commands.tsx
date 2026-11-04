export type Command = { run: string; note?: string };

export function Commands({ commands }: { commands: Command[] }) {
  return (
    <pre className="command"><code>{commands.map(({ run, note }) => (
      <span key={run} className="command-line">{run}{note && <span className="command-note">  # {note}</span>}</span>
    ))}</code></pre>
  );
}
