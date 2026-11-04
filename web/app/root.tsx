import { Links, Meta, Outlet, Scripts, ScrollRestoration, useLocation } from 'react-router';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { useState } from 'react';
import favicon from './public/brand/favicon.svg?no-inline';
import touchIcon from './public/brand/apple-touch-icon.png?no-inline';
export function links() { return [{ rel: 'icon', type: 'image/svg+xml', href: favicon }, { rel: 'apple-touch-icon', href: touchIcon }]; }
const PRIVATE = /^\/(account|admin|invite|login-error|account-closed)(\/|$)/;
export function Layout({ children }: { children: React.ReactNode }) {
 const privateRoute = PRIVATE.test(useLocation().pathname);
 return <html lang={privateRoute ? 'es' : 'en'}><head><meta charSet="utf-8"/><meta name="viewport" content="width=device-width, initial-scale=1"/><meta name="referrer" content="no-referrer"/><Meta/><Links/></head><body><a className="skip-link" href="#main">{privateRoute ? 'Saltar al contenido' : 'Skip to content'}</a>{children}<ScrollRestoration/><Scripts/></body></html>;
}
export default function App() {
 const [client] = useState(() => new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: 15000 }, mutations: { retry: false } } }));
 return <QueryClientProvider client={client}><Outlet/></QueryClientProvider>;
}
