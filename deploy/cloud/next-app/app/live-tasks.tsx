'use client';
import { useEffect, useState } from 'react';
import { setAuth } from '../db/_generated/client/runtime.ts';
import { list } from '../db/_generated/client/tasks.ts';
export function LiveTasks({ userId }: { userId: string }) {
  const [titles, setTitles] = useState<readonly string[]>([]);
  const [status, setStatus] = useState('pending');
  useEffect(() => {
    setAuth({ key: userId });
    const stop = list.subscribe(snapshot => {
      setStatus(snapshot.status === 'error' ? `error: ${snapshot.error instanceof Error ? snapshot.error.message : String(snapshot.error)}` : snapshot.status);
      setTitles(snapshot.data?.map(task => task.title) ?? []);
    });
    return () => { stop(); };
  }, [userId]);
  return <section aria-label="Live query"><output data-testid="live-status">{status}</output><ul data-testid="live-tasks">{titles.map((title, index) => <li key={index}>{title}</li>)}</ul></section>;
}
