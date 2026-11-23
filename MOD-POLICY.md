# Mod policy

This registry lists mods, modpacks and packs for [Horizon](https://github.com/horizon-rust/horizon). This page says
who makes them, who is responsible for them, and how to have one removed. By submitting to the registry you accept it.

## Who makes the mods

- **Mods made by the Horizon project** are marked **Official**: their author is "Horizon" and their source is in a
  [horizon-rust](https://github.com/horizon-rust) repository. They are the only mods the Horizon
  project makes and is responsible for.
- **Every other mod is made, owned and maintained by its own authors**, named on its page. The Horizon project and
  its maintainers are not their authors. They don't own these mods, sell them, or license them to you; each mod's
  licence is the one its authors chose.
- **A listing is not an endorsement.** Reviewers check that an entry is complete, that its files match their pinned
  hashes, that it builds or loads against the Horizon version it names, and that nothing obviously harmful is in it.
  That review is not a legal review, a security audit or a guarantee. An owner's later versions of an entry of
  kind `resourcepack`, `shader` (`.hshader`) or `datapack` publish once the automatic checks pass, without a person
  reading them. Once the maintainers turn it on, so may a native mod's later version from an owner they vouch for,
  when its whole source passes a stricter automatic check: no `unsafe` code, no file, network or process access and
  no new build inputs. New projects, page changes, `shaderpack` zips and every other mod version are reviewed. Mods
  are provided as they are, without warranty, by their authors.
- **What reviewers block on.** A submission is listed once it passes the safety, policy and integrity checks, even
  if it is very buggy. Reviewers (human and AI) don't test that a mod's features work. They request changes, hold or
  close a submission only for a reason on this list, and for nothing else:
  1. harmful or malicious code;
  2. hidden network or data access;
  3. missing copyright or licence attribution, or a licence the Licences section below doesn't accept;
  4. Mojang data (promise 3 below);
  5. a break of the cheat rule (Cheats, below);
  6. hashes that don't match the files, or files that aren't staged;
  7. broken packaging, or a native mod without its build proof;
  8. failing automatic checks.

  Bugs, gameplay edge cases, polish and features that don't work are never a reason to hold a listing, however
  serious they look: the reviewer may leave them as a comment, and merges. Fixes can follow in a later version.
  When a small change to the submission itself fixes a problem (a field of the entry, the icon's shape, the order of
  versions, a missing link, staging a file), a maintainer or reviewer makes that change in the pull request, says in a
  comment what changed, and merges, instead of sending it back to the author. The reviewers' procedure is
  [REVIEWING.md](https://github.com/horizon-rust/horizon-mods/blob/main/REVIEWING.md).
- **Native mods** (source `.hmod` packages built locally against their exact signed Horizon SDK) run
  with the same access to your computer as the game. Install them only from authors you trust.
  The frozen native-v1 catalogue retains old `.hnmod` metadata and assets for history, not installation on the SDK runtime.
  The frozen api-0.1 catalogue does the same for shader API 0.1 packs and content mods with pre-SDK manifests.

## What authors promise when they submit

Submitting a project, through the form or a pull request, means you confirm that:

1. You made it, or you have the right to publish it, under the licence its entry names.
2. **If it is a port or a translation of another mod** (for example from Java Edition), the original's licence allows
   that, or its authors gave you permission. You keep the original's copyright and licence notices, and you credit
   its authors and link the original on your page. A port of a mod whose licence reserves all rights needs its
   authors' written permission. A rewrite of such a mod needs none when it copies nothing of the original: new code
   only, and no source, textures, models, sounds, icons or language text. Say so in the pull request.
3. It contains no code, assets or data from Minecraft, Mojang or Microsoft beyond what their
   [usage guidelines](https://www.minecraft.net/usage-guidelines) and EULA allow. Horizon players bring their own
   copy of the game. Game-data tables copied from any version count as Mojang data, whatever licence the project
   they come from uses: for example recipe tables, or biome parameter tables such as cubiomes'. Read them at runtime
   from the player's own files instead.
4. It contains nothing that harms players: no malware, no stealing of accounts or data, no hidden network access.
5. The `source` link has the code the published files were built from.

You alone are responsible for what you publish and for any claim about it. The Horizon project may refuse, hide or
remove any project, at any time, without having to give a reason.

## Cheats

A mod may have features that give its players an edge on servers (kill aura, reach, fly, auto-clickers and the like)
when both of these are true:

1. It is not presented as a cheat or a hack: not in its name, its page, or its files.
2. A server can opt into those features: they stay off on any server that has not allowed them.

Horizon has no standard way yet for a mod to ask a server for that opt-in. Until it has one, reviewers don't hold a
mod over how its opt-in is built.

The registry still refuses mods presented as cheats, and anything built to get past a server's anticheat or to hide
a mod or its features from the server (for example a "GrimAC mode" preset). A server can't opt into those.
Client-side utilities that give no edge on servers, such as HUDs, seed crackers or pathfinding bots, stay welcome.

## Licences

A project may use any licence that lets it be shared, copyleft ones (GPL, LGPL, MPL) included, as long as:

- the licence its entry names is the one in its source repository;
- a port follows promise 2 above: the original's licence allows the port, or its authors gave written permission,
  and the original is credited and linked.

A licence that reserves all rights, or that doesn't allow sharing the project, needs a written grant from its
authors that allows publishing it here and lets the Horizon reviewers read the source (say so in the pull request,
with a link to the grant).

## Copyright and takedowns

If a project in this registry uses your work without your permission:

- [Open a takedown request](../../issues/new?template=takedown.yml) (public), or
- write privately to the maintainer through [lu4ovic.com](https://lu4ovic.com/#contact).

Say which project, which of your work it uses, and how to reach you. A claim that holds gets the project removed and
listed in [`revoked.toml`](revoked.toml): the registry stops listing it and the launcher stops offering it and turns
installed copies off. Clear claims are acted on quickly. The project's author is told, and can answer the claim in the
same issue.

Authors whose project was removed by mistake can ask for it back in that issue.

## Changes

This policy can change. The version in this repository's `main` branch is the one that applies.
