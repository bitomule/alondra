export function TestFlightLink({ url, className }: { url?: string; className: string }) {
  if (!url) return null;
  return <a className={className} href={url} rel="noopener noreferrer">Instala la app desde TestFlight</a>;
}
