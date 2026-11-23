#!/usr/bin/env python3
"""Decides whether a pull request is an owner's update of a resource pack, shader pack or data pack, which merges
without a reviewer, or of a native mod, which may once its packages pass tools/native-subset. Run by
.github/workflows/owner-update.yml when `check` succeeds, from main: never the pull request's copy. The pull request
is read through the API and as git objects, as data.

It is one when all of these hold:
  a. it is open into main at the head whose `check` just succeeded, and the `check` check run on that head is a
     success;
  b. it changes exactly one file from where it leaves main: a modified mods/<id>.toml (live, a regular file), and
     that entry is the same on main now;
  c. the head's entry is that entry with one or more [[versions]] records added, every other value unchanged, and
     no new record is below one with the same `published` day;
  d. its kind is resourcepack, shader or datapack, and each new file has its kind's extension: extra files in them
     are accepted but never loaded (README, "Updates"); or native-mod, see below; every other mod keeps a reviewer;
  e. GitHub's numeric id of whoever opened it is in the entry's owner_ids (never a login or the description);
  f. the id is not in revoked.toml. When the entry has a key, each new version has a signature (the check verified
     it).
A native mod's update also needs, while the repository variable NATIVE_AUTO_MERGE is `shadow` or `on` (unset or
anything else is `off`, and every native update waits for a reviewer):
  g. exactly one new record, a .hmod, whose `published` day is the newest: it lists first;
  h. the opener's id in .github/auto-merge-owners.txt on main as well, and the mod's id in neither
     .github/owner-transfers.txt nor revoked.toml;
  i. a horizon-rust/horizon SDK release (tag `sdk-<commit>`) to check the package's `horizon` pin against.
The workflow's `inspect` job then checks the whole new package against the previous release with tools/native-subset;
`merge` merges only when that passes and NATIVE_AUTO_MERGE is `on`.

Every read of main is at one commit, fetched once: `base`. Then it writes `pr`, `sha`, `kind`, `id`, `base` and the
blobs it read there (`entry`: mods/<id>.toml, `revoked`: revoked.toml) to $GITHUB_OUTPUT, so the merge can make sure
main still has them; for a native mod also `native`, `version`, both packages' URLs and SHA-256s, `pins` (each SDK
release's commit and its horizon-mixins revision) and the blobs of the two lists (`owners`, `transfers`). Anything
else: the reason, and the pull request waits for a reviewer as before. Its output holds the pull request's text, so
the runner reads no workflow command from it.

`owner_update.py merge` (the `merge` job) authorizes again at the main of that moment and merges; see `merge`.
`owner_update.py report` (the `report` job) comments on a native update that was not merged, unless its head moved on.

`owner_update.py replay` (workflow_dispatch) reads pull requests (REPLAY_PRS, else every open one; each open into main
and not a draft) and merge commits on main's first-parent line (REPLAY_COMMITS) the same way, merges nothing, and
writes the native pairs it finds as `pairs` (JSON) and `pins` for the replay's package check.

Environment: GH_TOKEN (read), GH_REPO, NATIVE_AUTO_MERGE, and the check's workflow run: HEAD_SHA, HEAD_REPO,
HEAD_BRANCH, RUN_EVENT, RUN_PATH. Needs git and python3 3.11+.
"""
import base64
import json
import os
import re
import secrets
import subprocess
import sys
import tomllib

NATIVE = "native-mod"
KINDS = {"resourcepack": "a resource pack", "shader": "a shader pack", "datapack": "a data pack",
         NATIVE: "a native mod"}
# The index build that `check` runs refuses other extensions too; checked here as well since no reviewer follows.
EXTENSIONS = {"resourcepack": ".zip", "shader": ".hshader", "datapack": ".zip", NATIVE: ".hmod"}
ENTRY = re.compile(r"mods/([a-z0-9_]{1,64})\.toml")
SHA = re.compile(r"[0-9a-f]{40}")
SHA256 = re.compile(r"[0-9a-f]{64}")
DAY = re.compile(r"[0-9]{4}-[0-9]{2}-[0-9]{2}")
# The same shapes scripts/fetch.sh accepts: they go into release URLs.
VERSION = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,63}")
FILE = re.compile(r"[A-Za-z0-9_+-][A-Za-z0-9._+-]{0,127}")
MODES = ("off", "shadow", "on")
OWNERS = ".github/auto-merge-owners.txt"
TRANSFERS = ".github/owner-transfers.txt"
ENGINE = "horizon-rust/horizon"
SDK_TAG = re.compile(r"sdk-([0-9a-f]{40})")
# The horizon-mixins revision an engine commit builds: crates/horizon-mod-package/src/mixins_tool.rs's REV.
MIXINS_TOOL = "crates/horizon-mod-package/src/mixins_tool.rs"
MIXINS_REV = re.compile(r'^pub const REV: &str = "([0-9a-f]{40})";$', re.M)
# The newest SDK releases read: a pin older than these waits for a reviewer.
MAX_SDK_RELEASES = 30


