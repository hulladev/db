/** @type {import('next').NextConfig} */
export default {
  output: 'standalone',
  serverExternalPackages: ['@hulla/db'],
  outputFileTracingIncludes: { '/*': ['node_modules/@hulla/db/native/**/*'] },
  // Hosted cookie sessions can use /db on the app origin. The destination is
  // server-only; local proofs may still connect directly to the service.
  async rewrites() {
    const service = process.env.HULLA_SERVICE_ORIGIN;
    return service ? [{ source: '/db/:path*', destination: `${service}/:path*` }] : [];
  },
};
