import { loggedInvoke, logEvent } from "./logger";

export interface QueryResult {
  rowsAffected: number;
  lastInsertId?: number;
}

interface DatabaseClient {
  execute: (query: string, bindValues?: unknown[]) => Promise<QueryResult>;
  select: <T>(query: string, bindValues?: unknown[]) => Promise<T>;
}

let dbPromise: Promise<DatabaseClient> | null = null;

/**
 * Load the SQLite database at the fixed data dir (`<home>/ImageClient`).
 * The path is provided by the backend (single source of truth) so the file and
 * its migrations match the plugin's registration.
 */
async function loadDb(): Promise<DatabaseClient> {
  const started = performance.now();
  const dir = await loggedInvoke<string>("data_dir");
  try {
    const db: DatabaseClient = {
      execute: (query, bindValues = []) => loggedInvoke<QueryResult>("db_execute", { query, bindValues }),
      select: <T>(query: string, bindValues: unknown[] = []) => loggedInvoke<T>("db_select", { query, bindValues }),
    };
    logEvent("info", "database.open", { status: "success", durationMs: performance.now() - started, path: `${dir}/image-client.db` });
    return db;
  } catch (error) {
    logEvent("error", "database.open", { status: "error", durationMs: performance.now() - started, error: String(error) });
    throw error;
  }
}

export function getDb(): Promise<DatabaseClient> {
  if (!dbPromise) {
    dbPromise = loadDb();
  }
  return dbPromise;
}

function sqlOperation(query: string): string {
  return query.trim().split(/\s+/, 1)[0]?.toUpperCase() || "UNKNOWN";
}

function sqlName(query: string): string {
  return query.replace(/\s+/g, " ").trim().slice(0, 180);
}

export async function dbExecute(query: string, bindValues: unknown[] = []): Promise<QueryResult> {
  const db = await getDb();
  const started = performance.now();
  const info = { operation: sqlOperation(query), statement: sqlName(query), bindCount: bindValues.length };
  logEvent("debug", "database.query.start", info);
  try {
    const result = await db.execute(query, bindValues);
    logEvent("info", "database.query.end", { ...info, status: "success", durationMs: performance.now() - started, rowsAffected: result.rowsAffected, lastInsertId: result.lastInsertId });
    return result;
  } catch (error) {
    logEvent("error", "database.query.end", { ...info, status: "error", durationMs: performance.now() - started, error: String(error) });
    throw error;
  }
}

export async function dbSelect<T>(query: string, bindValues: unknown[] = []): Promise<T> {
  const db = await getDb();
  const started = performance.now();
  const info = { operation: sqlOperation(query), statement: sqlName(query), bindCount: bindValues.length };
  logEvent("debug", "database.query.start", info);
  try {
    const result = await db.select<T>(query, bindValues);
    logEvent("info", "database.query.end", {
      ...info,
      status: "success",
      durationMs: performance.now() - started,
      rowCount: Array.isArray(result) ? result.length : undefined,
    });
    return result;
  } catch (error) {
    logEvent("error", "database.query.end", { ...info, status: "error", durationMs: performance.now() - started, error: String(error) });
    throw error;
  }
}