class Refused(Exception):
    pass


def same(a, b):
    """Equal values of the same types all the way down: in Python 1 == 1.0 == True."""
    if type(a) is not type(b):
        return False
    if isinstance(a, dict):
        return a.keys() == b.keys() and all(same(a[k], b[k]) for k in a)
    if isinstance(a, list):
        return len(a) == len(b) and all(same(x, y) for x, y in zip(a, b))
    return a == b


def parse(text, where):
    try:
        return tomllib.loads(text.decode("utf-8"))
    except (UnicodeDecodeError, tomllib.TOMLDecodeError) as e:
        raise Refused(f"{where} doesn't parse: {e}")


def mode(value):
    """NATIVE_AUTO_MERGE: `off` unless exactly `shadow` or `on`."""
    return value if value in MODES else "off"


def vouched(text):
    """The numeric ids in .github/auto-merge-owners.txt: one a line, `#` starts a comment. A line that is anything
    else refuses every native update, rather than guessing."""
    ids = set()
    for n, line in enumerate(text.decode("utf-8", "replace").splitlines(), 1):
        line = line.split("#", 1)[0].strip()
        if not line:
            continue
        if not re.fullmatch(r"[1-9][0-9]{0,19}", line):
            raise Refused(f"{OWNERS} line {n} is not one numeric id")
        ids.add(int(line))
    return ids


def transferred(text):
    """The mod ids in .github/owner-transfers.txt: the first word of each line that isn't a comment."""
    return {line.split()[0] for line in text.decode("utf-8", "replace").splitlines()
            if line.strip() and not line.lstrip().startswith("#")}


def newest(records):
    """The record the index lists first: the latest `published` day, the first in the file on a tie."""
    return max(enumerate(records), key=lambda p: (p[1].get("published"), -p[0]))[1]


def eligible(changes, base_text, head_text, main_text, revoked_text, author_id, owners_text=b"", transfers_text=b"",
             native="off"):
    """`changes`: (status, path) of each file the pull request changes from where it leaves main. The texts are
    bytes: the entry where it leaves main, at its head and on main now, revoked.toml on main, and for a native mod
    .github/auto-merge-owners.txt and .github/owner-transfers.txt on main. `native`: NATIVE_AUTO_MERGE. What it
    adds: (id, kind, [versions])."""
    if len(changes) != 1 or changes[0][0] != "M" or not ENTRY.fullmatch(changes[0][1]):
        listed = ", ".join(f"{s} {p}" for s, p in changes[:5])
        raise Refused(f"it changes more than one modified mods/<id>.toml: {listed}")
    path = changes[0][1]
    id_ = ENTRY.fullmatch(path)[1]
    if main_text != base_text:
        raise Refused(f"{path} changed on main since the pull request left it")
    base, head = parse(base_text, f"{path} on main"), parse(head_text, f"{path} at its head")
    if base.get("id") != id_:
        raise Refused(f"{path} on main has id {base.get('id')!r}")
    kind = base.get("kind")
    if kind not in KINDS:
        raise Refused(f"its kind is {kind!r}: only {', '.join(KINDS)} updates merge without a reviewer")
    if kind == NATIVE and native not in ("shadow", "on"):
        raise Refused(f"its kind is {kind!r} and NATIVE_AUTO_MERGE is off: native updates wait for a reviewer")
    rest = lambda e: {k: v for k, v in e.items() if k != "versions"}  # noqa: E731
    if not same(rest(head), rest(base)):
        changed = sorted(k for k in {*rest(head), *rest(base)} if not same(head.get(k), base.get(k)))
        raise Refused(f"it changes more than its versions: {', '.join(changed)}")
    old, new = base.get("versions"), head.get("versions")
    if not isinstance(old, list) or not isinstance(new, list):
        raise Refused("versions is not a list of records")
    added = [v for v in new if not any(same(v, o) for o in old)]
    kept = [v for v in new if any(same(v, o) for o in old)]
    if not added or not same(kept, old):
        raise Refused("it doesn't only add versions: every earlier record stays as it was, in its order")
    if any(not isinstance(v, dict) for v in added):
        raise Refused("a new version is not a record")
    # The index lists versions by `published` day, newest first, keeping a same-day tie in this order, and the
    # launcher takes the first as the newest: a new record below one of the same day would list that one as newest.
    days = []
    for v in new:
        day = v.get("published") if isinstance(v, dict) else None
        if isinstance(day, str) and day in days and any(v is a for a in added):
            raise Refused(f"version {v.get('version')!r} is below a version of the same day {day}: put it above that"
                          " one (two new versions of one day always wait for a reviewer)")
        days.append(day)
    for v in added:
        if not (isinstance(v.get("file"), str) and v["file"].endswith(EXTENSIONS[kind])):
            raise Refused(f"version {v.get('version')!r}'s file is not a {EXTENSIONS[kind]}")
    if kind == NATIVE:
        # One package to check against the one before it, and it must list first.
        if len(added) != 1:
            raise Refused("a native mod's update adds exactly one version")
        if not old or any(not isinstance(o, dict) for o in old):
            raise Refused("a native mod's update needs an earlier version to compare with")
        day = added[0].get("published")
        days = [o.get("published") for o in old]
        if not (isinstance(day, str) and DAY.fullmatch(day)) or any(not isinstance(d, str) for d in days):
            raise Refused("every version of a native mod needs a `published` day (YYYY-MM-DD)")
        if any(d > day for d in days):
            raise Refused(f"version {added[0].get('version')!r} is not the newest: a later day is listed")
    ids = base.get("owner_ids")
    if not isinstance(ids, list) or not ids:
        raise Refused(f"{path} has no owner_ids")
    if not (type(author_id) is int and any(type(i) is int and i == author_id for i in ids)):
        raise Refused(f"the account that opened it (id {author_id}) is not in {path}'s owner_ids")
    if kind == NATIVE:
        if author_id not in vouched(owners_text):
            raise Refused(f"the account that opened it (id {author_id}) is not in {OWNERS}")
        if id_ in transferred(transfers_text):
            raise Refused(f"{id_} is in {TRANSFERS}")
    revoked = parse(revoked_text, "revoked.toml").get("revoked", [])
    if any(isinstance(r, dict) and r.get("id") == id_ for r in revoked):
        raise Refused(f"{id_} is in revoked.toml")
    if base.get("key") and any(not isinstance(v.get("signature"), str) for v in added):
        raise Refused("the entry has a key and a new version has no signature")
    return id_, kind, [str(v.get("version")) for v in added]


