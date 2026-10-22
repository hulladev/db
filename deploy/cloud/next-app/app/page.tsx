import { headers } from 'next/headers';
import { db } from '../db/_generated/server.ts';
import { verifySession } from '../lib/session.ts';
import { createTask } from './actions.ts';
import { LiveTasks } from './live-tasks.tsx';
export const runtime = 'nodejs';
export const dynamic = 'force-dynamic';
export default async function Page() {
  const session = verifySession(new Headers(await headers()));
  if (!session) return <main><h1>Cloud compatibility fixture</h1><p>Supply the fixture session cookie through the local test harness.</p></main>;
  const tasks = await db.select('tasks').where({ owner: session.id }).limit(50);
  return <main><h1>Cloud compatibility fixture</h1>
    <section aria-label="Server rendered query"><ul data-testid="ssr-tasks">{tasks.map(task => <li key={task.id}>{task.title}</li>)}</ul></section>
    <form action={createTask}><label>Task title <input name="title" required maxLength={200} /></label><button type="submit">Create through server action</button></form>
    <LiveTasks userId={session.id} />
  </main>;
}
