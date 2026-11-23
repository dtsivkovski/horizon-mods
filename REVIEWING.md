# Reviewing a submission

For human and AI reviewers of pull requests to this registry. The rules behind it are in
[MOD-POLICY.md](MOD-POLICY.md).

A submission is listed once it passes the safety, policy and integrity checks, even if it is very buggy. You don't
test that a mod's features work.

## Procedure

1. Read the entry, the check report and the source (a native mod's source gets a security review).
2. Fix what a small change to the submission fixes: a field of the entry, the icon's shape, the order of versions, a
   missing link, staging a file. Push the change to the pull request and say in a comment what you changed. Don't
   send it back to the author for these.
3. Check what is left against the blocking list below. If nothing on it applies, merge. Bugs, gameplay edge cases,
   polish and features that don't work may go in a comment; they never hold the merge.
4. If something on the list applies, block, and name the item and the exact file and line.

So a verdict is one of two things:

- **merge**, with optional comments;
- **blocked**: one item from the list, at the exact file and line, for example
  `blocked: hidden network or data access, src/net.rs:42`.

## The blocking list

This list is complete. Anything not on it does not block.

1. Harmful or malicious code.
2. Hidden network or data access.
3. Missing copyright or licence attribution, or a licence the policy doesn't accept.
4. Mojang data.
5. A break of the cheat rule: the mod is presented as a cheat or hack, its edge-giving features stay on where a
   server has not allowed them, or it is built to get past an anticheat or hide from the server. How a mod asks a
   server for the opt-in is not standardised yet, so don't block over how the opt-in is built.
6. Hashes that don't match the files, or files that aren't staged (and that you can't stage yourself, step 2).
7. Broken packaging, or a native mod without its build proof.
8. Failing automatic checks (that a small fix from step 2 doesn't cure).

A "changes requested" review for a bug is a reviewer error: dismiss it and merge.

The policy also says the Horizon project may refuse or remove any project. That is a maintainers' decision, made
outside review. It is never a review verdict.

## Examples

- **A world-edit mod whose undo leaves some blocks behind.** That is a bug. Leave a comment that describes it, and
  merge.
- **An icon that isn't square.** Crop or pad it yourself, push it to the pull request, comment "made the icon
  square", and merge.
- **A mod that sends the player's session or files to an address its page doesn't mention.** Blocked: hidden network
  or data access, at the file and line that make the call.