def native_pair(repo, id_, base_text, head_text, staged=True):
    """The packages tools/native-subset compares: the newest earlier version, from its release `<id>-<version>`, and
    the new one, from `staging` (`staged`) or, once published, its own release. Every part of a URL is checked
    against the shape fetch.sh accepts."""
    old = parse(base_text, "the entry").get("versions") or []
    new = parse(head_text, "the entry").get("versions") or []
    added = [v for v in new if isinstance(v, dict) and not any(same(v, o) for o in old)]
    records = [o for o in old if isinstance(o, dict)]
    if len(added) != 1 or not records or not all(isinstance(o.get("published"), str) for o in records):
        raise Refused("not one new version above an earlier one")
    previous, current = newest(records), added[0]
    for record in (previous, current):
        version, file, sha = (record.get(k) for k in ("version", "file", "sha256"))
        if not all(isinstance(v, str) for v in (version, file, sha)) or not (
                VERSION.fullmatch(version) and FILE.fullmatch(file) and SHA256.fullmatch(sha)):
            raise Refused(f"version {version!r} has a version, file or sha256 no release URL can hold")
    release = f"https://github.com/{repo}/releases/download"
    new_url = (f"{release}/staging/{current['sha256']}.hmod" if staged
               else f"{release}/{id_}-{current['version']}/{current['file']}")
    return {
        "version": current["version"],
        "old_url": f"{release}/{id_}-{previous['version']}/{previous['file']}",
        "old_sha256": previous["sha256"],
        "new_url": new_url,
        "new_sha256": current["sha256"],
    }


def pick(pulls, sha, repo, branch):
    """The one open pull request whose head is `sha` on `repo`'s `branch`. A fork's workflow run names no pull
    request, so it is found among the open ones."""
    found = [p for p in pulls if (p["sha"], p["repo"], p["ref"]) == (sha, repo, branch)]
    if len(found) != 1:
        raise Refused(f"{len(found)} open pull requests into main have head {sha} on {repo} {branch}")
    return found[0]["number"]


def check_passed(runs):
    """Whether the newest `check` check run GitHub Actions made on the head succeeded."""
    ours = [r for r in runs if r.get("name") == "check" and (r.get("app") or {}).get("slug") == "github-actions"]
    if not ours:
        return False
    newest_run = max(ours, key=lambda r: r["id"])
    return newest_run.get("status") == "completed" and newest_run.get("conclusion") == "success"


def gh(*args, token=None):
    """`token`: another GH_TOKEN for this call only."""
    env = {**os.environ, "GH_TOKEN": token} if token else None
    out = subprocess.run(["gh", *args], capture_output=True, text=True, env=env)
    if out.returncode:
        raise Refused(f"gh {' '.join(args[:2])}: {out.stderr.strip()}")
    return out.stdout


def git(*args):
    return subprocess.run(["git", *args], capture_output=True, check=True).stdout


