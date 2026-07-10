import { spinner } from "@clack/prompts";

/// Run one pipeline stage under a clack spinner: the title spins while `fn` runs, resolves to a
/// success line, and on failure stops with an error mark before rethrowing (surfaced by the caller).
export async function step<T>(title: string, fn: () => Promise<T>): Promise<T> {
  const s = spinner();
  s.start(title);
  try {
    const result = await fn();
    s.stop(title);
    return result;
  } catch (error) {
    s.error(title);
    throw error;
  }
}
