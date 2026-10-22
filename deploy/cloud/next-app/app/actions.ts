'use server';
import { headers } from 'next/headers';
import { db } from '../db/_generated/server.ts';
import { verifySession } from '../lib/session.ts';
export async function createTask(form: FormData): Promise<void> {
  const session = verifySession(new Headers(await headers()));
  if (!session) throw new Error('Sign in required');
  const title = form.get('title');
  if (typeof title !== 'string' || title.trim().length === 0 || title.length > 200) throw new Error('Invalid title');
  await db.insert('tasks', { title: title.trim(), owner: session.id });
}