def blob(commit, path):
    """`path` at `commit`, bytes; refused unless it is a regular file."""
    tree = git("ls-tree", "-z", commit, "--", path)
    if not tree:
        raise Refused(f"{path} is not at {commit[:12]}")
    mode_, kind, _ = tree.split(b" ", 2)
    if kind != b"blob" or mode_ not in (b"100644", b"100755"):
        raise Refused(f"{path} at {commit[:12]} is not a regular file")
    return git("show", f"{commit}:{path}")


def blob_id(commit, path):
    """The id of `path`'s blob at `commit`; "" when it isn't there, "-" when it is not a regular file."""
    tree = git("ls-tree", "-z", commit, "--", path)
    if not tree:
        return ""
    mode_, kind, rest = tree.split(b" ", 2)
    if kind != b"blob" or mode_ not in (b"100644", b"100755"):
        return "-"
    return rest.split(b"\t", 1)[0].decode()


def fetch_main():
    """main's head, fetched once as refs/owner/main: every read of main that follows is at this commit."""
    git("fetch", "-q", "--no-tags", "--depth=1", "origin", "+refs/heads/main:refs/owner/main")
    main = git("rev-parse", "refs/owner/main").decode().strip()
    if not SHA.fullmatch(main):
        raise Refused("no main")
    return main


def listed(commit, path):
    """`path` at `commit` like `blob`, or nothing when it isn't there: a missing list vouches for no one."""
    return blob(commit, path) if git("ls-tree", commit, "--", path) else b""


def sdk_pins(tags, tag_commit, mixins_source):
    """Each released SDK's engine commit and the horizon-mixins revision it builds (None when it names none).
    `tags`: release tags, newest first; `tag_commit(tag)`: the commit the tag names; `mixins_source(commit)`:
    MIXINS_TOOL at that commit (None when absent). A tag whose commit isn't the one its name says counts for
    nothing."""
    pins = []
    for tag in [t for t in tags if SDK_TAG.fullmatch(t)][:MAX_SDK_RELEASES]:
        commit = SDK_TAG.fullmatch(tag)[1]
        if tag_commit(tag) != commit:
            continue
        found = MIXINS_REV.search(mixins_source(commit) or "")
        pins.append((commit, found[1] if found else None))
    return pins


def released_pins():
    """sdk_pins from horizon-rust/horizon through the API: published releases only, never a draft."""
    tags = gh("api", "--paginate", f"repos/{ENGINE}/releases?per_page=100", "--jq",
              ".[] | select(.draft | not) | .tag_name").split()

    # Every sdk- tag in one call; an annotated one costs one more.
    refs = {r["ref"].removeprefix("refs/tags/"): r["object"]
            for r in json.loads(gh("api", f"repos/{ENGINE}/git/matching-refs/tags/sdk-"))}

    def tag_commit(tag):
        ref = refs.get(tag)
        if ref and ref["type"] == "tag":
            ref = json.loads(gh("api", f"repos/{ENGINE}/git/tags/{ref['sha']}"))["object"]
        return ref["sha"] if ref and ref["type"] == "commit" else None

    def mixins_source(commit):
        try:
            content = gh("api", f"repos/{ENGINE}/contents/{MIXINS_TOOL}?ref={commit}", "--jq", ".content")
        except Refused:
            return None
        return base64.b64decode(content).decode("utf-8", "replace")

    return sdk_pins(tags, tag_commit, mixins_source)


def pins_line(pins):
    return " ".join(f"{h}={m or '-'}" for h, m in pins)


def entry_texts(base, head, main):
    """The changes from `base` to `head`, and the entry's text at base, head and main (empty unless the changes are
    one entry)."""
    diff = git("diff", "--name-status", "--no-renames", "-z", base, head).decode("utf-8").split("\0")
    changes = list(zip(diff[0::2], diff[1::2]))
    path = changes[0][1] if changes else ""
    if len(changes) == 1 and ENTRY.fullmatch(path):
        return changes, path, [blob(c, path) for c in (base, head, main)]
    return changes, path, [b""] * 3


