import { db } from '../../../db/_generated/server.ts';
import { fixtureSettings, verifySession } from '../../../lib/session.ts';
export const runtime = 'nodejs';
export const dynamic = 'force-dynamic';
export async function GET(request: Request) {
  const session = verifySession(request.headers);
  if (!session) return Response.json({ error: 'Sign in required' }, { status: 401 });
  return Response.json(await db.select('tasks').where({ owner: session.id }).limit(50));
}
export async function POST(request: Request) {
  const session = verifySession(request.headers);
  if (!session) return Response.json({ error: 'Sign in required' }, { status: 401 });
  if (request.headers.get('origin') !== fixtureSettings().origin) return Response.json({ error: 'Invalid origin' }, { status: 403 });
  const input: unknown = await request.json();
  if (!input || typeof input !== 'object' || !('title' in input) || typeof input.title !== 'string'
    || !input.title.trim() || input.title.length > 200) return Response.json({ error: 'Invalid title' }, { status: 400 });
  const title = input.title.trim();
  if (new URL(request.url).searchParams.get('rollback') === 'true') {
    const rollback = new Error('Fixture rollback');
    try {
      await db.transaction(async tx => {
        await tx.insert('tasks', { title, owner: session.id });
        await tx.insert('tasks', { title: `${title} second`, owner: session.id });
        throw rollback;
      });
    } catch (error) {
      if (error !== rollback) throw error;
      return Response.json({ rolledBack: true }, { status: 409 });
    }
  }
  return Response.json(await db.insert('tasks', { title, owner: session.id }));
}
