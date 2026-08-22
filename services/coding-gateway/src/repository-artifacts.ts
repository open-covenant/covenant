import type { Sandbox } from './types.js';

const MAX_CHANGED_FILES = 40;
const MAX_FILE_BYTES = 128_000;
const MAX_TOTAL_BYTES = 512 * 1024;

export interface RepositoryFile {
  path: string;
  content: string;
}

export async function captureRepositoryFiles(
  sandbox: Pick<Sandbox, 'readFile'>,
  paths: string[],
): Promise<RepositoryFile[]> {
  if (paths.length > MAX_CHANGED_FILES) {
    throw new Error(`repository change exceeds the ${MAX_CHANGED_FILES}-file capture limit`);
  }

  const files: RepositoryFile[] = [];
  let totalBytes = 0;
  for (const path of paths) {
    let content: string;
    try {
      content = await sandbox.readFile(path);
    } catch {
      throw new Error(`changed file is unavailable: ${path}`);
    }
    if (content.includes('\u0000')) throw new Error(`binary changed file is unsupported: ${path}`);

    const bytes = Buffer.byteLength(content, 'utf8');
    if (bytes > MAX_FILE_BYTES) {
      throw new Error(`changed file exceeds the ${MAX_FILE_BYTES}-byte capture limit: ${path}`);
    }
    totalBytes += bytes;
    if (totalBytes > MAX_TOTAL_BYTES) {
      throw new Error(`repository change exceeds the ${MAX_TOTAL_BYTES}-byte capture limit`);
    }
    files.push({ path, content });
  }
  return files;
}