def decide(env):
    if env["RUN_EVENT"] != "pull_request" or env["RUN_PATH"] != ".github/workflows/check.yml":
        raise Refused(f"not a pull request's check ({env['RUN_EVENT']}, {env['RUN_PATH']})")
    repo, sha = env["GH_REPO"], env["HEAD_SHA"]
    if not SHA.fullmatch(sha):
        raise Refused("no head commit")
    lines = gh("api", "--paginate", f"repos/{repo}/pulls?state=open&base=main&per_page=100", "--jq",
               ".[] | {number, sha: .head.sha, repo: .head.repo.full_name, ref: .head.ref}")
    n = pick([json.loads(line) for line in lines.splitlines() if line], sha, env["HEAD_REPO"], env["HEAD_BRANCH"])
    pr = json.loads(gh("api", f"repos/{repo}/pulls/{n}"))
    if (pr["state"], pr["base"]["ref"], pr["head"]["sha"]) != ("open", "main", sha):
        raise Refused(f"#{n} is not open into main at {sha}")
    if pr.get("draft"):
        raise Refused(f"#{n} is a draft")
    runs = json.loads(gh("api", f"repos/{repo}/commits/{sha}/check-runs?check_name=check&per_page=100"))
    if not check_passed(runs["check_runs"]):
        raise Refused(f"the required check `check` is not a success on {sha}")
    main = fetch_main()
    base = gh("api", f"repos/{repo}/compare/{main}...{sha}", "--jq", ".merge_base_commit.sha").strip()
    if not SHA.fullmatch(base):
        raise Refused("no merge base")
    git("fetch", "-q", "--no-tags", "--depth=1", "origin", base, f"+refs/pull/{n}/head:refs/owner/pr")
    if git("rev-parse", "refs/owner/pr").decode().strip() != sha:
        raise Refused(f"#{n}'s head moved on from {sha}")
    changes, path, texts = entry_texts(base, sha, main)
    lists = [listed(main, OWNERS), listed(main, TRANSFERS)]
    native = mode(env.get("NATIVE_AUTO_MERGE", ""))
    id_, kind, versions = eligible(changes, *texts, blob(main, "revoked.toml"), pr["user"]["id"], *lists, native)
    out = {"pr": n, "kind": KINDS[kind], "id": id_, "base": main}
    out.update({k: blob_id(main, p) for k, p in authorizing(id_, kind == NATIVE).items()})
    if kind == NATIVE:
        pins = released_pins()
        if not pins:
            raise Refused(f"no SDK release of {ENGINE} to check the package's pin against")
        out.update(native_pair(repo, id_, texts[0], texts[1]), native="true", pins=pins_line(pins))
        print(f"#{n}: {id_} {', '.join(versions)}, an owner's update of a native mod (opened by id "
              f"{pr['user']['id']}): the entry's rules hold; the package check decides ({native})")
    else:
        print(f"#{n}: {id_} {', '.join(versions)}, an owner's update of {KINDS[kind]} (opened by id "
              f"{pr['user']['id']})")
    return out


def authorizing(id_, native):
    """The files on main a decision rests on, by the output that holds each one's blob."""
    paths = {"entry": f"mods/{id_}.toml", "revoked": "revoked.toml"}
    if native:
        paths.update(owners=OWNERS, transfers=TRANSFERS)
    return paths


def decided(env):
    """What `decide` wrote, from the merge job's environment, each value checked for its shape."""
    d = {k: env.get(k.upper(), "") for k in ("pr", "sha", "base", "id", "native", "entry", "revoked", "owners",
                                             "transfers")}
    blob_ = re.compile(r"[0-9a-f]{40}|")
    if not (re.fullmatch(r"[1-9][0-9]{0,9}", d["pr"]) and SHA.fullmatch(d["sha"]) and SHA.fullmatch(d["base"])
            and re.fullmatch(r"[a-z0-9_]{1,64}", d["id"]) and d["native"] in ("true", "")
            and all(blob_.fullmatch(d[k]) for k in ("entry", "revoked", "owners", "transfers"))
            and SHA.fullmatch(d["entry"]) and SHA.fullmatch(d["revoked"])):
        raise Refused("the decision's outputs are malformed")
    return d


def authorize(d, main, blobs, pr, repo):
    """Whether the decision `d` still holds at `main`, one commit: `blobs` are the authorizing files' blob ids there
    (`authorizing`) and must be the ones the decision read; `pr` is the live pull request, which must still be open
    into `repo`'s main, not a draft, at the head that was checked."""
    for key, path in authorizing(d["id"], d["native"] == "true").items():
        if blobs.get(key) != d[key]:
            raise Refused(f"{path} on main ({main[:12]}) is not what the decision read at {d['base'][:12]}")
    if pr.get("state") != "open" or pr.get("merged") or pr.get("draft"):
        raise Refused(f"#{d['pr']} is no longer open, or is a draft")
    if (pr["base"]["ref"], pr["base"]["repo"]["full_name"]) != ("main", repo):
        raise Refused(f"#{d['pr']} no longer goes into main")
    if pr["head"]["sha"] != d["sha"]:
        raise Refused(f"#{d['pr']}'s head moved on from {d['sha']}")


def landed(d, main, parents, blobs, changes, pr):
    """What is wrong with a merge that happened: the squashed commit's `parents`, the authorizing `blobs` at its
    parent, the `changes` it makes to that parent and the pull request `pr` afterwards. Empty when it is the merge
    that was authorized."""
    problems = []
    if len(parents) != 1:
        problems.append(f"the merge commit has {len(parents)} parents")
    elif parents[0] != main:
        # main moved between the authorization and the merge: the decision must hold at the commit it merged onto.
        for key, path in authorizing(d["id"], d["native"] == "true").items():
            if blobs.get(key) != d[key]:
                problems.append(f"{path} changed on main between the authorization and the merge")
    if changes != [("M", f"mods/{d['id']}.toml")]:
        problems.append("the merge changes more than mods/" + d["id"] + ".toml")
    if pr.get("base", {}).get("ref") != "main" or not pr.get("merged"):
        problems.append(f"#{d['pr']} did not merge into main")
    return problems


