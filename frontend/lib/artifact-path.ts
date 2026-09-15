/** Convert only a path inside this target's workspace. An unrelated absolute
 * path must reach the server unchanged and fail containment, never silently open
 * a different relative file. Canonicalization remains the envagent's job. */
export function artifactPath(
  workdir: string | undefined,
  path: string,
): string {
  if (workdir === undefined || !path.startsWith("/")) return path;
  const prefix = `${workdir.replace(/\/+$/, "")}/`;
  return path.startsWith(prefix) ? path.slice(prefix.length) : path;
}
