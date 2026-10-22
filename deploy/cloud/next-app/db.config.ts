import { defineConfig } from '@hulla/db/config';
import { fixtureSettings, verifySession } from './lib/session.ts';
const settings = fixtureSettings();
export default defineConfig({
  database: { server: { url: settings.databaseUrl, namespace: settings.namespace }, client: { url: process.env.NEXT_PUBLIC_HULLA_SERVICE_URL!, credentials: "include" } },
  service: { host: process.env.SERVICE_HOST ?? '127.0.0.1', port: Number(process.env.SERVICE_PORT ?? 3001), origins: [settings.origin] },
  auth: headers => {
    const session = verifySession(headers);
    return session ? { key: session.id, identity: session } : null;
  },
});