def merge(env):
    """Merges the pull request `decide` found eligible, as the App, if its decision still holds; the merge job only
    runs this when it may (a native update: NATIVE_AUTO_MERGE `on` and its packages passed). Returns the merge
    commit and what is wrong with it (empty when nothing is); raises Refused when it doesn't merge.

    GitHub's merge API takes the head as a precondition (`sha`) but not main: it squashes onto main as it is when the
    call arrives. Branch protection doesn't require an up-to-date branch and there is no merge queue; updating main
    directly, which could name the parent, would not pass the required `check`. So main is fetched once, the decision
    is authorized at that commit (its blobs of the entry, revoked.toml and the owner lists are the decided ones, and
    the live pull request is open into main, not a draft, at the checked head), and the call follows at once. The
    window left is that call's own latency, under a second; the workflow's `owner-update-merge` group runs one merge
    at a time, so no other owner update lands in it. A maintainer's push in it would still be squashed under: so after
    the merge its parent is read, and when that is not the authorized commit the decision is checked again there,
    together with the commit's changes (only the entry) and the pull request's base. Anything wrong fails the job
    loudly: a merge can't be taken back by the workflow, and the launcher refuses a revoked id whatever the index
    says."""
    d, repo = decided(env), env["GH_REPO"]
    if d["native"] == "true" and (mode(env.get("MODE", "")) != "on" or env.get("VERDICT") != "pass"):
        raise Refused("a native update merges only when NATIVE_AUTO_MERGE is on and its packages pass")
    main = fetch_main()
    paths = authorizing(d["id"], d["native"] == "true")
    blobs = {k: blob_id(main, p) for k, p in paths.items()}
    pr = json.loads(gh("api", f"repos/{repo}/pulls/{d['pr']}"))
    authorize(d, main, blobs, pr, repo)
    result = json.loads(gh("api", "-X", "PUT", f"repos/{repo}/pulls/{d['pr']}/merge", "-f", "merge_method=squash",
                           "-f", f"sha={d['sha']}", token=env["MERGE_TOKEN"]))
    commit = str(result.get("sha", ""))
    if not (result.get("merged") and SHA.fullmatch(commit)):
        raise Refused(f"#{d['pr']} did not merge")
    try:
        git("fetch", "-q", "--no-tags", "--depth=2", "origin", commit)
        parents = git("rev-parse", f"{commit}^@").decode().split()
        blobs_then = {k: blob_id(parents[0], p) for k, p in paths.items()} if parents else {}
        diff = git("diff", "--name-status", "--no-renames", "-z", f"{commit}^1", commit).decode().split("\0")
        changes = list(zip(diff[0::2], diff[1::2]))
        after = json.loads(gh("api", f"repos/{repo}/pulls/{d['pr']}"))
    except (Refused, subprocess.CalledProcessError, ValueError) as e:
        return commit, [f"the merge couldn't be read back: {e}"]
    return commit, landed(d, main, parents, blobs_then, changes, after)


RULES = re.compile(r"R[0-9]{1,2}(,R[0-9]{1,2})*")
MARKER = "<!-- native-auto-merge -->"


def report_plan(mode_, verdict, rules, merged, sha, head, repo):
    """The comment on a native update that did not merge by itself and whether to label it `needs-review`, or None
    when there is nothing to say: it merged, the switch is off, or the pull request's head is no longer `sha` (the
    run for the new head reports). Fixed text: only the mode, the verdict, rule ids and the commit."""
    if merged == "true" or head != sha or mode_ not in ("shadow", "on"):
        return None
    rules = rules if RULES.fullmatch(rules) else ""
    if verdict not in ("pass", "refused") or (verdict == "refused" and not rules):
        verdict = "error"
    listed_rules = (f"{rules.replace(',', ', ')} ([what they mean](https://github.com/{repo}/blob/main/tools/"
                    "native-subset/README.md#rules))")
    shadow = "Shadow run, nothing merges by itself yet: "
    text = {
        ("shadow", "pass"): f"{shadow}this native mod update passed the automatic checks at {sha} and would have"
                            " merged once auto-merge is on. A reviewer merges it as before.",
        ("shadow", "refused"): f"{shadow}the automatic checks at {sha} refused rules {listed_rules}. A reviewer merges"
                               " it as before.",
        ("shadow", "error"): f"{shadow}the packages at {sha} could not be checked automatically. A reviewer merges it"
                             " as before.",
        ("on", "pass"): f"This native mod update passed the automatic checks at {sha} but was not merged by itself. It"
                        " waits for a reviewer.",
        ("on", "refused"): f"This native mod update waits for a reviewer: the automatic checks at {sha} refused rules"
                           f" {listed_rules}.",
        ("on", "error"): f"This native mod update waits for a reviewer: its packages at {sha} could not be checked"
                         " automatically.",
    }[mode_, verdict]
    return f"{MARKER}\n{text}", (mode_, verdict) != ("shadow", "pass")


