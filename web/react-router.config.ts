import type { Config } from '@react-router/dev/config';
// Private routes contain only an unauthenticated loading shell at build time.
export default { ssr: false, prerender: ['/', '/privacy', '/install', '/account', '/admin', '/invite'] } satisfies Config;
