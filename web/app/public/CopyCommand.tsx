import { Fragment, useState } from 'react';

export function CopyCommand({ command, className = '' }: { command: string; className?: string }) {
  const [copied, setCopied] = useState(false);
  async function copy() {
    try { await navigator.clipboard.writeText(command); setCopied(true); setTimeout(() => setCopied(false), 2000); } catch { setCopied(false); }
  }
  return (
    <div className={`copy-command ${className}`}>
      <code><span aria-hidden="true">$ </span>{command.split('/').map((part, index) => <Fragment key={index}>{index > 0 && <>/<wbr/></>}{part}</Fragment>)}</code>
      <button type="button" onClick={copy} aria-label={`Copy the command ${command}`}>{copied ? 'Copied' : 'Copy'}</button>
      <span className="visually-hidden" role="status">{copied ? 'Command copied' : ''}</span>
    </div>
  );
}