def report(env):
    """Comments on a native update and labels it, as report_plan says. One comment a pull request, edited on each
    new head. The job runs one report a pull request at a time, and the head is read again right before each write,
    so a run for an older head never writes over the newer one's."""
    repo, n, sha = env["GH_REPO"], env.get("PR", ""), env.get("SHA", "")
    if not (re.fullmatch(r"[1-9][0-9]{0,9}", n) and SHA.fullmatch(sha)):
        raise Refused("the decision's outputs are malformed")

    def head():
        """The live head, or "" once the pull request is closed or merged."""
        return gh("api", f"repos/{repo}/pulls/{n}", "--jq",
                  'if .state == "open" and (.merged | not) then .head.sha else "" end').strip()

    plan = report_plan(mode(env.get("NATIVE_AUTO_MERGE", "")), env.get("VERDICT", ""), env.get("RULES", ""),
                       env.get("MERGED", ""), sha, head(), repo)
    if plan is None:
        return "nothing to report"
    body, label = plan
    ids = gh("api", "--paginate", f"repos/{repo}/issues/{n}/comments", "--jq",
             f'.[] | select(.user.login == "github-actions[bot]" and (.body | startswith("{MARKER}"))) | .id').split()
    if head() != sha:
        return "the head moved on: the run for the new head reports"
    if ids and ids[-1].isdigit():
        gh("api", "--silent", "-X", "PATCH", f"repos/{repo}/issues/comments/{ids[-1]}", "-f", f"body={body}")
    else:
        gh("api", "--silent", f"repos/{repo}/issues/{n}/comments", "-f", f"body={body}")
    if label:
        if head() != sha:
            return "commented; the head moved on before the label"
        try:
            gh("api", "--silent", f"repos/{repo}/labels/needs-review")
        except Refused:
            gh("api", "--silent", f"repos/{repo}/labels", "-f", "name=needs-review", "-f", "color=fbca04", "-f",
               "description=Waits for a reviewer")
        gh("api", "--silent", f"repos/{repo}/issues/{n}/labels", "-f", "labels[]=needs-review")
    return "commented and labelled" if label else "commented"


def replayable(pr):
    """Why the replay doesn't read a pull request, or None: like `decide`, only one open into main, not a draft."""
    if pr.get("state") != "open" or pr.get("draft"):
        return "not open, or a draft"
    if pr.get("base", {}).get("ref") != "main":
        return "not into main"
    return None


def first_parents(parents, head):
    """`head` and its first parents, newest first: main's own history, without the commits a merge brought in.
    `parents`: each commit's first parent ("" for a root), as far as it was read."""
    line, seen = [], set()
    while head and head in parents and head not in seen:
        seen.add(head)
        line.append(head)
        head = parents[head]
    return line


