# Security

LoadGuard installs a system service, `loadguard-helper`, that runs as root and
changes firewall rules on request. Problems in it can matter, so reports are
welcome.

## Reporting a vulnerability

Please report privately through GitHub: open the repository's **Security** tab
and choose **Report a vulnerability**. Do not open a public issue for security
problems.

Include what an attacker can do, the steps to reproduce it, and the LoadGuard
version or commit.

## Supported versions

LoadGuard is young and has no stable releases yet. Fixes go into the latest
version on the `main` branch only.

## Design

What the helper trusts and what it does not:

- **Who is asking.** The helper learns the caller's user from the D-Bus daemon,
  never from the request, and only acts on that user's apps. Each user's rules
  live in a separate nftables table.
- **What it accepts.** Requests carry an app name, speeds and a duration. Never
  rules, cgroup paths or user ids: the helper finds the app's cgroups itself and
  writes the rules itself.
- **Authorisation.** Changes need the polkit action
  `io.github.keaexe.LoadGuard.manage`, which by default asks an active local user
  for their own password. Remote and inactive sessions are refused.
- **Untrusted input.** Every user controls the names of the cgroups below their
  own app slice. Names are checked against a strict list of characters before
  they reach the firewall, and scanning is capped so that a user creating many
  cgroups or processes cannot make the root service do unbounded work.
- **Confinement.** systemd runs the helper with only the network administration
  capability, a read-only system, no home directories and no network access of
  its own (see `packaging/loadguard.service`). It runs `nft` by absolute path
  with an empty environment.
- **Saved limits** are stored in `/var/lib/loadguard`, readable by root only.

Known limitations, by design:

- After a user authenticates once, the helper accepts that user's requests
  without asking again until they fully log out. Any program running as that
  user can change that user's limits during that time, but never another
  user's.
- Limits rely on apps staying in their cgroup. A program that moves itself to a
  new cgroup and leaves nothing behind escapes its limit. This affects only the
  user's own apps and is not a security boundary.
- LoadGuard limits bandwidth for convenience. It is not meant to contain
  hostile programs.
