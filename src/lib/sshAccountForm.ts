// Pure logic for SshAccountDialog. The Rust side (`session::validate_target`,
// `add_ssh_account`) is the enforcement; this only gives early feedback.

export interface SshForm {
  target: string;
  root: string;
  name: string;
  create: boolean;
}

export function validateTarget(t: string): string | null {
  const v = t.trim();
  if (v === '') return 'The SSH target is empty.';
  if (v.startsWith('-')) return "An SSH target cannot start with '-'.";
  if (/[\s\p{Cc}]/u.test(v)) return 'An SSH target cannot contain spaces.';
  return null;
}

export function addArgs(f: SshForm) {
  return { target: f.target.trim(), root: f.root.trim(), name: f.name.trim() || null, create: f.create };
}

export function childPath(parent: string, dir: string): string {
  return parent.endsWith('/') ? `${parent}${dir}` : `${parent}/${dir}`;
}

export function parentPath(p: string): string {
  const trimmed = p.replace(/\/+$/, '');
  const i = trimmed.lastIndexOf('/');
  return i <= 0 ? '/' : trimmed.slice(0, i);
}

// Splits the easy-setup dialog's single "Server address" field into its
// parts: user@host:port / host:port / user@host / host. Pre-fills Username
// and Port; does not validate — validateTarget still owns that.
export function parseAddress(addr: string): { host: string; port: number | null; user: string | null } {
  let rest = addr.trim();
  let user: string | null = null;
  const at = rest.indexOf('@');
  if (at >= 0) {
    user = rest.slice(0, at);
    rest = rest.slice(at + 1);
  }
  let port: number | null = null;
  const colon = rest.lastIndexOf(':');
  if (colon >= 0) {
    const maybePort = Number(rest.slice(colon + 1));
    if (Number.isInteger(maybePort) && maybePort > 0) {
      port = maybePort;
      rest = rest.slice(0, colon);
    }
  }
  return { host: rest, port, user };
}
