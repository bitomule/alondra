import type { Config } from '@react-router/dev/config';
// Private routes contain only an unauthenticated loading shell at build time.
export default { ssr: false, prerender: ['/', '/how-it-works', '/install', '/docs', '/security', '/privacy', '/license', '/account', '/admin', '/invite', '/login-error', '/account-closed'] } satisfies Config;