def replay(env):
    """Every pull request of REPLAY_PRS (else every open one into main) and merge commit of REPLAY_COMMITS, read
    as `decide` reads one, as if NATIVE_AUTO_MERGE were `shadow`: (rows, pairs), each row (label, verdict) and
    each pair a native update's packages for the replay's check."""
    repo = env["GH_REPO"]
    numbers = re.split(r"[\s,]+", env.get("REPLAY_PRS", "").strip())
    commits = re.split(r"[\s,]+", env.get("REPLAY_COMMITS", "").strip())
    numbers, commits = [n for n in numbers if n], [c for c in commits if c]
    if any(not re.fullmatch(r"[1-9][0-9]{0,6}", n) for n in numbers) or any(not SHA.fullmatch(c) for c in commits):
        raise Refused("REPLAY_PRS takes pull request numbers and REPLAY_COMMITS 40-digit commits")
    if not numbers and not commits:
        numbers = gh("api", "--paginate", f"repos/{repo}/pulls?state=open&base=main&per_page=100", "--jq",
                     ".[].number").split()
    main = fetch_main()
    lists = [listed(main, OWNERS), listed(main, TRANSFERS)]
    revoked = blob(main, "revoked.toml")
    rows, pairs = [], []
    on_main = set()
    if commits:
        # A replayed commit is read against its first parent as main then: it must be on main's first-parent line,
        # not a commit that a merge's second parent brought in.
        listing = gh("api", "--paginate", f"repos/{repo}/commits?sha={main}&per_page=100", "--jq",
                     '.[] | .sha + " " + (.parents[0].sha // "")')
        parents = dict((line.split(" ") + [""])[:2] for line in listing.splitlines() if line)
        on_main = set(first_parents(parents, main))

    def one(label, base, head, main_commit, author, staged):
        changes, path, texts = entry_texts(base, head, main_commit)
        try:
            verdict = "eligible: " + " ".join(eligible(changes, *texts, revoked, author, *lists, "shadow")[2])
        except Refused as e:
            verdict = f"refused: {e}"
        rows.append((label, verdict))
        if len(changes) == 1 and ENTRY.fullmatch(path) and parse(texts[0], path).get("kind") == NATIVE:
            id_ = ENTRY.fullmatch(path)[1]
            try:
                pair = native_pair(repo, id_, texts[0], texts[1], staged)
            except Refused:
                return
            pairs.append({"label": label, "id": id_, "entry": verdict.split(":")[0], **pair})

    for n in numbers:
        try:
            pr = json.loads(gh("api", f"repos/{repo}/pulls/{n}"))
            why = replayable(pr)
            if why:
                raise Refused(why)
            head = pr["head"]["sha"]
            base = gh("api", f"repos/{repo}/compare/{main}...{head}", "--jq", ".merge_base_commit.sha").strip()
            if not (SHA.fullmatch(base) and SHA.fullmatch(head)):
                raise Refused("no merge base")
            git("fetch", "-q", "--no-tags", "--depth=1", "origin", base, f"+refs/pull/{n}/head:refs/owner/pr")
            one(f"#{n}", base, head, main, pr["user"]["id"], True)
        except (Refused, subprocess.CalledProcessError, KeyError, ValueError) as e:
            rows.append((f"#{n}", f"refused: {e}"))
    for c in commits:
        try:
            if c not in on_main:
                raise Refused("not a commit on main's first-parent history")
            git("fetch", "-q", "--no-tags", "--depth=2", "origin", c)
            parent = git("rev-parse", f"{c}^1").decode().strip()
            pulls = json.loads(gh("api", f"repos/{repo}/commits/{c}/pulls"))
            author = pulls[0]["user"]["id"] if pulls else 0
            # Read as it was when it merged: main was its parent.
            one(c[:12], parent, c, parent, author, False)
        except (Refused, subprocess.CalledProcessError, KeyError, ValueError) as e:
            rows.append((c[:12], f"refused: {e}"))
    return rows, pairs


def shown(text):
    """Text from a pull request, safe in a Markdown table: no markup, links or control characters."""
    return re.sub(r"[^A-Za-z0-9 .,:;_()/#'=+-]", "?", str(text))[:300]


def main():
    # File names, TOML keys and versions are the pull request's: a line of them must not be a workflow command
    # (::set-output:: would set `pr`). The token is never printed, so commands stay off for the whole step.
    print(f"::stop-commands::{secrets.token_hex(16)}", flush=True)
    summary = os.environ.get("GITHUB_STEP_SUMMARY", os.devnull)
    output = os.environ.get("GITHUB_OUTPUT", os.devnull)
    if sys.argv[1:] == ["merge"]:
        try:
            commit, problems = merge(os.environ)
        except (Refused, subprocess.CalledProcessError) as e:
            message = f"#{os.environ.get('PR', '?')} waits for a reviewer: {e}"
            print(message)
            with open(summary, "a") as f:
                f.write(message + "\n")
            return 0
        with open(output, "a") as f:
            f.write("merged=true\n")
        with open(summary, "a") as f:
            f.write(f"merged #{os.environ['PR']} at {os.environ['SHA']} as {commit}\n")
            f.writelines(f"**{p}**: a maintainer must look at {commit}\n" for p in problems)
        print(f"merged as {commit}", *problems, sep="\n")
        return 1 if problems else 0
    if sys.argv[1:] == ["report"]:
        try:
            print(report(os.environ))
        except Refused as e:
            print(f"report: {e}")
            return 1
        return 0
    if sys.argv[1:] == ["replay"]:
        try:
            rows, pairs = replay(os.environ)
            pins = released_pins()
        except Refused as e:
            print(f"replay: {e}")
            return 1
        with open(summary, "a") as f:
            f.write("## Entries\n\n| Pull request or commit | Entry rules |\n| --- | --- |\n")
            f.writelines(f"| {shown(label)} | {shown(verdict)} |\n" for label, verdict in rows)
        with open(output, "a") as f:
            f.write(f"pairs={json.dumps(pairs, separators=(',', ':'))}\npins={pins_line(pins)}\n")
        print(f"{len(rows)} read, {len(pairs)} native pairs to check")
        return 0
    try:
        out = decide(os.environ)
    except Refused as e:
        message = f"Not an owner update that merges by itself: {e}. It waits for a reviewer."
        print(message)
        with open(summary, "a") as f:
            f.write(message + "\n")
        return 0
    with open(output, "a") as f:
        f.write(f"sha={os.environ['HEAD_SHA']}\n")
        f.writelines(f"{k}={v}\n" for k, v in out.items())
    with open(summary, "a") as f:
        what = "the package check decides" if out.get("native") else "every rule holds"
        f.write(f"#{out['pr']}: an owner's update of {out['kind']}; {what}.\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
