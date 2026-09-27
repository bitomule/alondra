import { Links, Meta, Outlet, Scripts, ScrollRestoration, useLocation } from 'react-router';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { useState } from 'react';
import favicon from './public/brand/favicon.svg?no-inline';
import touchIcon from './public/brand/apple-touch-icon.png?no-inline';
export function links() { return [{ rel: 'icon', type: 'image/svg+xml', href: favicon }, { rel: 'apple-touch-icon', href: touchIcon }]; }
export function Layout({ children }: { children: React.ReactNode }) {
 const location = useLocation();
 const locale = location.pathname.replace(/\/$/, '') === '/proposal/en' || (location.pathname === '/' && new URLSearchParams(location.search).get('lang') === 'en') ? 'en' : 'es';
 return <html lang={locale}><head><meta charSet="utf-8"/><meta name="viewport" content="width=device-width, initial-scale=1"/><meta name="referrer" content="no-referrer"/><Meta/><Links/></head><body><a className="skip-link" href="#main">{locale === 'en' ? 'Skip to content' : 'Saltar al contenido'}</a>{children}<ScrollRestoration/><Scripts/></body></html>;
}
export default function App() {
 const [client] = useState(() => new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: 15000 }, mutations: { retry: false } } }));
 return <QueryClientProvider client={client}><Outlet/></QueryClientProvider>;
}
