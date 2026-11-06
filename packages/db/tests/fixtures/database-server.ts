import { sqlite } from '@hulla/db-sqlite';
import { schema } from './database-schema.ts';

export const primary = sqlite({ schema, filename: ':memory:', functions: () => import('./database-catalogue.ts') });
