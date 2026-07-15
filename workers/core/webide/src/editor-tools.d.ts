export type Diagnostic = {
  line: number;
  message: string;
  severity: "error" | "warning";
};
export type SearchResult = {
  path: string;
  matches: Array<{ line: number; start: number; end: number; text: string }>;
};
export function highlightCode(path: string, source: string): string;
export function languageFor(path: string): string;
export function lintDocument(
  path: string,
  source: string,
  files: Record<string, string>,
): Diagnostic[];
export function searchProjectFiles(
  files: Record<string, string>,
  query: string,
  options?: { caseSensitive?: boolean; regex?: boolean },
): { error: string | null; matchCount: number; results: SearchResult[] };
